//! Background closure loading: a small worker pool pulling store paths off a shared queue,
//! so the UI never blocks on `nix path-info`.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

use crate::closure::{Cache, Closure};
use crate::sources::Generation;

/// Messages from background threads to the UI loop.
pub enum Msg {
    Closure {
        store_path: String,
        result: Result<Arc<Closure>, String>,
    },
    /// Embedded home-manager generations, computed once system closures are loaded.
    HomeGenerations(Result<Vec<Generation>, String>),
}

#[derive(Default)]
struct Queue {
    items: Mutex<VecDeque<String>>,
    ready: Condvar,
}

pub struct Loader {
    queue: Arc<Queue>,
}

impl Loader {
    pub fn new(workers: usize, cache: Option<Cache>, tx: Sender<Msg>) -> Self {
        let queue = Arc::new(Queue::default());
        let cache = Arc::new(cache);
        for _ in 0..workers {
            let (queue, cache, tx) = (queue.clone(), cache.clone(), tx.clone());
            thread::spawn(move || loop {
                let store_path = {
                    let mut items = queue.items.lock().expect("loader queue poisoned");
                    loop {
                        match items.pop_front() {
                            Some(item) => break item,
                            None => items = queue.ready.wait(items).expect("loader queue poisoned"),
                        }
                    }
                };
                let result = Closure::load(Path::new(&store_path), cache.as_ref().as_ref())
                    .map(Arc::new)
                    .map_err(|e| format!("{e:#}"));
                if tx.send(Msg::Closure { store_path, result }).is_err() {
                    return; // The UI has exited.
                }
            });
        }
        Self { queue }
    }

    /// Queues a load at the back. Callers track what they've requested; each call loads once.
    pub fn request(&self, store_path: &str) {
        let mut items = self.queue.items.lock().expect("loader queue poisoned");
        items.push_back(store_path.to_owned());
        self.queue.ready.notify_one();
    }

    /// Moves a still-queued load to the front, e.g. because it's now on screen.
    pub fn prioritize(&self, store_path: &str) {
        let mut items = self.queue.items.lock().expect("loader queue poisoned");
        if let Some(pos) = items.iter().position(|p| p == store_path) {
            if let Some(item) = items.remove(pos) {
                items.push_front(item);
            }
        }
    }
}

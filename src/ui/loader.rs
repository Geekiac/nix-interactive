//! Background closure loading: a small worker pool pulling store paths off a shared queue,
//! so the UI never blocks on `nix path-info`.

use std::collections::VecDeque;
use std::path::Path;
use std::process::Command;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

use crate::closure::{Cache, Closure};
use crate::git::Repo;
use crate::sources::Generation;

/// Messages from background threads to the UI loop.
pub enum Msg {
    Closure {
        store_path: String,
        result: Result<Arc<Closure>, String>,
    },
    /// Embedded home-manager generations, computed once system closures are loaded.
    HomeGenerations(Result<Vec<Generation>, String>),
    /// The configuration repo's history.
    Repo(Result<Arc<Repo>, String>),
    /// A finished external command: its stdout, or why it failed.
    Job {
        key: JobKey,
        result: Result<String, String>,
    },
}

/// External commands the UI runs, keyed by the store paths they're about.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum JobKey {
    Nvd {
        old: String,
        new: String,
    },
    WhyDepends {
        root: String,
        path: String,
    },
    /// `git log` between two commits.
    GitLog {
        from: String,
        to: String,
    },
}

pub enum JobState {
    Running,
    Done(String),
    Failed(String),
}

#[derive(Default)]
struct Queue {
    items: Mutex<VecDeque<String>>,
    ready: Condvar,
}

pub struct Loader {
    queue: Arc<Queue>,
    tx: Sender<Msg>,
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
        Self { queue, tx }
    }

    /// Runs `program args` on its own thread and reports back as [`Msg::Job`].
    pub fn spawn_job(&self, key: JobKey, program: &str, args: Vec<String>) {
        let (tx, program) = (self.tx.clone(), program.to_owned());
        thread::spawn(move || {
            let result = match Command::new(&program).args(&args).output() {
                Err(e) => Err(format!("couldn't run {program}: {e}")),
                Ok(out) if out.status.success() => {
                    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
                }
                Ok(out) => {
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    Err(format!(
                        "{program} failed ({}): {}",
                        out.status,
                        stderr.trim()
                    ))
                }
            };
            let _ = tx.send(Msg::Job { key, result });
        });
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

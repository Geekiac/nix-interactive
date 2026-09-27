//! The interactive terminal UI (`nixi` with no subcommand).

mod ansi;
mod app;
mod detail;
mod loader;
mod view;

use std::fs;
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use anyhow::Result;
use ratatui::crossterm::event::{self, Event, KeyEventKind};

use self::app::{App, Gens, Tab, TabKind};
use self::loader::{Loader, Msg};
use crate::closure::{resolve_store_path, Cache};
use crate::sources::{home, profile, Generation};

const WORKERS: usize = 4;

pub struct Options {
    pub profile: PathBuf,
    pub user: String,
    /// Start on the home-manager tab.
    pub home_first: bool,
    /// Extra closures (e.g. `./result`) shown in their own tab.
    pub paths: Vec<PathBuf>,
    pub cache: Option<Cache>,
}

fn path_generations(paths: &[PathBuf]) -> Result<Vec<Generation>> {
    paths
        .iter()
        .enumerate()
        .map(|(i, path)| {
            Ok(Generation {
                number: i as u64 + 1,
                last_number: i as u64 + 1,
                store_path: resolve_store_path(path)?,
                created: fs::symlink_metadata(path).and_then(|m| m.modified()).ok(),
                current: false,
                path: path.clone(),
            })
        })
        .collect()
}

pub fn run(opts: Options) -> Result<()> {
    let (tx, rx) = mpsc::channel();
    let profile_name = opts
        .profile
        .file_name()
        .map_or("profile".into(), |n| n.to_string_lossy().into_owned());

    let mut tabs = Vec::new();
    match profile::generations(&opts.profile) {
        Ok(gens) => {
            // Home-manager generations need every system closure; work them out off-thread.
            let (system_gens, user, cache, tx) = (
                gens.clone(),
                opts.user.clone(),
                opts.cache.clone(),
                tx.clone(),
            );
            thread::spawn(move || {
                let result = home::generations(&system_gens, &user, cache.as_ref())
                    .map_err(|e| format!("{e:#}"));
                let _ = tx.send(Msg::HomeGenerations(result));
            });
            tabs.push(Tab::new(profile_name, TabKind::Profile, Gens::Ready(gens)));
            tabs.push(Tab::new(
                format!("home ({})", opts.user),
                TabKind::Home,
                Gens::Loading,
            ));
        }
        Err(e) => tabs.push(Tab::new(
            profile_name,
            TabKind::Profile,
            Gens::Failed(format!("{e:#}")),
        )),
    }
    if !opts.paths.is_empty() {
        let gens = match path_generations(&opts.paths) {
            Ok(gens) => Gens::Ready(gens),
            Err(e) => Gens::Failed(format!("{e:#}")),
        };
        tabs.push(Tab::new("paths", TabKind::Paths, gens));
    }
    let home_tab = tabs.iter().position(|t| t.kind == TabKind::Home);
    let active = home_tab.filter(|_| opts.home_first).unwrap_or(0);

    let loader = Loader::new(WORKERS, opts.cache, tx);
    let mut app = App::new(tabs, active, loader);

    let mut terminal = ratatui::init();
    let result = (|| -> Result<()> {
        while !app.quit {
            terminal.draw(|f| view::draw(f, &mut app))?;
            if event::poll(Duration::from_millis(100))? {
                if let Event::Key(key) = event::read()? {
                    if key.kind == KeyEventKind::Press {
                        app.on_key(key);
                    }
                }
            }
            while let Ok(msg) = rx.try_recv() {
                app.on_msg(msg);
            }
        }
        Ok(())
    })();
    ratatui::restore();
    result
}

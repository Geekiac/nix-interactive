//! The interactive terminal UI (`nixi` with no subcommand).

mod ansi;
mod app;
mod detail;
mod loader;
mod view;

use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::Duration;

use anyhow::Result;
use ratatui::crossterm::event::{self, Event, KeyEventKind};

use self::app::{App, Gens, PendingDelete, RepoState, Tab, TabKind};
use self::loader::{Loader, Msg};
use crate::closure::{resolve_store_path, Cache};
use crate::config::NamedProfile;
use crate::delete;
use crate::git::Repo;
use crate::sources::{home, profile, Generation};

const WORKERS: usize = 4;

pub struct Options {
    pub profile: PathBuf,
    pub user: String,
    /// Start on the home-manager tab.
    pub home_first: bool,
    /// Extra closures (e.g. `./result`) shown in their own tab.
    pub paths: Vec<PathBuf>,
    /// More profiles from the config file, each in its own tab.
    pub extra_profiles: Vec<NamedProfile>,
    pub cache: Option<Cache>,
    /// Configuration repo to link generations to commits.
    pub repo: Option<PathBuf>,
    /// `OLD:NEW` range to open on.
    pub range: Option<String>,
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

/// Works out embedded home-manager generations off-thread; they need every system closure.
fn spawn_home(system_gens: Vec<Generation>, opts: &Options, tx: &mpsc::Sender<Msg>) {
    let (user, cache, tx) = (opts.user.clone(), opts.cache.clone(), tx.clone());
    thread::spawn(move || {
        let result =
            home::generations(&system_gens, &user, cache.as_ref()).map_err(|e| format!("{e:#}"));
        let _ = tx.send(Msg::HomeGenerations(result));
    });
}

/// Runs a confirmed deletion on the plain terminal (the TUI is suspended), so sudo can ask
/// for a password and nix-env's output stays visible.
fn run_delete(del: &PendingDelete) -> Result<(), String> {
    println!(
        "Deleting generation {} of {}{}",
        del.number,
        del.profile.display(),
        if del.sudo {
            " (sudo may ask for your password)"
        } else {
            ""
        }
    );
    let status = delete::command(&del.profile, &[del.number], del.sudo).status();
    let result = match status {
        Ok(status) if status.success() => return Ok(()),
        Ok(status) => format!("nix-env exited with {status}"),
        Err(e) => format!("couldn't run nix-env: {e}"),
    };
    print!("{result}. Press enter to return to nixi.");
    let _ = io::stdout().flush();
    let _ = io::stdin().read_line(&mut String::new());
    Err(result)
}

pub fn run(opts: Options) -> Result<()> {
    let (tx, rx) = mpsc::channel();
    let profile_name = opts
        .profile
        .file_name()
        .map_or("profile".into(), |n| n.to_string_lossy().into_owned());

    let mut repo_state = RepoState::Unconfigured;
    if let Some(repo) = opts.repo.clone() {
        repo_state = RepoState::Loading;
        let tx = tx.clone();
        thread::spawn(move || {
            let result = Repo::load(&repo)
                .map(Arc::new)
                .map_err(|e| format!("{e:#}"));
            let _ = tx.send(Msg::Repo(result));
        });
    }

    let mut tabs = Vec::new();
    match profile::generations(&opts.profile) {
        Ok(gens) => {
            spawn_home(gens.clone(), &opts, &tx);
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
    for extra in &opts.extra_profiles {
        let gens = match profile::generations(&extra.path) {
            Ok(gens) => Gens::Ready(gens),
            Err(e) => Gens::Failed(format!("{e:#}")),
        };
        tabs.push(Tab::new(extra.name.clone(), TabKind::Profile, gens));
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

    let loader = Loader::new(WORKERS, opts.cache.clone(), tx.clone());
    let mut app = App::new(tabs, active, loader);
    app.repo = repo_state;
    if let Some(range) = opts.range.clone() {
        app.start_with_range(range);
    }

    let mut terminal = ratatui::init();
    let result = (|| -> Result<()> {
        while !app.quit {
            terminal.draw(|f| view::draw(f, &mut app))?;
            if event::poll(Duration::from_millis(100))? {
                if let Event::Key(key) = event::read()? {
                    if key.kind == KeyEventKind::Press {
                        app.on_key(key);
                    }
                    if let Some(del) = app.take_pending_delete() {
                        ratatui::restore();
                        let result = run_delete(&del).and_then(|()| {
                            profile::generations(&del.profile).map_err(|e| format!("{e:#}"))
                        });
                        terminal = ratatui::init();
                        // The home tab is derived from the first (system) profile tab.
                        let home = app.tabs.iter().position(|t| t.kind == TabKind::Home);
                        if let (Ok(gens), 0, Some(home)) = (&result, del.tab, home) {
                            app.tabs[home].gens = Gens::Loading;
                            spawn_home(gens.clone(), &opts, &tx);
                        }
                        app.deleted(del.tab, del.number, result);
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

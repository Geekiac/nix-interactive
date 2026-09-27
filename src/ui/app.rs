//! TUI state and key handling, independent of drawing.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::widgets::{ListState, TableState};

use super::detail::Detail;
use super::loader::{JobKey, JobState, Loader, Msg};
use crate::closure::Closure;
use crate::delete;
use crate::diff::{self, ChangeKind, ClosureDiff, PackageEntry, Selection};
use crate::git::{Link, Repo};
use crate::range::parse_range;
use crate::sources::Generation;
use crate::store_path::Version;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabKind {
    Profile,
    Home,
    Paths,
}

pub enum Gens {
    Loading,
    Ready(Vec<Generation>),
    Failed(String),
}

pub struct Tab {
    pub title: String,
    pub kind: TabKind,
    pub gens: Gens,
    pub cursor: usize,
    /// Pinned old side; otherwise the generation before the new side.
    pub base: Option<usize>,
    /// Pinned new side; otherwise the cursor.
    pub target: Option<usize>,
    pub list_state: ListState,
}

impl Tab {
    pub fn new(title: impl Into<String>, kind: TabKind, gens: Gens) -> Self {
        let mut tab = Self {
            title: title.into(),
            kind,
            gens: Gens::Loading,
            cursor: 0,
            base: None,
            target: None,
            list_state: ListState::default(),
        };
        tab.set_gens(gens);
        tab
    }

    /// Replaces the generations, starting the cursor on the current (else newest) one.
    pub fn set_gens(&mut self, gens: Gens) {
        if let Gens::Ready(list) = &gens {
            self.cursor = list
                .iter()
                .position(|g| g.current)
                .unwrap_or(list.len().saturating_sub(1));
        }
        self.gens = gens;
    }

    pub fn generations(&self) -> &[Generation] {
        match &self.gens {
            Gens::Ready(gens) => gens,
            _ => &[],
        }
    }

    /// `(old, new)` indices to diff: pinned sides win; otherwise new is the cursor and old
    /// the generation before it ("what did this switch change").
    pub fn pair(&self) -> Option<(usize, usize)> {
        if self.generations().is_empty() {
            return None;
        }
        let new = self.target.unwrap_or(self.cursor);
        let old = match self.base {
            Some(base) => base,
            None => new.checked_sub(1)?,
        };
        Some((old, new))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Category {
    Upgraded,
    Downgraded,
    Changed,
    Selection,
    Added,
    Removed,
    Rebuilt,
}

impl Category {
    /// Status letter, as in nvd (`B` for rebuilt is ours).
    pub fn marker(self) -> char {
        match self {
            Self::Upgraded => 'U',
            Self::Downgraded => 'D',
            Self::Changed | Self::Selection => 'C',
            Self::Added => 'A',
            Self::Removed => 'R',
            Self::Rebuilt => 'B',
        }
    }

    /// Key that shows/hides this category. Selection changes share `c` with version changes.
    pub fn toggle_key(self) -> char {
        match self {
            Self::Upgraded => 'u',
            Self::Downgraded => 'd',
            Self::Changed | Self::Selection => 'c',
            Self::Added => 'a',
            Self::Removed => 'r',
            Self::Rebuilt => 'b',
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Upgraded => "upgraded",
            Self::Downgraded => "downgraded",
            Self::Changed => "changed",
            Self::Selection => "selection",
            Self::Added => "added",
            Self::Removed => "removed",
            Self::Rebuilt => "rebuilt",
        }
    }

    pub const ALL: [Self; 7] = [
        Self::Upgraded,
        Self::Downgraded,
        Self::Changed,
        Self::Selection,
        Self::Added,
        Self::Removed,
        Self::Rebuilt,
    ];
}

/// One package line in the diff pane.
#[derive(Debug, Clone)]
pub struct DiffRow {
    pub category: Category,
    pub pname: String,
    /// Empty for added packages.
    pub left: Vec<Version>,
    /// Empty for removed packages.
    pub right: Vec<Version>,
    pub selection: Selection,
    pub size_delta: i64,
}

fn rows(d: &ClosureDiff) -> Vec<DiffRow> {
    let entry = |category, e: &PackageEntry, left: bool, right: bool| DiffRow {
        category,
        pname: e.pname.clone(),
        left: if left { e.versions.clone() } else { Vec::new() },
        right: if right {
            e.versions.clone()
        } else {
            Vec::new()
        },
        selection: e.selection,
        size_delta: e.size_delta,
    };
    let mut rows: Vec<DiffRow> = d
        .version_changes
        .iter()
        .map(|c| DiffRow {
            category: match c.kind {
                ChangeKind::Upgraded => Category::Upgraded,
                ChangeKind::Downgraded => Category::Downgraded,
                ChangeKind::Changed => Category::Changed,
            },
            pname: c.pname.clone(),
            left: c.left.clone(),
            right: c.right.clone(),
            selection: c.selection,
            size_delta: c.size_delta,
        })
        .collect();
    rows.extend(
        d.selection_changes
            .iter()
            .map(|e| entry(Category::Selection, e, true, true)),
    );
    rows.extend(
        d.added
            .iter()
            .map(|e| entry(Category::Added, e, false, true)),
    );
    rows.extend(
        d.removed
            .iter()
            .map(|e| entry(Category::Removed, e, true, false)),
    );
    rows.extend(
        d.rebuilt
            .iter()
            .map(|e| entry(Category::Rebuilt, e, true, true)),
    );
    rows.sort_by_key(|r| Category::ALL.iter().position(|c| *c == r.category));
    rows
}

/// The diff for the pair on screen, keyed by `(old, new)` store paths.
pub struct CurrentDiff {
    pub key: (String, String),
    pub diff: ClosureDiff,
    pub rows: Vec<DiffRow>,
}

impl CurrentDiff {
    pub fn count(&self, category: Category) -> usize {
        self.rows.iter().filter(|r| r.category == category).count()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    List,
    Diff,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sort {
    /// Grouped by category, then by name (nvd's order).
    Name,
    /// Largest size change first.
    Size,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffMode {
    /// Our package table.
    Packages,
    /// `nvd diff`'s own output.
    Nvd,
    /// Configuration commits between the two generations.
    Commits,
}

/// Lines the commits view shows above the `git log` output (at most).
pub const COMMITS_HEADER_LINES: usize = 5;

pub enum RepoState {
    /// No `--repo` given.
    Unconfigured,
    Loading,
    Ready {
        repo: Arc<Repo>,
        /// By profile tab index, then generation number. Home-manager generations are
        /// numbered by system generation, so they use the first tab's links.
        links: HashMap<usize, HashMap<u64, Link>>,
    },
    Failed(String),
}

/// The `D` confirmation popup: deletion only proceeds once the generation number is typed.
pub struct DeleteConfirm {
    pub tab: usize,
    pub generation: Generation,
    pub profile: PathBuf,
    /// The exact command that will run.
    pub command: String,
    pub warnings: Vec<String>,
    pub input: String,
}

/// A confirmed deletion for the event loop to carry out with the terminal released (sudo
/// may need to prompt for a password).
pub struct PendingDelete {
    pub tab: usize,
    pub profile: PathBuf,
    pub number: u64,
    pub sudo: bool,
}

/// A confirmed action that needs the real terminal (sudo prompts, nix's progress output),
/// carried out by the event loop with the TUI suspended.
pub enum Pending {
    Delete(PendingDelete),
    /// Garbage collection (`C`).
    Gc,
}

pub struct App {
    pub tabs: Vec<Tab>,
    pub active: usize,
    pub focus: Focus,
    pub closures: HashMap<String, Arc<Closure>>,
    pub errors: HashMap<String, String>,
    requested: HashSet<String>,
    pub current: Option<CurrentDiff>,
    pub filter: String,
    pub editing_filter: bool,
    /// Text of the `:` range prompt while it's open.
    pub range_input: Option<String>,
    /// A one-off notice for the status bar (e.g. a bad range), cleared by the next key.
    pub message: Option<String>,
    /// A range to apply once the active tab's generations have loaded.
    pending_range: Option<String>,
    pub delete_confirm: Option<DeleteConfirm>,
    /// The `C` confirmation popup is open.
    pub gc_confirm: bool,
    pending: Option<Pending>,
    /// Toggle keys of hidden categories.
    pub hidden: HashSet<char>,
    pub sort: Sort,
    pub diff_state: TableState,
    pub show_help: bool,
    pub quit: bool,
    pub diff_mode: DiffMode,
    /// Scroll position of the text views (nvd output, commits).
    pub text_scroll: usize,
    pub repo: RepoState,
    /// External command runs (nvd, why-depends); kept so revisiting is instant.
    pub jobs: HashMap<JobKey, JobState>,
    pub detail: Option<Detail>,
    /// The `(old, new)` store paths last on screen, to reset scrolling when they change.
    last_pair: Option<(String, String)>,
    /// Rows visible in each pane at the last draw, for page up/down.
    pub list_height: usize,
    pub diff_height: usize,
    loader: Loader,
}

impl App {
    pub fn new(tabs: Vec<Tab>, active: usize, loader: Loader) -> Self {
        let mut app = Self {
            tabs,
            active,
            focus: Focus::List,
            closures: HashMap::new(),
            errors: HashMap::new(),
            requested: HashSet::new(),
            current: None,
            filter: String::new(),
            editing_filter: false,
            range_input: None,
            message: None,
            pending_range: None,
            delete_confirm: None,
            gc_confirm: false,
            pending: None,
            hidden: HashSet::from(['b']),
            sort: Sort::Name,
            diff_state: TableState::default(),
            show_help: false,
            quit: false,
            diff_mode: DiffMode::Packages,
            text_scroll: 0,
            repo: RepoState::Unconfigured,
            jobs: HashMap::new(),
            detail: None,
            last_pair: None,
            list_height: 10,
            diff_height: 10,
            loader,
        };
        app.sync();
        for i in 0..app.tabs.len() {
            app.prefetch(i);
        }
        app
    }

    pub fn tab(&self) -> &Tab {
        &self.tabs[self.active]
    }

    fn tab_mut(&mut self) -> &mut Tab {
        &mut self.tabs[self.active]
    }

    fn request(&mut self, store_path: &str, urgent: bool) {
        if self.closures.contains_key(store_path) || self.errors.contains_key(store_path) {
            return;
        }
        if self.requested.insert(store_path.to_owned()) {
            self.loader.request(store_path);
        }
        if urgent {
            self.loader.prioritize(store_path);
        }
    }

    /// Queues every generation of a tab, newest first, so browsing is instant later.
    fn prefetch(&mut self, tab: usize) {
        let paths: Vec<String> = self.tabs[tab]
            .generations()
            .iter()
            .rev()
            .map(|g| g.store_path.clone())
            .collect();
        for path in paths {
            self.request(&path, false);
        }
    }

    /// Store paths of the pair on screen, if there is one.
    pub fn pair_paths(&self) -> Option<(&Generation, &Generation)> {
        let tab = self.tab();
        let (old, new) = tab.pair()?;
        let gens = tab.generations();
        Some((gens.get(old)?, gens.get(new)?))
    }

    fn run_job(&mut self, key: JobKey, program: &str, args: Vec<String>) {
        if !self.jobs.contains_key(&key) {
            self.jobs.insert(key.clone(), JobState::Running);
            self.loader.spawn_job(key, program, args);
        }
    }

    /// Loads what the screen needs and recomputes the diff when the pair changed.
    pub fn sync(&mut self) {
        let Some((old, new, old_path, new_path)) = self.pair_paths().map(|(o, n)| {
            (
                o.store_path.clone(),
                n.store_path.clone(),
                o.path.display().to_string(),
                n.path.display().to_string(),
            )
        }) else {
            return;
        };
        self.request(&new, true);
        self.request(&old, true);
        if self.last_pair.as_ref() != Some(&(old.clone(), new.clone())) {
            self.last_pair = Some((old.clone(), new.clone()));
            self.text_scroll = 0;
        }
        if old != new && self.diff_mode == DiffMode::Nvd {
            let key = JobKey::Nvd {
                old: old.clone(),
                new: new.clone(),
            };
            let args = ["--color", "always", "diff", &old_path, &new_path];
            self.run_job(key, "nvd", args.map(str::to_owned).to_vec());
        }
        if self.diff_mode == DiffMode::Commits {
            if let Some((key, args)) = self.commits_job_spec() {
                self.run_job(key, "git", args);
            }
        }
        if old == new
            || self
                .current
                .as_ref()
                .is_some_and(|c| c.key == (old.clone(), new.clone()))
        {
            return;
        }
        if let (Some(left), Some(right)) = (self.closures.get(&old), self.closures.get(&new)) {
            let d = diff::diff(left, right);
            let rows = rows(&d);
            self.current = Some(CurrentDiff {
                key: (old, new),
                diff: d,
                rows,
            });
            self.select_first_row();
        }
    }

    /// The `nvd diff` run for the pair on screen, if started.
    pub fn nvd_job(&self) -> Option<&JobState> {
        let (old, new) = self.pair_paths()?;
        self.jobs.get(&JobKey::Nvd {
            old: old.store_path.clone(),
            new: new.store_path.clone(),
        })
    }

    /// Commit link for a generation of the active tab (none for ad-hoc paths).
    pub fn link(&self, g: &Generation) -> Option<&Link> {
        let RepoState::Ready { links, .. } = &self.repo else {
            return None;
        };
        let tab = match self.tab().kind {
            TabKind::Paths => return None,
            TabKind::Home => 0,
            TabKind::Profile => self.active,
        };
        links.get(&tab)?.get(&g.number)
    }

    pub fn pair_links(&self) -> Option<(&Link, &Link)> {
        let (old, new) = self.pair_paths()?;
        Some((self.link(old)?, self.link(new)?))
    }

    /// The `git log` job for the pair on screen, when their commits differ.
    fn commits_job_spec(&self) -> Option<(JobKey, Vec<String>)> {
        let RepoState::Ready { repo, .. } = &self.repo else {
            return None;
        };
        let (old, new) = self.pair_links()?;
        if old.commit.hash == new.commit.hash {
            return None;
        }
        let (args, _) = repo.log_args(&old.commit, &new.commit, true);
        let key = JobKey::GitLog {
            from: old.commit.hash.clone(),
            to: new.commit.hash.clone(),
        };
        Some((key, args))
    }

    pub fn commits_job(&self) -> Option<&JobState> {
        self.jobs.get(&self.commits_job_spec()?.0)
    }

    /// Lines in the current text view, for scrolling.
    fn text_line_count(&self) -> usize {
        let (job, header) = match self.diff_mode {
            DiffMode::Nvd => (self.nvd_job(), 0),
            DiffMode::Commits => (self.commits_job(), COMMITS_HEADER_LINES),
            DiffMode::Packages => (None, 0),
        };
        match job {
            Some(JobState::Done(out)) => out.lines().count() + header,
            _ => header,
        }
    }

    /// The `nix why-depends` run shown in the detail popup, if any.
    pub fn why_job(&self) -> Option<&JobState> {
        let (root, path) = self.detail.as_ref()?.why.clone()?;
        self.jobs.get(&JobKey::WhyDepends { root, path })
    }

    fn open_detail(&mut self) {
        let Some(row) = self
            .diff_state
            .selected()
            .and_then(|i| self.visible_rows().get(i).map(|r| (*r).clone()))
        else {
            return;
        };
        let Some((old, new)) = self.pair_paths() else {
            return;
        };
        let (Some(left), Some(right)) = (
            self.closures.get(&old.store_path),
            self.closures.get(&new.store_path),
        ) else {
            return;
        };
        let title = &self.tab().title;
        let detail = Detail::new(&row, (old, new), (left, right), |g| {
            format!("{title} {}", g.number_label())
        });
        self.detail = Some(detail);
    }

    /// Runs `nix why-depends` from the selected path's generation to that path.
    fn why_depends(&mut self) {
        let Some(detail) = &mut self.detail else {
            return;
        };
        let Some(selected) = detail.selected() else {
            return;
        };
        let (root, path) = (detail.root(selected.side).to_owned(), selected.path.clone());
        detail.why = Some((root.clone(), path.clone()));
        detail.jump_to_why = true;
        let args = [
            "--extra-experimental-features",
            "nix-command",
            "why-depends",
            &root,
            &path,
        ];
        let args = args.map(str::to_owned).to_vec();
        self.run_job(JobKey::WhyDepends { root, path }, "nix", args);
    }

    fn on_detail_key(&mut self, code: KeyCode) {
        let Some(detail) = &mut self.detail else {
            return;
        };
        match code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter => self.detail = None,
            KeyCode::Up | KeyCode::Char('k') => detail.cursor = detail.cursor.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                detail.cursor = (detail.cursor + 1).min(detail.paths.len().saturating_sub(1));
            }
            KeyCode::PageUp => detail.scroll = detail.scroll.saturating_sub(10),
            KeyCode::PageDown => detail.scroll = detail.scroll.saturating_add(10),
            KeyCode::Char('w') => self.why_depends(),
            _ => {}
        }
    }

    /// The diff for the pair on screen, if it's computed.
    pub fn shown_diff(&self) -> Option<&CurrentDiff> {
        let (old, new) = self.pair_paths()?;
        self.current
            .as_ref()
            .filter(|c| c.key.0 == old.store_path && c.key.1 == new.store_path)
    }

    /// Rows after category toggles, the filter, and sorting.
    pub fn visible_rows(&self) -> Vec<&DiffRow> {
        let Some(current) = self.shown_diff() else {
            return Vec::new();
        };
        let filter = self.filter.to_lowercase();
        let mut rows: Vec<&DiffRow> = current
            .rows
            .iter()
            .filter(|r| !self.hidden.contains(&r.category.toggle_key()))
            .filter(|r| filter.is_empty() || r.pname.to_lowercase().contains(&filter))
            .collect();
        if self.sort == Sort::Size {
            rows.sort_by_key(|r| std::cmp::Reverse(r.size_delta.unsigned_abs()));
        }
        rows
    }

    fn select_first_row(&mut self) {
        let any = !self.visible_rows().is_empty();
        self.diff_state = TableState::default().with_selected(any.then_some(0));
    }

    /// Pins both sides of the active tab to an `OLD:NEW` range (`-1:0`, `40:43`, …).
    pub fn apply_range(&mut self, text: &str) -> Result<(), String> {
        let (old, new) = parse_range(text).map_err(|e| e.to_string())?;
        let gens = self.tab().generations();
        if gens.is_empty() {
            return Err("no generations loaded in this tab".into());
        }
        let not_here = |e: anyhow::Error| match e.to_string() {
            e if e.contains("is a path") => format!("{e}; open paths with nixi --path"),
            e => e,
        };
        let old = old.index(gens).map_err(not_here)?;
        let new = new.index(gens).map_err(not_here)?;
        let tab = self.tab_mut();
        tab.base = Some(old);
        tab.target = Some(new);
        tab.cursor = new;
        self.sync();
        Ok(())
    }

    /// Opens on a range once the active tab can resolve it (home-manager loads later).
    pub fn start_with_range(&mut self, range: String) {
        self.pending_range = Some(range);
        self.apply_pending_range();
    }

    fn apply_pending_range(&mut self) {
        if matches!(self.tab().gens, Gens::Loading) {
            return;
        }
        if let Some(range) = self.pending_range.take() {
            if let Err(e) = self.apply_range(&range) {
                self.message = Some(e);
            }
        }
    }

    pub fn on_msg(&mut self, msg: Msg) {
        match msg {
            Msg::Closure { store_path, result } => {
                self.requested.remove(&store_path);
                match result {
                    Ok(closure) => {
                        self.closures.insert(store_path, closure);
                    }
                    Err(e) => {
                        self.errors.insert(store_path, e);
                    }
                }
            }
            Msg::HomeGenerations(result) => {
                if let Some(i) = self.tabs.iter().position(|t| t.kind == TabKind::Home) {
                    self.tabs[i].set_gens(match result {
                        Ok(gens) => Gens::Ready(gens),
                        Err(e) => Gens::Failed(e),
                    });
                    self.prefetch(i);
                }
                self.apply_pending_range();
            }
            Msg::Repo(result) => {
                self.repo = match result {
                    Ok(repo) => {
                        let links = self
                            .tabs
                            .iter()
                            .enumerate()
                            .filter(|(_, t)| t.kind == TabKind::Profile)
                            .map(|(i, t)| (i, repo.links(t.generations())))
                            .collect();
                        RepoState::Ready { repo, links }
                    }
                    Err(e) => RepoState::Failed(e),
                };
            }
            Msg::Job { key, result } => {
                let state = match result {
                    Ok(out) => JobState::Done(out),
                    Err(e) => JobState::Failed(e),
                };
                self.jobs.insert(key, state);
            }
        }
        self.sync();
    }

    /// Opens the delete confirmation for the highlighted generation, or explains why not.
    fn start_delete(&mut self) {
        let tab = self.tab();
        let refusal = match tab.kind {
            TabKind::Home => Some(
                "home-manager generations are part of system generations; \
                 delete the system generation instead"
                    .to_owned(),
            ),
            TabKind::Paths => Some("ad-hoc paths aren't generations".to_owned()),
            TabKind::Profile => None,
        };
        let Some(g) = tab.generations().get(tab.cursor).cloned() else {
            return;
        };
        if let Some(reason) = refusal.or_else(|| delete::refusal(&g)) {
            self.message = Some(format!("Can't delete: {reason}"));
            return;
        }
        let Some(profile) = delete::profile_of(&g.path) else {
            return;
        };
        let sudo = delete::needs_sudo(&profile);
        self.delete_confirm = Some(DeleteConfirm {
            tab: self.active,
            command: delete::command_line(&profile, &[g.number], sudo).join(" "),
            warnings: delete::warnings(&g),
            generation: g,
            profile,
            input: String::new(),
        });
    }

    fn on_delete_key(&mut self, code: KeyCode) {
        let Some(confirm) = &mut self.delete_confirm else {
            return;
        };
        match code {
            KeyCode::Char(c) if c.is_ascii_digit() => confirm.input.push(c),
            KeyCode::Backspace => {
                confirm.input.pop();
            }
            KeyCode::Esc => {
                self.delete_confirm = None;
                self.message = Some("Deletion cancelled; nothing deleted.".to_owned());
            }
            KeyCode::Enter => {
                let confirm = self.delete_confirm.take().expect("checked above");
                let number = confirm.generation.number;
                if confirm.input == number.to_string() {
                    self.pending = Some(Pending::Delete(PendingDelete {
                        tab: confirm.tab,
                        sudo: delete::needs_sudo(&confirm.profile),
                        profile: confirm.profile,
                        number,
                    }));
                } else {
                    self.message = Some(format!(
                        "Typed {:?}, not {number}; nothing deleted.",
                        confirm.input
                    ));
                }
            }
            _ => {}
        }
    }

    /// A confirmed action waiting to be run by the event loop.
    pub fn take_pending(&mut self) -> Option<Pending> {
        self.pending.take()
    }

    /// Updates a tab after a deletion attempt, with its re-read generations on success.
    pub fn deleted(&mut self, tab: usize, number: u64, result: Result<Vec<Generation>, String>) {
        match result {
            Ok(gens) => {
                let t = &mut self.tabs[tab];
                let cursor = t.cursor;
                t.set_gens(Gens::Ready(gens));
                t.cursor = cursor.min(t.generations().len().saturating_sub(1));
                // Pins are indices into the old list.
                t.base = None;
                t.target = None;
                self.message = Some(format!(
                    "Deleted generation {number}. Press C to collect garbage and free the space."
                ));
            }
            Err(e) => self.message = Some(format!("Deleting generation {number} failed: {e}")),
        }
        self.sync();
    }

    pub fn on_key(&mut self, key: KeyEvent) {
        self.message = None;
        if self.delete_confirm.is_some() {
            self.on_delete_key(key.code);
            return;
        }
        if self.gc_confirm {
            self.gc_confirm = false;
            if key.code == KeyCode::Char('y') {
                self.pending = Some(Pending::Gc);
            } else {
                self.message = Some("Garbage collection cancelled.".to_owned());
            }
            return;
        }
        if let Some(input) = &mut self.range_input {
            match key.code {
                KeyCode::Char(c) => input.push(c),
                KeyCode::Backspace => {
                    input.pop();
                }
                KeyCode::Enter => {
                    let text = std::mem::take(input);
                    self.range_input = None;
                    if let Err(e) = self.apply_range(&text) {
                        self.message = Some(e);
                    }
                }
                KeyCode::Esc => self.range_input = None,
                _ => {}
            }
            return;
        }
        if self.editing_filter {
            match key.code {
                KeyCode::Char(c) => self.filter.push(c),
                KeyCode::Backspace => {
                    self.filter.pop();
                }
                KeyCode::Enter => self.editing_filter = false,
                KeyCode::Esc => {
                    self.filter.clear();
                    self.editing_filter = false;
                }
                _ => {}
            }
            self.select_first_row();
            return;
        }
        if self.show_help {
            self.show_help = false;
            return;
        }
        if self.detail.is_some() {
            self.on_detail_key(key.code);
            return;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }

        match key.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Char('?') => self.show_help = true,
            KeyCode::Char(c @ '1'..='9') => {
                let i = c as usize - '1' as usize;
                if i < self.tabs.len() {
                    self.active = i;
                }
            }
            KeyCode::Tab | KeyCode::BackTab => {
                self.focus = match self.focus {
                    Focus::List => Focus::Diff,
                    Focus::Diff => Focus::List,
                }
            }
            KeyCode::Left | KeyCode::Char('h') => self.focus = Focus::List,
            KeyCode::Right | KeyCode::Char('l') => self.focus = Focus::Diff,
            KeyCode::Char('/') => {
                self.editing_filter = true;
                self.focus = Focus::Diff;
            }
            KeyCode::Char(':') => self.range_input = Some(String::new()),
            KeyCode::Char('D') => self.start_delete(),
            KeyCode::Char('C') => self.gc_confirm = true,
            KeyCode::Char('s') => {
                self.sort = match self.sort {
                    Sort::Name => Sort::Size,
                    Sort::Size => Sort::Name,
                };
                self.select_first_row();
            }
            KeyCode::Char(c @ ('u' | 'd' | 'c' | 'a' | 'r' | 'b')) => {
                if !self.hidden.remove(&c) {
                    self.hidden.insert(c);
                }
                self.select_first_row();
            }
            KeyCode::Char(' ') => {
                let tab = self.tab_mut();
                tab.base = (tab.base != Some(tab.cursor)).then_some(tab.cursor);
            }
            KeyCode::Char(c @ ('n' | 'L')) => {
                let mode = if c == 'n' {
                    DiffMode::Nvd
                } else {
                    DiffMode::Commits
                };
                self.diff_mode = if self.diff_mode == mode {
                    DiffMode::Packages
                } else {
                    mode
                };
                self.text_scroll = 0;
            }
            KeyCode::Enter if self.focus == Focus::Diff => {
                if self.diff_mode == DiffMode::Packages {
                    self.open_detail();
                }
            }
            KeyCode::Char('w') if self.focus == Focus::Diff => {
                if self.diff_mode == DiffMode::Packages {
                    self.open_detail();
                    self.why_depends();
                }
            }
            KeyCode::Enter => {
                let tab = self.tab_mut();
                tab.target = (tab.target != Some(tab.cursor)).then_some(tab.cursor);
            }
            KeyCode::Esc => {
                if !self.filter.is_empty() {
                    self.filter.clear();
                    self.select_first_row();
                } else {
                    let tab = self.tab_mut();
                    tab.base = None;
                    tab.target = None;
                }
            }
            code => self.move_selection(code),
        }
        self.sync();
    }

    fn move_selection(&mut self, code: KeyCode) {
        if self.focus == Focus::Diff && self.diff_mode != DiffMode::Packages {
            let page = self.diff_height.max(1);
            let max = self.text_line_count().saturating_sub(page);
            let pos = self.text_scroll;
            self.text_scroll = match code {
                KeyCode::Up | KeyCode::Char('k') => pos.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => (pos + 1).min(max),
                KeyCode::PageUp => pos.saturating_sub(page),
                KeyCode::PageDown => (pos + page).min(max),
                KeyCode::Home | KeyCode::Char('g') => 0,
                KeyCode::End | KeyCode::Char('G') => max,
                _ => return,
            };
            return;
        }
        let (len, page, pos) = match self.focus {
            Focus::List => (
                self.tab().generations().len(),
                self.list_height,
                self.tab().cursor,
            ),
            Focus::Diff => (
                self.visible_rows().len(),
                self.diff_height,
                self.diff_state.selected().unwrap_or(0),
            ),
        };
        if len == 0 {
            return;
        }
        let last = len - 1;
        let page = page.max(1);
        let pos = match code {
            KeyCode::Up | KeyCode::Char('k') => pos.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => (pos + 1).min(last),
            KeyCode::PageUp => pos.saturating_sub(page),
            KeyCode::PageDown => (pos + page).min(last),
            KeyCode::Home | KeyCode::Char('g') => 0,
            KeyCode::End | KeyCode::Char('G') => last,
            _ => return,
        };
        match self.focus {
            Focus::List => self.tab_mut().cursor = pos,
            Focus::Diff => self.diff_state.select(Some(pos)),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::closure::PathInfo;
    use crate::ui::detail::Side;
    use std::path::PathBuf;
    use std::sync::mpsc;

    fn closure(root: &str, packages: &[&str]) -> Arc<Closure> {
        let info = |path: &str, references: Vec<String>, nar_size| PathInfo {
            path: path.into(),
            nar_size,
            references,
        };
        let mut infos = vec![info(
            root,
            packages.iter().map(|p| p.to_string()).collect(),
            1,
        )];
        infos.extend(
            packages
                .iter()
                .map(|p| info(p, vec![], 1000 * p.len() as u64)),
        );
        Arc::new(Closure::new(root.into(), None, infos))
    }

    /// Three system generations with preloaded closures; the loader has no workers.
    pub(crate) fn app() -> App {
        let packages: [&[&str]; 3] = [
            &["/nix/store/a-firefox-140.0", "/nix/store/b-htop-3.3"],
            &[
                "/nix/store/a2-firefox-141.0",
                "/nix/store/b-htop-3.3",
                "/nix/store/c-ripgrep-14.1",
            ],
            &["/nix/store/a3-firefox-141.0.2", "/nix/store/c-ripgrep-14.1"],
        ];
        let gens = (0..3)
            .map(|i| Generation {
                number: 41 + i as u64,
                last_number: 41 + i as u64,
                path: PathBuf::from(format!("/nix/var/nix/profiles/system-{}-link", 41 + i)),
                // Unversioned, so the roots themselves only count as (hidden) rebuilds.
                store_path: format!("/nix/store/s{i}-nixos-system"),
                created: None,
                current: i == 2,
            })
            .collect::<Vec<_>>();
        let (tx, _rx) = mpsc::channel();
        let mut app = App::new(
            vec![
                Tab::new("system", TabKind::Profile, Gens::Ready(gens.clone())),
                Tab::new("home (bob)", TabKind::Home, Gens::Loading),
            ],
            0,
            Loader::new(0, None, tx),
        );
        for (g, pkgs) in gens.iter().zip(packages) {
            app.on_msg(Msg::Closure {
                store_path: g.store_path.clone(),
                result: Ok(closure(&g.store_path, pkgs)),
            });
        }
        app
    }

    fn press(app: &mut App, keys: &str) {
        for c in keys.chars() {
            let code = match c {
                '\n' => KeyCode::Enter,
                '\x1b' => KeyCode::Esc,
                '\t' => KeyCode::Tab,
                c => KeyCode::Char(c),
            };
            app.on_key(KeyEvent::new(code, KeyModifiers::NONE));
        }
    }

    fn visible(app: &App) -> Vec<(Category, &str)> {
        app.visible_rows()
            .iter()
            .map(|r| (r.category, r.pname.as_str()))
            .collect()
    }

    #[test]
    fn defaults_to_current_vs_previous() {
        let app = app();
        assert_eq!(app.tab().pair(), Some((1, 2)));
        assert_eq!(
            visible(&app),
            [(Category::Upgraded, "firefox"), (Category::Removed, "htop")]
        );
    }

    #[test]
    fn moving_the_cursor_follows_the_switch() {
        let mut app = app();
        press(&mut app, "k");
        assert_eq!(app.tab().pair(), Some((0, 1)));
        assert_eq!(
            visible(&app),
            [
                (Category::Upgraded, "firefox"),
                (Category::Added, "ripgrep")
            ]
        );
        press(&mut app, "k");
        assert_eq!(app.tab().pair(), None);
    }

    #[test]
    fn pinning_old_side() {
        let mut app = app();
        press(&mut app, "gG"); // pin nothing yet; cursor at the newest
        press(&mut app, "g "); // pin generation 41 as old
        press(&mut app, "G");
        assert_eq!(app.tab().pair(), Some((0, 2)));
        assert_eq!(
            visible(&app),
            [
                (Category::Upgraded, "firefox"),
                (Category::Added, "ripgrep"),
                (Category::Removed, "htop")
            ]
        );
        press(&mut app, "\x1b");
        assert_eq!(app.tab().pair(), Some((1, 2)));
    }

    #[test]
    fn detail_popup_shows_paths_and_referrers() {
        let mut app = app();
        press(&mut app, "\t\n"); // focus the diff, open the first row (firefox)
        let detail = app.detail.as_ref().expect("detail open");
        assert_eq!(detail.row.pname, "firefox");
        let paths: Vec<(Side, &str)> = detail
            .paths
            .iter()
            .map(|p| (p.side, p.path.as_str()))
            .collect();
        assert_eq!(
            paths,
            [
                (Side::Old, "/nix/store/a2-firefox-141.0"),
                (Side::New, "/nix/store/a3-firefox-141.0.2")
            ]
        );
        assert_eq!(detail.cursor, 1, "starts on the new side");
        assert_eq!(detail.referrers, ["nixos-system"]);
        assert_eq!(detail.selected_on(), (true, true));
        press(&mut app, "k\x1b");
        assert!(app.detail.is_none());

        press(&mut app, "j\n"); // htop, removed: referrers come from the old side
        let detail = app.detail.as_ref().expect("detail open");
        assert_eq!(detail.row.pname, "htop");
        assert_eq!(detail.referrers_side, Side::Old);
        assert_eq!(detail.paths.len(), 1);
    }

    #[test]
    fn nvd_mode_tracks_the_job() {
        let mut app = app();
        press(&mut app, "n");
        assert!(matches!(app.nvd_job(), Some(JobState::Running)));
        let (old, new) = app.last_pair.clone().unwrap();
        let output: String = (0..30).map(|i| format!("line {i}\n")).collect();
        app.on_msg(Msg::Job {
            key: JobKey::Nvd { old, new },
            result: Ok(output),
        });
        press(&mut app, "\tG");
        assert_eq!(app.text_scroll, 20, "30 lines, 10 visible");
        press(&mut app, "k");
        assert_eq!(app.text_scroll, 19);
        press(&mut app, "\t"); // back to the list; moving resets the scroll
        press(&mut app, "k");
        assert_eq!(app.text_scroll, 0);
    }

    #[test]
    fn range_prompt_pins_both_sides() {
        let mut app = app();
        press(&mut app, ":-2:-1\n");
        assert_eq!(app.tab().pair(), Some((0, 1)));
        assert_eq!(app.tab().cursor, 1);
        press(&mut app, ":41:43\n");
        assert_eq!(app.tab().pair(), Some((0, 2)));
        press(&mut app, "\x1b"); // unpin: back to following the cursor
        assert_eq!(app.tab().pair(), Some((1, 2)));

        press(&mut app, ":-2\n"); // a lone side is compared with the current generation
        assert_eq!(app.tab().pair(), Some((0, 2)));
        press(&mut app, "\x1b");
        press(&mut app, ":-5:0\n");
        assert!(app.message.as_deref().unwrap().contains("can't go back 5"));
        assert_eq!(app.tab().pair(), Some((1, 2)), "unchanged on error");
        press(&mut app, "j");
        assert!(app.message.is_none(), "cleared by the next key");
        press(&mut app, ":0:./result\n");
        assert!(app.message.as_deref().unwrap().contains("nixi --path"));
        press(&mut app, ":-1:0\x1b"); // esc cancels without applying
        assert!(app.range_input.is_none());
    }

    #[test]
    fn delete_needs_the_typed_generation_number() {
        let mut app = app();
        // The cursor starts on the current generation (43), which is never deletable.
        press(&mut app, "D");
        assert!(app.delete_confirm.is_none());
        assert!(app.message.as_deref().unwrap().contains("current"));

        press(&mut app, "kD");
        let confirm = app.delete_confirm.as_ref().expect("confirmation open");
        assert_eq!(confirm.generation.number, 42);
        assert!(
            confirm.command.ends_with("--delete-generations 42"),
            "{}",
            confirm.command
        );
        assert!(confirm.warnings.iter().any(|w| w.contains("boot menu")));

        press(&mut app, "41\n"); // wrong number
        assert!(app.take_pending().is_none());
        assert!(app.message.as_deref().unwrap().contains("nothing deleted"));

        press(&mut app, "D4x2\x1b"); // letters are ignored; esc cancels
        assert!(app.delete_confirm.is_none());
        assert!(app.take_pending().is_none());

        press(&mut app, "D42\n");
        let Some(Pending::Delete(pending)) = app.take_pending() else {
            panic!("deletion not confirmed");
        };
        assert_eq!((pending.tab, pending.number), (0, 42));
        assert!(pending.profile.ends_with("system"));

        let remaining: Vec<Generation> = app.tabs[0]
            .generations()
            .iter()
            .filter(|g| g.number != 42)
            .cloned()
            .collect();
        app.deleted(0, 42, Ok(remaining));
        let numbers: Vec<u64> = app.tab().generations().iter().map(|g| g.number).collect();
        assert_eq!(numbers, [41, 43]);
        assert_eq!(app.tab().cursor, 1);
        assert!(app
            .message
            .as_deref()
            .unwrap()
            .contains("Deleted generation 42"));
    }

    #[test]
    fn gc_needs_y_to_confirm() {
        let mut app = app();
        press(&mut app, "Cn");
        assert!(!app.gc_confirm && app.take_pending().is_none());
        assert!(app.message.as_deref().unwrap().contains("cancelled"));
        press(&mut app, "C");
        assert!(app.gc_confirm);
        press(&mut app, "y");
        assert!(matches!(app.take_pending(), Some(Pending::Gc)));
    }

    #[test]
    fn delete_is_refused_outside_profile_tabs() {
        let mut app = app();
        let gens = app.tabs[0].generations().to_vec();
        app.on_msg(Msg::HomeGenerations(Ok(gens)));
        press(&mut app, "2kD");
        assert!(app.delete_confirm.is_none());
        assert!(app.message.as_deref().unwrap().contains("home-manager"));
    }

    #[test]
    fn start_range_waits_for_generations() {
        let mut app = app();
        app.active = 1; // home tab, still loading
        app.start_with_range("-1:0".into());
        assert_eq!(app.tab().pair(), None);
        let gens = app.tabs[0].generations().to_vec();
        app.on_msg(Msg::HomeGenerations(Ok(gens)));
        assert_eq!(app.tab().base, Some(1));
        assert_eq!(app.tab().target, Some(2));
    }

    #[test]
    fn filter_toggles_and_sort() {
        let mut app = app();
        press(&mut app, "/HT\n");
        assert_eq!(visible(&app), [(Category::Removed, "htop")]);
        press(&mut app, "\x1br");
        assert_eq!(visible(&app), [(Category::Upgraded, "firefox")]);
        press(&mut app, "r");
        // Sizes are 1000 per path character: removing htop outweighs firefox's growth.
        press(&mut app, "s");
        assert_eq!(
            visible(&app),
            [(Category::Removed, "htop"), (Category::Upgraded, "firefox")]
        );
        let sizes: Vec<i64> = app.visible_rows().iter().map(|r| r.size_delta).collect();
        assert_eq!(sizes, [-21000, 2000]);
        press(&mut app, "b");
        assert_eq!(app.visible_rows().len(), 3, "rebuilt root shown after `b`");
    }
}

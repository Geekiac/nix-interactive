//! TUI state and key handling, independent of drawing.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::widgets::{ListState, TableState};

use super::loader::{Loader, Msg};
use crate::closure::Closure;
use crate::diff::{self, ChangeKind, ClosureDiff, PackageEntry, Selection};
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
    /// Toggle keys of hidden categories.
    pub hidden: HashSet<char>,
    pub sort: Sort,
    pub diff_state: TableState,
    pub show_help: bool,
    pub quit: bool,
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
            hidden: HashSet::from(['b']),
            sort: Sort::Name,
            diff_state: TableState::default(),
            show_help: false,
            quit: false,
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

    /// Loads what the screen needs and recomputes the diff when the pair changed.
    pub fn sync(&mut self) {
        let Some((old, new)) = self
            .pair_paths()
            .map(|(o, n)| (o.store_path.clone(), n.store_path.clone()))
        else {
            return;
        };
        self.request(&new, true);
        self.request(&old, true);
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
            }
        }
        self.sync();
    }

    pub fn on_key(&mut self, key: KeyEvent) {
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

//! Package detail popup: every store path of a package on both sides, what directly
//! requires it, and `nix why-depends` on demand.

use std::collections::HashSet;

use super::app::{Category, DiffRow};
use crate::closure::Closure;
use crate::diff::{PackageSet, Selection};
use crate::sources::Generation;
use crate::store_path::{name, Version};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Old,
    New,
}

pub struct DetailPath {
    pub side: Side,
    pub path: String,
    pub version: Version,
    pub nar_size: u64,
}

pub struct Detail {
    pub row: DiffRow,
    pub old_label: String,
    pub new_label: String,
    pub old_root: String,
    pub new_root: String,
    /// Old side's paths first, then the new side's.
    pub paths: Vec<DetailPath>,
    pub cursor: usize,
    /// Names of store paths that directly reference this package, on `referrers_side`
    /// (the new side, unless the package was removed).
    pub referrers: Vec<String>,
    pub referrers_side: Side,
    /// The `nix why-depends` run shown in the popup, as `(root, path)`.
    pub why: Option<(String, String)>,
    pub scroll: u16,
    /// Scroll to the why-depends output at the next draw.
    pub jump_to_why: bool,
}

fn package_paths(closure: &Closure, pname: &str, side: Side) -> Vec<DetailPath> {
    let set = PackageSet::from_paths(closure.paths.keys().map(String::as_str));
    set.get(pname)
        .unwrap_or(&[])
        .iter()
        .map(|p| DetailPath {
            side,
            path: p.path.clone(),
            version: p.version.clone(),
            nar_size: closure.paths.get(&p.path).map_or(0, |i| i.nar_size),
        })
        .collect()
}

/// Names of paths in `closure` that directly reference any of `targets`, sorted.
fn referrers(closure: &Closure, targets: &HashSet<&str>) -> Vec<String> {
    let mut names: Vec<String> = closure
        .paths
        .values()
        .filter(|i| !targets.contains(i.path.as_str()))
        .filter(|i| i.references.iter().any(|r| targets.contains(r.as_str())))
        .map(|i| name(&i.path).to_owned())
        .collect();
    names.sort();
    names.dedup();
    names
}

impl Detail {
    pub fn new(
        row: &DiffRow,
        (old, new): (&Generation, &Generation),
        (left, right): (&Closure, &Closure),
        label: impl Fn(&Generation) -> String,
    ) -> Self {
        let mut paths = package_paths(left, &row.pname, Side::Old);
        paths.extend(package_paths(right, &row.pname, Side::New));
        let referrers_side = if row.category == Category::Removed {
            Side::Old
        } else {
            Side::New
        };
        let targets: HashSet<&str> = paths
            .iter()
            .filter(|p| p.side == referrers_side)
            .map(|p| p.path.as_str())
            .collect();
        let referrers = referrers(
            if referrers_side == Side::Old {
                left
            } else {
                right
            },
            &targets,
        );
        // Start on the first path of the side that matters most.
        let cursor = paths
            .iter()
            .position(|p| p.side == referrers_side)
            .unwrap_or(0);
        Self {
            row: row.clone(),
            old_label: label(old),
            new_label: label(new),
            old_root: old.store_path.clone(),
            new_root: new.store_path.clone(),
            paths,
            cursor,
            referrers,
            referrers_side,
            why: None,
            scroll: 0,
            jump_to_why: false,
        }
    }

    pub fn selected(&self) -> Option<&DetailPath> {
        self.paths.get(self.cursor)
    }

    pub fn root(&self, side: Side) -> &str {
        match side {
            Side::Old => &self.old_root,
            Side::New => &self.new_root,
        }
    }

    /// Whether the package is directly selected (e.g. `environment.systemPackages`) on each side.
    pub fn selected_on(&self) -> (bool, bool) {
        match self.row.selection {
            Selection::Unselected => (false, false),
            Selection::NewlyUnselected => (true, false),
            Selection::NewlySelected => (false, true),
            Selection::Selected => (true, true),
        }
    }
}

//! Package-level closure diffing, following nvd's semantics so results can be checked
//! against `nvd diff`, plus a "rebuilt" category nvd doesn't report.

use std::collections::{BTreeMap, HashSet};

use crate::closure::Closure;
use crate::store_path::{parse_name, Version};

#[derive(Debug, Clone)]
pub struct Package {
    pub version: Version,
    pub path: String,
}

/// Store paths grouped by pname, each group sorted by ascending version.
#[derive(Debug, Default)]
pub struct PackageSet {
    by_pname: BTreeMap<String, Vec<Package>>,
}

impl PackageSet {
    pub fn from_paths<'a>(paths: impl IntoIterator<Item = &'a str>) -> Self {
        let mut by_pname: BTreeMap<String, Vec<Package>> = BTreeMap::new();
        for path in paths {
            let (pname, version) = parse_name(path);
            by_pname.entry(pname.to_owned()).or_default().push(Package {
                version: Version::new(version),
                path: path.to_owned(),
            });
        }
        for pkgs in by_pname.values_mut() {
            pkgs.sort_by(|a, b| a.version.cmp(&b.version));
        }
        Self { by_pname }
    }

    pub fn contains(&self, pname: &str) -> bool {
        self.by_pname.contains_key(pname)
    }

    pub fn get(&self, pname: &str) -> Option<&[Package]> {
        self.by_pname.get(pname).map(Vec::as_slice)
    }
}

/// Whether a pname is directly selected (e.g. in `environment.systemPackages`) on each side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Selection {
    Unselected,
    NewlySelected,
    NewlyUnselected,
    Selected,
}

impl Selection {
    fn new(left: bool, right: bool) -> Self {
        match (left, right) {
            (false, false) => Self::Unselected,
            (true, false) => Self::NewlyUnselected,
            (false, true) => Self::NewlySelected,
            (true, true) => Self::Selected,
        }
    }

    pub fn marker(self) -> char {
        match self {
            Self::Unselected => '.',
            Self::NewlyUnselected => '-',
            Self::NewlySelected => '+',
            Self::Selected => '*',
        }
    }

    pub fn selected_anywhere(self) -> bool {
        self != Self::Unselected
    }

    fn changed(self) -> bool {
        matches!(self, Self::NewlySelected | Self::NewlyUnselected)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    Upgraded,
    Downgraded,
    /// Version sets overlap, e.g. one of two versions was bumped.
    Changed,
}

impl ChangeKind {
    pub fn marker(self) -> char {
        match self {
            Self::Upgraded => 'U',
            Self::Downgraded => 'D',
            Self::Changed => 'C',
        }
    }
}

#[derive(Debug, Clone)]
pub struct VersionChange {
    pub pname: String,
    pub kind: ChangeKind,
    pub left: Vec<Version>,
    pub right: Vec<Version>,
    pub selection: Selection,
}

#[derive(Debug, Clone)]
pub struct PackageEntry {
    pub pname: String,
    pub versions: Vec<Version>,
    pub selection: Selection,
}

#[derive(Debug, Default)]
pub struct ClosureDiff {
    pub version_changes: Vec<VersionChange>,
    /// Same versions, but the package became (un)selected.
    pub selection_changes: Vec<PackageEntry>,
    pub added: Vec<PackageEntry>,
    pub removed: Vec<PackageEntry>,
    /// Same versions, different store paths: rebuilt because a dependency or build input changed.
    pub rebuilt: Vec<String>,
    pub left_path_count: usize,
    pub right_path_count: usize,
    pub paths_added: usize,
    pub paths_removed: usize,
    pub left_size: u64,
    pub right_size: u64,
}

fn versions(pkgs: &[Package]) -> Vec<Version> {
    pkgs.iter().map(|p| p.version.clone()).collect()
}

fn paths(pkgs: &[Package]) -> Vec<&str> {
    let mut paths: Vec<&str> = pkgs.iter().map(|p| p.path.as_str()).collect();
    paths.sort_unstable();
    paths
}

/// Packages nvd treats as selected: direct references of `<root>/sw` when both sides are
/// NixOS systems, otherwise of the root itself.
fn selected_set(closure: &Closure, use_sw: bool) -> PackageSet {
    let root = match (&closure.sw, use_sw) {
        (Some(sw), true) => sw.as_str(),
        _ => closure.root.as_str(),
    };
    PackageSet::from_paths(closure.references(root).iter().map(String::as_str))
}

pub fn diff(left: &Closure, right: &Closure) -> ClosureDiff {
    let use_sw = left.sw.is_some() && right.sw.is_some();
    let (left_sel, right_sel) = (selected_set(left, use_sw), selected_set(right, use_sw));
    let selection =
        |pname: &str| Selection::new(left_sel.contains(pname), right_sel.contains(pname));

    let left_set = PackageSet::from_paths(left.paths.keys().map(String::as_str));
    let right_set = PackageSet::from_paths(right.paths.keys().map(String::as_str));

    let mut d = ClosureDiff::default();
    for (pname, left_pkgs) in &left_set.by_pname {
        let sel = selection(pname);
        let Some(right_pkgs) = right_set.get(pname) else {
            d.removed.push(PackageEntry {
                pname: pname.clone(),
                versions: versions(left_pkgs),
                selection: sel,
            });
            continue;
        };
        let (lv, rv) = (versions(left_pkgs), versions(right_pkgs));
        if lv != rv {
            let kind = if lv[lv.len() - 1] < rv[0] {
                ChangeKind::Upgraded
            } else if lv[0] > rv[rv.len() - 1] {
                ChangeKind::Downgraded
            } else {
                ChangeKind::Changed
            };
            d.version_changes.push(VersionChange {
                pname: pname.clone(),
                kind,
                left: lv,
                right: rv,
                selection: sel,
            });
        } else if sel.changed() {
            d.selection_changes.push(PackageEntry {
                pname: pname.clone(),
                versions: lv,
                selection: sel,
            });
        } else if paths(left_pkgs) != paths(right_pkgs) {
            d.rebuilt.push(pname.clone());
        }
    }
    for (pname, right_pkgs) in &right_set.by_pname {
        if !left_set.contains(pname) {
            d.added.push(PackageEntry {
                pname: pname.clone(),
                versions: versions(right_pkgs),
                selection: selection(pname),
            });
        }
    }

    let left_paths: HashSet<&String> = left.paths.keys().collect();
    let right_paths: HashSet<&String> = right.paths.keys().collect();
    d.left_path_count = left_paths.len();
    d.right_path_count = right_paths.len();
    d.paths_added = right_paths.difference(&left_paths).count();
    d.paths_removed = left_paths.difference(&right_paths).count();
    d.left_size = left.total_nar_size();
    d.right_size = right.total_nar_size();
    d
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::closure::PathInfo;

    /// Builds a closure whose root directly references (selects) the `selected` paths.
    fn closure(root: &str, selected: &[&str], others: &[&str]) -> Closure {
        let info = |path: &str, references: Vec<String>| PathInfo {
            path: path.into(),
            nar_size: 100,
            references,
        };
        let mut infos = vec![info(root, selected.iter().map(|s| s.to_string()).collect())];
        infos.extend(selected.iter().chain(others).map(|p| info(p, vec![])));
        Closure::new(root.into(), None, infos)
    }

    #[test]
    fn classifies_changes() {
        let left = closure(
            "/nix/store/r1-system",
            &["/nix/store/a-firefox-140.0", "/nix/store/b-htop-3.3"],
            &[
                "/nix/store/c-openssl-3.4",
                "/nix/store/d-python3-3.12",
                "/nix/store/e-gone-1.0",
                "/nix/store/f-zlib-1.3",
                "/nix/store/g-curl-8.1",
                "/nix/store/h-curl-8.2",
            ],
        );
        let right = closure(
            "/nix/store/r2-system",
            &["/nix/store/a2-firefox-141.0", "/nix/store/d-python3-3.12"],
            &[
                "/nix/store/b-htop-3.3",
                "/nix/store/c2-openssl-3.3",
                "/nix/store/i-new-2.0",
                "/nix/store/f2-zlib-1.3",
                "/nix/store/g-curl-8.1",
                "/nix/store/j-curl-8.3",
            ],
        );
        let d = diff(&left, &right);

        let changes: Vec<(&str, ChangeKind, Selection)> = d
            .version_changes
            .iter()
            .map(|c| (c.pname.as_str(), c.kind, c.selection))
            .collect();
        assert_eq!(
            changes,
            [
                ("curl", ChangeKind::Changed, Selection::Unselected),
                ("firefox", ChangeKind::Upgraded, Selection::Selected),
                ("openssl", ChangeKind::Downgraded, Selection::Unselected),
            ]
        );
        let sel: Vec<(&str, Selection)> = d
            .selection_changes
            .iter()
            .map(|e| (e.pname.as_str(), e.selection))
            .collect();
        assert_eq!(
            sel,
            [
                ("htop", Selection::NewlyUnselected),
                ("python3", Selection::NewlySelected)
            ]
        );
        assert_eq!(
            d.added.iter().map(|e| e.pname.as_str()).collect::<Vec<_>>(),
            ["new"]
        );
        assert_eq!(
            d.removed
                .iter()
                .map(|e| e.pname.as_str())
                .collect::<Vec<_>>(),
            ["gone"]
        );
        // The unversioned roots share the pname `system`, so they count as a rebuild too.
        assert_eq!(d.rebuilt, ["system", "zlib"]);
        assert_eq!((d.left_path_count, d.right_path_count), (9, 9));
        assert_eq!((d.paths_added, d.paths_removed), (6, 6));
        assert_eq!((d.left_size, d.right_size), (900, 900));
    }
}

//! Nix profiles: a `<name>` symlink pointing at one of its `<name>-<N>-link` generations.
//! Covers the NixOS system profile, standalone home-manager, and `nix profile` profiles.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};

use super::Generation;
use crate::closure::resolve_store_path;

pub const SYSTEM_PROFILE: &str = "/nix/var/nix/profiles/system";

/// Parses `<name>-<N>-link` into `N` when `<name>` matches.
fn generation_number(file_name: &str, name: &str) -> Option<u64> {
    file_name
        .strip_prefix(name)?
        .strip_prefix('-')?
        .strip_suffix("-link")?
        .parse()
        .ok()
}

fn profile_name(profile: &Path) -> Result<&str> {
    profile
        .file_name()
        .and_then(|n| n.to_str())
        .with_context(|| format!("invalid profile path {}", profile.display()))
}

/// The generation `profile` currently points at.
pub fn current_generation(profile: &Path) -> Result<u64> {
    let target = fs::read_link(profile)
        .with_context(|| format!("{} is not a profile symlink", profile.display()))?;
    target
        .file_name()
        .and_then(|f| f.to_str())
        .and_then(|f| generation_number(f, profile_name(profile).ok()?))
        .with_context(|| {
            format!(
                "{} points at {}, not a generation link",
                profile.display(),
                target.display()
            )
        })
}

/// All generations of `profile`, oldest first. Numbers can have gaps after garbage collection.
pub fn generations(profile: &Path) -> Result<Vec<Generation>> {
    let name = profile_name(profile)?;
    let current = current_generation(profile)?;
    let dir = profile.parent().unwrap_or(Path::new("."));
    let mut gens = Vec::new();
    for entry in fs::read_dir(dir).with_context(|| format!("cannot read {}", dir.display()))? {
        let path = entry?.path();
        let Some(n) = path
            .file_name()
            .and_then(|f| f.to_str())
            .and_then(|f| generation_number(f, name))
        else {
            continue;
        };
        gens.push(Generation {
            number: n,
            last_number: n,
            store_path: resolve_store_path(&path)?,
            created: fs::symlink_metadata(&path).and_then(|m| m.modified()).ok(),
            current: n == current,
            path,
        });
    }
    gens.sort_by_key(|g| g.number);
    Ok(gens)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::relative;
    use std::os::unix::fs::symlink;

    #[test]
    fn parses_generation_numbers() {
        assert_eq!(generation_number("system-43-link", "system"), Some(43));
        assert_eq!(generation_number("system-43-link", "home"), None);
        assert_eq!(generation_number("system-foo-43-link", "system"), None);
        assert_eq!(generation_number("system", "system"), None);
    }

    #[test]
    fn lists_generations_across_gaps() {
        let dir = std::env::temp_dir().join(format!("nixi-profile-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        // Any store path works as a target; the links only need to resolve. $PATH has some
        // both in `nix develop` and in the build sandbox (where /run/current-system is absent).
        let path_var = std::env::var("PATH").unwrap();
        let target = path_var
            .split(':')
            .find(|p| p.starts_with("/nix/store/"))
            .expect("a store path on $PATH");
        for n in [3, 7, 9] {
            symlink(target, dir.join(format!("system-{n}-link"))).unwrap();
        }
        symlink(target, dir.join("other-8-link")).unwrap();
        symlink("system-9-link", dir.join("system")).unwrap();

        let gens = generations(&dir.join("system")).unwrap();
        let numbers: Vec<(u64, bool)> = gens.iter().map(|g| (g.number, g.current)).collect();
        assert_eq!(numbers, [(3, false), (7, false), (9, true)]);
        let numbers = |back| relative(&gens, back).map(|g| g.number).ok();
        assert_eq!(
            (numbers(0), numbers(1), numbers(2), numbers(3)),
            (Some(9), Some(7), Some(3), None)
        );

        fs::remove_file(dir.join("system")).unwrap();
        symlink("system-3-link", dir.join("system")).unwrap();
        assert!(relative(&generations(&dir.join("system")).unwrap(), 1).is_err());
        fs::remove_dir_all(dir).unwrap();
    }
}

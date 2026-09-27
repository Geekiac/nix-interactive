//! Nix profiles: a `<name>` symlink pointing at one of its `<name>-<N>-link` generations.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

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

/// All generation links of `profile`, sorted by generation number.
pub fn generations(profile: &Path) -> Result<Vec<(u64, PathBuf)>> {
    let name = profile
        .file_name()
        .and_then(|n| n.to_str())
        .with_context(|| format!("invalid profile path {}", profile.display()))?;
    let dir = profile.parent().unwrap_or(Path::new("."));
    let mut gens = Vec::new();
    for entry in fs::read_dir(dir).with_context(|| format!("cannot read {}", dir.display()))? {
        let entry = entry?;
        if let Some(n) = entry
            .file_name()
            .to_str()
            .and_then(|f| generation_number(f, name))
        {
            gens.push((n, entry.path()));
        }
    }
    gens.sort_by_key(|(n, _)| *n);
    Ok(gens)
}

/// The generation `profile` currently points at.
pub fn current_generation(profile: &Path) -> Result<u64> {
    let target = fs::read_link(profile)
        .with_context(|| format!("{} is not a profile symlink", profile.display()))?;
    let name = profile.file_name().and_then(|n| n.to_str()).unwrap_or("");
    target
        .file_name()
        .and_then(|f| f.to_str())
        .and_then(|f| generation_number(f, name))
        .with_context(|| {
            format!(
                "{} points at {}, not a generation link",
                profile.display(),
                target.display()
            )
        })
}

/// `(previous, current)` generation links: the current one and the newest one before it
/// (numbers can have gaps after garbage collection).
pub fn previous_and_current(profile: &Path) -> Result<(PathBuf, PathBuf)> {
    let current = current_generation(profile)?;
    let gens = generations(profile)?;
    let Some(pos) = gens.iter().position(|(n, _)| *n == current) else {
        bail!("generation {current} of {} not found", profile.display());
    };
    if pos == 0 {
        bail!("{} has no generation before {current}", profile.display());
    }
    Ok((gens[pos - 1].1.clone(), gens[pos].1.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn parses_generation_numbers() {
        assert_eq!(generation_number("system-43-link", "system"), Some(43));
        assert_eq!(generation_number("system-43-link", "home"), None);
        assert_eq!(generation_number("system-foo-43-link", "system"), None);
        assert_eq!(generation_number("system", "system"), None);
    }

    #[test]
    fn finds_previous_generation_across_gaps() {
        let dir = std::env::temp_dir().join(format!("nixi-profile-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        for n in [3, 7, 9] {
            symlink("/nix/store/x", dir.join(format!("system-{n}-link"))).unwrap();
        }
        symlink("/nix/store/x", dir.join("other-8-link")).unwrap();
        symlink("system-9-link", dir.join("system")).unwrap();

        let (prev, cur) = previous_and_current(&dir.join("system")).unwrap();
        assert_eq!(
            (prev, cur),
            (dir.join("system-7-link"), dir.join("system-9-link"))
        );

        fs::remove_file(dir.join("system")).unwrap();
        symlink("system-3-link", dir.join("system")).unwrap();
        assert!(previous_and_current(&dir.join("system")).is_err());
        fs::remove_dir_all(dir).unwrap();
    }
}

//! Deleting profile generations with `nix-env --delete-generations`, shared by `nixi delete`
//! and the viewer's `D` key. Deleting only removes the profile link; the space comes back at
//! the next garbage collection.

use std::env;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::sources::Generation;

/// The profile a generation link belongs to: `/p/system-40-link` -> `/p/system`.
pub fn profile_of(link: &Path) -> Option<PathBuf> {
    let name = link.file_name()?.to_str()?;
    let (profile, number) = name.strip_suffix("-link")?.rsplit_once('-')?;
    number.parse::<u64>().ok()?;
    Some(link.with_file_name(profile))
}

/// Why a generation must not be deleted, if it mustn't.
pub fn refusal(g: &Generation) -> Option<String> {
    if g.current {
        return Some(format!(
            "generation {} is the current one; switch to another generation before deleting it",
            g.number
        ));
    }
    if profile_of(&g.path).is_none() {
        return Some(format!("{} is not a profile generation", g.path.display()));
    }
    None
}

/// Things worth knowing before deleting a generation.
pub fn warnings(g: &Generation) -> Vec<String> {
    let mut warnings = Vec::new();
    let booted = fs::canonicalize("/run/booted-system").ok();
    if booted.as_deref().and_then(Path::to_str) == Some(g.store_path.as_str()) {
        warnings.push(
            "This is the generation the machine booted into; it stays in use until reboot."
                .to_owned(),
        );
    }
    if profile_of(&g.path).is_some_and(|p| p.file_name().is_some_and(|n| n == "system")) {
        warnings.push("The boot menu keeps this entry until the next nixos-rebuild.".to_owned());
    }
    warnings
}

fn current_uid() -> Option<u32> {
    // Linux: /proc/self is owned by the process's user. Elsewhere, ask `id`.
    if let Ok(meta) = fs::metadata("/proc/self") {
        return Some(meta.uid());
    }
    let out = Command::new("id").arg("-u").output().ok()?;
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

/// Whether deleting needs root: the profile's directory belongs to someone else (the
/// system profile belongs to root). When unsure, try without and let nix-env explain.
pub fn needs_sudo(profile: &Path) -> bool {
    let owner = profile
        .parent()
        .and_then(|d| fs::metadata(d).ok())
        .map(|m| m.uid());
    matches!((owner, current_uid()), (Some(owner), Some(me)) if owner != me)
}

/// `nix-env` resolved on PATH, so `sudo` (with its own secure_path) runs the same one.
fn nix_env() -> PathBuf {
    env::var_os("PATH")
        .and_then(|paths| {
            env::split_paths(&paths)
                .map(|dir| dir.join("nix-env"))
                .find(|p| p.is_file())
        })
        .unwrap_or_else(|| PathBuf::from("nix-env"))
}

/// The command line that deletes `numbers` from `profile`, as program and arguments.
pub fn command_line(profile: &Path, numbers: &[u64], sudo: bool) -> Vec<String> {
    let mut line = Vec::new();
    if sudo {
        line.push("sudo".to_owned());
    }
    line.push(nix_env().display().to_string());
    line.push("--profile".to_owned());
    line.push(profile.display().to_string());
    line.push("--delete-generations".to_owned());
    line.extend(numbers.iter().map(u64::to_string));
    line
}

/// A runnable command for [`command_line`], inheriting the terminal (sudo may prompt).
pub fn command(profile: &Path, numbers: &[u64], sudo: bool) -> Command {
    let line = command_line(profile, numbers, sudo);
    let mut cmd = Command::new(&line[0]);
    cmd.args(&line[1..]);
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;

    fn generation(link: &str, current: bool) -> Generation {
        Generation {
            number: 40,
            last_number: 40,
            path: PathBuf::from(link),
            store_path: "/nix/store/x-nixos-system".into(),
            created: None,
            current,
        }
    }

    #[test]
    fn finds_the_profile_of_a_link() {
        let profile = |p: &str| profile_of(Path::new(p));
        assert_eq!(
            profile("/nix/var/nix/profiles/system-40-link"),
            Some(PathBuf::from("/nix/var/nix/profiles/system"))
        );
        assert_eq!(
            profile("/home/a/.local/state/nix/profiles/home-manager-7-link"),
            Some(PathBuf::from(
                "/home/a/.local/state/nix/profiles/home-manager"
            ))
        );
        assert_eq!(profile("/nix/store/abc-home-manager-generation"), None);
        assert_eq!(profile("/p/system-x-link"), None);
    }

    #[test]
    fn refuses_current_and_non_profile_generations() {
        assert!(refusal(&generation("/nix/var/nix/profiles/system-40-link", false)).is_none());
        let current = refusal(&generation("/nix/var/nix/profiles/system-40-link", true));
        assert!(current.unwrap().contains("current"));
        let embedded = refusal(&generation("/nix/store/abc-home-manager-generation", false));
        assert!(embedded.unwrap().contains("not a profile generation"));
    }

    #[test]
    fn warns_about_the_boot_menu_for_system_profiles() {
        let system = warnings(&generation("/nix/var/nix/profiles/system-40-link", false));
        assert!(system.iter().any(|w| w.contains("boot menu")));
        let other = warnings(&generation("/p/home-manager-3-link", false));
        assert!(!other.iter().any(|w| w.contains("boot menu")));
    }

    #[test]
    fn builds_the_nix_env_command() {
        let line = command_line(Path::new("/p/system"), &[38, 40], true);
        assert_eq!(line[0], "sudo");
        assert!(line[1].ends_with("nix-env"));
        assert_eq!(
            &line[2..],
            ["--profile", "/p/system", "--delete-generations", "38", "40"]
        );
        let line = command_line(Path::new("/p/system"), &[38], false);
        assert!(line[0].ends_with("nix-env"));
    }
}

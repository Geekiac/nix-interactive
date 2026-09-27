//! `~/.config/nix-interactive/config.toml`: defaults for the command-line options, and extra
//! profiles to browse. Command-line flags and environment variables take precedence.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

pub const EXAMPLE: &str = r#"# ~/.config/nix-interactive/config.toml

# Configuration repo, to link generations to commits (like --repo / NIXI_REPO).
repo = "~/repos/nix-config"

# Profile to browse (like --profile). Default: /nix/var/nix/profiles/system
# profile = "/nix/var/nix/profiles/system"

# User whose embedded home-manager generations to show (like --user). Default: $USER
# user = "alice"

# Extra profiles, each shown in its own tab. `--profile <name>` selects one on the
# command line, e.g. `nixi list --profile hm`.
# [[profiles]]
# name = "hm"
# path = "~/.local/state/nix/profiles/home-manager"
"#;

#[derive(Debug, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub repo: Option<PathBuf>,
    pub profile: Option<PathBuf>,
    pub user: Option<String>,
    #[serde(default)]
    pub profiles: Vec<NamedProfile>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct NamedProfile {
    pub name: String,
    pub path: PathBuf,
}

/// `$XDG_CONFIG_HOME/nix-interactive/config.toml`, falling back to `~/.config`.
pub fn default_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("nix-interactive").join("config.toml"))
}

/// Expands a leading `~/`.
fn expand_home(path: &Path) -> PathBuf {
    match (path.strip_prefix("~"), std::env::var_os("HOME")) {
        (Ok(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ => path.to_owned(),
    }
}

impl Config {
    pub fn parse(text: &str) -> Result<Self> {
        let mut config: Self = toml::from_str(text)?;
        for path in config
            .repo
            .iter_mut()
            .chain(config.profile.iter_mut())
            .chain(config.profiles.iter_mut().map(|p| &mut p.path))
        {
            *path = expand_home(path);
        }
        Ok(config)
    }

    /// Loads `path` if given (it must exist), else the default location if it exists.
    pub fn load(path: Option<&Path>) -> Result<Self> {
        let (path, required) = match path {
            Some(path) => (path.to_owned(), true),
            None => match default_path() {
                Some(path) => (path, false),
                None => return Ok(Self::default()),
            },
        };
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if !required && e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default())
            }
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        Self::parse(&text).with_context(|| format!("in {}", path.display()))
    }

    /// A configured profile's path by name, or `arg` itself as a path.
    pub fn resolve_profile(&self, arg: &Path) -> PathBuf {
        self.profiles
            .iter()
            .find(|p| Path::new(&p.name) == arg)
            .map_or_else(|| arg.to_owned(), |p| p.path.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_example() {
        let config = Config::parse(EXAMPLE).unwrap();
        let home = PathBuf::from(std::env::var_os("HOME").unwrap());
        assert_eq!(config.repo, Some(home.join("repos/nix-config")));
        assert_eq!(config.profile, None);
        assert!(config.profiles.is_empty());
    }

    #[test]
    fn parses_profiles_and_rejects_typos() {
        let config = Config::parse(
            r#"
            user = "bob"
            [[profiles]]
            name = "hm"
            path = "/p/home-manager"
            "#,
        )
        .unwrap();
        assert_eq!(config.user.as_deref(), Some("bob"));
        assert_eq!(
            config.resolve_profile(Path::new("hm")),
            Path::new("/p/home-manager")
        );
        assert_eq!(
            config.resolve_profile(Path::new("/x/system")),
            Path::new("/x/system")
        );

        let err = Config::parse("repos = \"/x\"").unwrap_err();
        assert!(err.to_string().contains("unknown field"), "{err}");
    }

    #[test]
    fn missing_default_file_is_fine_but_explicit_one_is_not() {
        assert!(Config::load(Some(Path::new("/nonexistent/config.toml"))).is_err());
    }
}

mod closure;
mod config;
mod diff;
mod git;
mod render;
mod sources;
mod store_path;
mod ui;

use std::collections::HashMap;
use std::io::{self, IsTerminal, StdoutLock, Write};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::closure::{Cache, Closure};
use crate::config::Config;
use crate::git::{Link, Repo};
use crate::sources::{profile::SYSTEM_PROFILE, Generation, Source};

/// Interactive historical diff viewer for Nix generations.
///
/// With no subcommand, opens the interactive viewer: generations on the left, what changed
/// on the right. Press `?` inside for keys.
#[derive(Parser)]
#[command(
    version,
    after_help = "Examples:
  nixi                          browse system and home-manager generations
  nixi --repo ~/nix-config      ...linked to the commits they were built from
  nixi diff                     what the last switch changed (-1:0)
  nixi diff -2:-1               ...and the switch before that
  nixi diff 40:43               compare generations 40 and 43
  nixi diff 0:./result          what switching to ./result would change
  nixi diff --home              what the last home-manager change changed
  nixi list                     list generations

Settings can live in ~/.config/nix-interactive/config.toml (see `nixi config --example`)."
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    #[command(flatten)]
    source: SourceArgs,

    /// Extra closure to browse in the viewer's "paths" tab, e.g. `./result`. Repeatable.
    #[arg(long = "path", value_name = "PATH")]
    paths: Vec<PathBuf>,

    /// When to use ANSI colors.
    #[arg(long, value_enum, default_value_t = ColorMode::Auto, global = true)]
    color: ColorMode,

    /// Don't read or write the closure cache.
    #[arg(long, global = true)]
    no_cache: bool,

    /// Config file [default: ~/.config/nix-interactive/config.toml].
    #[arg(long, global = true, value_name = "PATH")]
    config: Option<PathBuf>,
}

#[derive(Subcommand)]
enum Command {
    /// Print package-level differences between two closures (like `nvd diff`).
    ///
    /// With no arguments, compares the current generation with the one before it.
    Diff {
        /// Sides to compare [default: -1:0, the previous generation vs the current one].
        ///
        /// Each side is `0` (current) or `-N` (N generations before it), a positive
        /// generation number, or a path: a profile link, `./result`, or a store path.
        /// Examples: `-2:-1`, `40:43`, `0:./result`.
        #[arg(value_name = "OLD:NEW", allow_hyphen_values = true)]
        range: Option<String>,
    },
    /// List generations, oldest first (`*` marks the current one).
    List,
    /// Show the config file location and the settings in effect.
    Config {
        /// Print an example config file instead.
        #[arg(long)]
        example: bool,
    },
}

#[derive(Args)]
struct SourceArgs {
    /// Profile whose generations are used, or the name of one from the config file
    /// (with --home: the system profile to look in) [default: /nix/var/nix/profiles/system].
    #[arg(long, global = true)]
    profile: Option<PathBuf>,

    /// Use home-manager generations embedded in system generations (the NixOS module).
    /// Generation numbers then refer to system generations. For standalone home-manager,
    /// pass its profile with --profile instead.
    #[arg(long, global = true)]
    home: bool,

    /// User whose home-manager generations to use [default: $USER].
    #[arg(long, global = true)]
    user: Option<String>,

    /// Git repo of your configuration (e.g. a NixOS flake), to link generations to commits.
    #[arg(long, env = "NIXI_REPO", global = true)]
    repo: Option<PathBuf>,
}

impl SourceArgs {
    /// Fills unset options from the config file; flags and NIXI_REPO win.
    fn apply(&mut self, config: &Config) {
        let profile = self
            .profile
            .take()
            .or_else(|| config.profile.clone())
            .unwrap_or_else(|| PathBuf::from(SYSTEM_PROFILE));
        self.profile = Some(config.resolve_profile(&profile));
        self.user = self.user.take().or_else(|| config.user.clone());
        self.repo = self.repo.take().or_else(|| config.repo.clone());
    }

    fn profile(&self) -> &Path {
        self.profile.as_deref().unwrap_or(Path::new(SYSTEM_PROFILE))
    }

    fn user(&self) -> String {
        self.user
            .clone()
            .or_else(|| std::env::var("USER").ok())
            .unwrap_or_default()
    }

    fn source(&self) -> Source {
        if self.home {
            Source::Home {
                system_profile: self.profile().to_owned(),
                user: self.user(),
            }
        } else {
            Source::Profile(self.profile().to_owned())
        }
    }

    /// Commit links by generation number. Home-manager generations are numbered by system
    /// generation, so they're linked through the system profile.
    fn links(&self, repo: &Repo, gens: &[Generation]) -> Result<HashMap<u64, Link>> {
        if self.home {
            Ok(repo.links(&sources::profile::generations(self.profile())?))
        } else {
            Ok(repo.links(gens))
        }
    }

    fn label(&self, g: &Generation) -> String {
        if self.home {
            format!(
                "{} (system generation {})",
                g.path.display(),
                g.number_label()
            )
        } else {
            g.path.display().to_string()
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum ColorMode {
    Auto,
    Always,
    Never,
}

impl ColorMode {
    fn enabled(self) -> bool {
        match self {
            Self::Always => true,
            Self::Never => false,
            Self::Auto => io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none(),
        }
    }
}

/// Writes to stdout, treating a closed pipe (e.g. `| head`) as success.
fn emit(write: impl FnOnce(&mut StdoutLock) -> io::Result<()>) -> Result<()> {
    let mut out = io::stdout().lock();
    match write(&mut out).and_then(|()| out.flush()) {
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        result => Ok(result?),
    }
}

/// A diff side: what to load, and how to name it in the output.
struct Side {
    path: PathBuf,
    label: String,
    /// Generation number, when the side is a generation (for commit links).
    number: Option<u64>,
}

impl Side {
    fn of(g: &Generation, source: &SourceArgs) -> Self {
        Self {
            path: g.path.clone(),
            label: source.label(g),
            number: Some(g.number),
        }
    }
}

/// One side of an `OLD:NEW` range.
#[derive(Debug, PartialEq)]
enum SideSpec {
    /// Steps back from the current generation (`0`, `-1`, …).
    Back(u64),
    /// A generation number.
    Number(u64),
    Path(String),
}

impl SideSpec {
    fn parse(s: &str) -> Result<Self> {
        if s.is_empty() {
            bail!("empty side in range; expected OLD:NEW, e.g. -1:0");
        }
        Ok(match s.parse::<i64>() {
            Ok(n) if n <= 0 => Self::Back(n.unsigned_abs()),
            Ok(n) => Self::Number(n.unsigned_abs()),
            Err(_) => Self::Path(s.to_owned()),
        })
    }

    fn resolve(&self, source: &SourceArgs, gens: &[Generation]) -> Result<Side> {
        match self {
            Self::Back(back) => Ok(Side::of(sources::relative(gens, *back)?, source)),
            Self::Number(n) => Ok(Side::of(sources::find(gens, *n)?, source)),
            Self::Path(path) => Ok(Side {
                path: PathBuf::from(path),
                label: path.clone(),
                number: None,
            }),
        }
    }
}

fn parse_range(range: &str) -> Result<(SideSpec, SideSpec)> {
    let (old, new) = range
        .split_once(':')
        .with_context(|| format!("expected OLD:NEW, e.g. -1:0 (got {range:?})"))?;
    Ok((SideSpec::parse(old)?, SideSpec::parse(new)?))
}

/// Plain-text "what changed in the config" section for two linked generations.
fn write_commits(out: &mut impl Write, repo: &Repo, old: &Link, new: &Link) -> Result<()> {
    writeln!(out, "Commits: {} -> {}", old.label(), new.label())?;
    if old.commit.hash == new.commit.hash {
        writeln!(out, "  (same commit)")?;
        return Ok(());
    }
    if old.commit.time > new.commit.time {
        writeln!(out, "  (going back in time; commits undone:)")?;
    }
    for subject in repo.subjects_between(&old.commit, &new.commit)? {
        writeln!(out, "  {subject}")?;
    }
    Ok(())
}

fn run_diff(
    range: Option<String>,
    source: &SourceArgs,
    cache: Option<&Cache>,
    color: bool,
) -> Result<()> {
    let (old, new) = parse_range(range.as_deref().unwrap_or("-1:0"))?;
    let needs_gens = [&old, &new].iter().any(|s| !matches!(s, SideSpec::Path(_)));
    let gens = if needs_gens {
        source.source().generations(cache)?
    } else {
        Vec::new()
    };
    let (left, right) = (old.resolve(source, &gens)?, new.resolve(source, &gens)?);

    let d = diff::diff(
        &Closure::load(&left.path, cache)?,
        &Closure::load(&right.path, cache)?,
    );
    let commits = match (&source.repo, left.number, right.number) {
        (Some(repo), Some(old), Some(new)) => {
            let repo = Repo::load(repo)?;
            let links = source.links(&repo, &gens)?;
            match (links.get(&old), links.get(&new)) {
                (Some(old), Some(new)) => {
                    let mut text = Vec::new();
                    write_commits(&mut text, &repo, old, new)?;
                    text
                }
                _ => b"Commits: no commit found before one of the generations\n".to_vec(),
            }
        }
        _ => Vec::new(),
    };
    emit(|out| {
        render::write_diff(out, &left.label, &right.label, &d, color)?;
        out.write_all(&commits)
    })
}

fn run_list(source: &SourceArgs, cache: Option<&Cache>) -> Result<()> {
    let gens = source.source().generations(cache)?;
    let links = match &source.repo {
        Some(repo) => source.links(&Repo::load(repo)?, &gens)?,
        None => HashMap::new(),
    };
    let commit = |g: &Generation| -> String {
        match links.get(&g.number) {
            Some(link) => {
                let subject: String = link.commit.subject.chars().take(40).collect();
                format!("{} {subject}", link.label())
            }
            None => String::new(),
        }
    };
    let commit_width = gens
        .iter()
        .map(|g| commit(g).chars().count())
        .max()
        .unwrap_or(0);
    // The commit column (and its gap) only appears when there are links.
    let commit_column = |text: &str| {
        if links.is_empty() {
            String::new()
        } else {
            format!("{text:commit_width$}  ")
        }
    };
    let width = gens
        .iter()
        .map(|g| g.number_label().len())
        .max()
        .unwrap_or(0)
        .max(3);
    emit(|out| {
        writeln!(
            out,
            "   {:>width$}  {:16}  {}STORE PATH",
            "GEN",
            "CREATED",
            commit_column("COMMIT")
        )?;
        for g in &gens {
            writeln!(
                out,
                "{}  {:>width$}  {:16}  {}{}",
                if g.current { '*' } else { ' ' },
                g.number_label(),
                g.created_label(),
                commit_column(&commit(g)),
                g.store_path,
            )?;
        }
        Ok(())
    })
}

fn run_config(path: Option<&Path>, source: &SourceArgs, config: &Config) -> Result<()> {
    let path = path.map(Path::to_owned).or_else(config::default_path);
    emit(|out| {
        match &path {
            Some(path) if path.exists() => writeln!(out, "config file: {}", path.display())?,
            Some(path) => writeln!(out, "config file: {} (not found)", path.display())?,
            None => writeln!(out, "config file: none ($HOME is unset)")?,
        }
        writeln!(out, "profile: {}", source.profile().display())?;
        writeln!(out, "user: {}", source.user())?;
        match &source.repo {
            Some(repo) => writeln!(out, "repo: {}", repo.display())?,
            None => writeln!(out, "repo: (none)")?,
        }
        for p in &config.profiles {
            writeln!(out, "extra profile {}: {}", p.name, p.path.display())?;
        }
        Ok(())
    })
}

fn main() -> Result<()> {
    let mut cli = Cli::parse();
    let config = Config::load(cli.config.as_deref())?;
    cli.source.apply(&config);
    let cache = (!cli.no_cache)
        .then(Cache::default_dir)
        .flatten()
        .map(Cache::new);

    match cli.command {
        Some(Command::Diff { range }) => {
            run_diff(range, &cli.source, cache.as_ref(), cli.color.enabled())
        }
        Some(Command::List) => run_list(&cli.source, cache.as_ref()),
        Some(Command::Config { example: true }) => {
            emit(|out| out.write_all(config::EXAMPLE.as_bytes()))
        }
        Some(Command::Config { example: false }) => {
            run_config(cli.config.as_deref(), &cli.source, &config)
        }
        None => ui::run(ui::Options {
            user: cli.source.user(),
            repo: cli.source.repo.clone(),
            profile: cli.source.profile().to_owned(),
            extra_profiles: config.profiles,
            home_first: cli.source.home,
            paths: cli.paths,
            cache,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ranges() {
        use SideSpec::*;
        assert_eq!(parse_range("-1:0").unwrap(), (Back(1), Back(0)));
        assert_eq!(parse_range("-3:-2").unwrap(), (Back(3), Back(2)));
        assert_eq!(parse_range("40:43").unwrap(), (Number(40), Number(43)));
        assert_eq!(
            parse_range("0:./result").unwrap(),
            (Back(0), Path("./result".into()))
        );
        assert_eq!(
            parse_range("/nix/store/a-x:/nix/store/b-y").unwrap(),
            (Path("/nix/store/a-x".into()), Path("/nix/store/b-y".into()))
        );
        assert!(parse_range("-1").is_err());
        assert!(parse_range(":0").is_err());
    }

    #[test]
    fn range_with_leading_dash_is_not_a_flag() {
        let cli = Cli::try_parse_from(["nixi", "diff", "-2:-1", "--home"]).unwrap();
        assert!(matches!(cli.command, Some(Command::Diff { range: Some(r) }) if r == "-2:-1"));
        assert!(cli.source.home);
    }
}

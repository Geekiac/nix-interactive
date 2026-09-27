mod closure;
mod diff;
mod render;
mod sources;
mod store_path;
mod ui;

use std::io::{self, IsTerminal, StdoutLock, Write};
use std::path::PathBuf;

use anyhow::Result;
use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::closure::{Cache, Closure};
use crate::sources::{profile::SYSTEM_PROFILE, Generation, Source};

/// Interactive historical diff viewer for Nix generations.
///
/// With no subcommand, opens the interactive viewer.
#[derive(Parser)]
#[command(version)]
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
}

#[derive(Subcommand)]
enum Command {
    /// Print package-level differences between two closures (like `nvd diff`).
    ///
    /// With no arguments, compares the current generation with the one before it.
    Diff {
        /// Old side: a generation number, profile link, `./result`, or store path.
        #[arg(requires = "right")]
        left: Option<String>,
        /// New side: a generation number, profile link, `./result`, or store path.
        right: Option<String>,
    },
    /// List generations, oldest first (`*` marks the current one).
    List,
}

#[derive(Args)]
struct SourceArgs {
    /// Profile whose generations are used (with --home: the system profile to look in).
    #[arg(long, default_value = SYSTEM_PROFILE, global = true)]
    profile: PathBuf,

    /// Use home-manager generations embedded in system generations (the NixOS module).
    /// Generation numbers then refer to system generations. For standalone home-manager,
    /// pass its profile with --profile instead.
    #[arg(long, global = true)]
    home: bool,

    /// User whose home-manager generations to use [default: $USER].
    #[arg(long, global = true)]
    user: Option<String>,
}

impl SourceArgs {
    fn user(&self) -> String {
        self.user
            .clone()
            .or_else(|| std::env::var("USER").ok())
            .unwrap_or_default()
    }

    fn source(&self) -> Source {
        if self.home {
            Source::Home {
                system_profile: self.profile.clone(),
                user: self.user(),
            }
        } else {
            Source::Profile(self.profile.clone())
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
}

fn resolve_side(arg: &str, source: &SourceArgs, gens: &[Generation]) -> Result<Side> {
    match arg.parse::<u64>() {
        Ok(n) => {
            let g = sources::find(gens, n)?;
            Ok(Side {
                path: g.path.clone(),
                label: source.label(g),
            })
        }
        Err(_) => Ok(Side {
            path: PathBuf::from(arg),
            label: arg.to_owned(),
        }),
    }
}

fn run_diff(
    left: Option<String>,
    right: Option<String>,
    source: &SourceArgs,
    cache: Option<&Cache>,
    color: bool,
) -> Result<()> {
    let args: Vec<&str> = left.iter().chain(&right).map(String::as_str).collect();
    let needs_gens = args.is_empty() || args.iter().any(|a| a.parse::<u64>().is_ok());
    let gens = if needs_gens {
        source.source().generations(cache)?
    } else {
        Vec::new()
    };
    let (left, right) = match args[..] {
        [left, right] => (
            resolve_side(left, source, &gens)?,
            resolve_side(right, source, &gens)?,
        ),
        _ => {
            let (prev, cur) = sources::previous_and_current(&gens)?;
            let side = |g: &Generation| Side {
                path: g.path.clone(),
                label: source.label(g),
            };
            (side(prev), side(cur))
        }
    };

    let d = diff::diff(
        &Closure::load(&left.path, cache)?,
        &Closure::load(&right.path, cache)?,
    );
    emit(|out| render::write_diff(out, &left.label, &right.label, &d, color))
}

fn run_list(source: &SourceArgs, cache: Option<&Cache>) -> Result<()> {
    let gens = source.source().generations(cache)?;
    let width = gens
        .iter()
        .map(|g| g.number_label().len())
        .max()
        .unwrap_or(0)
        .max(3);
    emit(|out| {
        writeln!(out, "   {:>width$}  {:16}  STORE PATH", "GEN", "CREATED")?;
        for g in &gens {
            writeln!(
                out,
                "{}  {:>width$}  {:16}  {}",
                if g.current { '*' } else { ' ' },
                g.number_label(),
                g.created_label(),
                g.store_path,
            )?;
        }
        Ok(())
    })
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let cache = (!cli.no_cache)
        .then(Cache::default_dir)
        .flatten()
        .map(Cache::new);

    match cli.command {
        Some(Command::Diff { left, right }) => run_diff(
            left,
            right,
            &cli.source,
            cache.as_ref(),
            cli.color.enabled(),
        ),
        Some(Command::List) => run_list(&cli.source, cache.as_ref()),
        None => ui::run(ui::Options {
            user: cli.source.user(),
            profile: cli.source.profile,
            home_first: cli.source.home,
            paths: cli.paths,
            cache,
        }),
    }
}

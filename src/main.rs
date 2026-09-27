mod closure;
mod diff;
mod profile;
mod render;
mod store_path;

use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};

use crate::closure::{Cache, Closure};

/// Interactive historical diff viewer for Nix generations.
#[derive(Parser)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Command,

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
    /// With no paths, compares the profile's current generation with the one before it.
    Diff {
        /// Old side: a profile link, `./result`, or store path.
        #[arg(requires = "right")]
        left: Option<PathBuf>,
        /// New side: a profile link, `./result`, or store path.
        right: Option<PathBuf>,
        /// Profile whose generations are compared when no paths are given.
        #[arg(long, default_value = profile::SYSTEM_PROFILE, conflicts_with = "left")]
        profile: PathBuf,
    },
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

fn main() -> Result<()> {
    let cli = Cli::parse();
    let cache = (!cli.no_cache)
        .then(Cache::default_dir)
        .flatten()
        .map(Cache::new);

    match cli.command {
        Command::Diff {
            left,
            right,
            profile,
        } => {
            let (left, right) = match (left, right) {
                (Some(left), Some(right)) => (left, right),
                _ => profile::previous_and_current(&profile)?,
            };
            let left_closure = Closure::load(&left, cache.as_ref())?;
            let right_closure = Closure::load(&right, cache.as_ref())?;
            let d = diff::diff(&left_closure, &right_closure);
            let mut out = io::stdout().lock();
            let written = render::write_diff(
                &mut out,
                &left.display().to_string(),
                &right.display().to_string(),
                &d,
                cli.color.enabled(),
            )
            .and_then(|()| out.flush());
            match written {
                // The reader went away (e.g. `| head`); that's not an error worth reporting.
                Err(e) if e.kind() == io::ErrorKind::BrokenPipe => {}
                result => result?,
            }
        }
    }
    Ok(())
}

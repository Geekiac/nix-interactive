//! Garbage collection with `nix-store --gc`, reporting what it freed. Shared by `nixi gc`,
//! `nixi delete`'s follow-up, and the viewer's `C` key.
//!
//! This frees *every* store path no GC root uses, not only what a just-deleted generation
//! held, so the reported total can include unrelated garbage.

use std::io::{self, BufRead, BufReader};
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};

use crate::delete::nix_tool;

/// Nix's own summary of a collection, e.g. `5106 store paths deleted, 15661.3 MiB freed`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Freed {
    pub paths: u64,
    /// The amount as Nix printed it, e.g. `15661.3 MiB`.
    pub amount: String,
}

impl Freed {
    pub fn summary(&self) -> String {
        format!("{} freed ({} store paths deleted)", self.amount, self.paths)
    }
}

/// Parses `N store paths deleted, X UNIT freed` out of `nix-store --gc`'s stdout.
pub fn parse_summary(stdout: &str) -> Option<Freed> {
    stdout.lines().rev().find_map(|line| {
        let (paths, rest) = line.trim().split_once(" store paths deleted, ")?;
        let amount = rest.strip_suffix(" freed")?;
        Some(Freed {
            paths: paths.parse().ok()?,
            amount: amount.to_owned(),
        })
    })
}

pub fn command_line() -> Vec<String> {
    vec![
        nix_tool("nix-store").display().to_string(),
        "--gc".to_owned(),
    ]
}

/// Runs the collection on the current terminal: progress (stderr) shows as it happens,
/// and stdout is echoed while being read for the summary.
pub fn run() -> Result<Freed> {
    let line = command_line();
    let mut child = Command::new(&line[0])
        .args(&line[1..])
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| format!("couldn't run {}", line[0]))?;
    let mut stdout = String::new();
    for text in BufReader::new(child.stdout.take().context("nix-store stdout")?).lines() {
        let text = text?;
        println!("{text}");
        stdout.push_str(&text);
        stdout.push('\n');
    }
    let status = child.wait()?;
    if !status.success() {
        bail!("nix-store --gc failed ({status})");
    }
    parse_summary(&stdout).context("nix-store --gc finished but printed no summary")
}

/// Asks a yes/no question on the terminal; anything but `y`/`yes` means no.
pub fn ask(question: &str) -> Result<bool> {
    use std::io::Write;
    print!("{question} [y/N] ");
    io::stdout().flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    Ok(matches!(answer.trim().to_lowercase().as_str(), "y" | "yes"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nix_summaries() {
        let out = "1 store paths deleted, 0.0 KiB freed\n";
        assert_eq!(
            parse_summary(out),
            Some(Freed {
                paths: 1,
                amount: "0.0 KiB".into()
            })
        );
        let out = "some other line\n5106 store paths deleted, 15661.32 MiB freed\n";
        let freed = parse_summary(out).unwrap();
        assert_eq!(freed.paths, 5106);
        assert_eq!(
            freed.summary(),
            "15661.32 MiB freed (5106 store paths deleted)"
        );
        assert_eq!(parse_summary("nothing to see\n"), None);
    }
}

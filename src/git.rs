//! Linking generations to commits of the configuration repo (e.g. a NixOS flake).
//!
//! Exact when the generation records `system.configurationRevision`; otherwise the newest
//! commit made before the generation, cross-checked against the nixpkgs revision pinned in
//! that commit's `flake.lock`.

use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::UNIX_EPOCH;

use anyhow::{bail, Context, Result};
use serde_json::Value;

use crate::sources::Generation;

/// Most recent commits considered; older generations than this rarely survive GC anyway.
const MAX_COMMITS: usize = 5000;

#[derive(Debug, Clone)]
pub struct Commit {
    pub hash: String,
    /// Committer time, seconds since the epoch.
    pub time: i64,
    pub subject: String,
    /// nixpkgs revision pinned by this commit's `flake.lock`, if any.
    pub nixpkgs: Option<String>,
}

impl Commit {
    pub fn short(&self) -> &str {
        &self.hash[..self.hash.len().min(7)]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confidence {
    /// The generation records this commit (`system.configurationRevision`).
    Exact,
    /// Recorded, but built from a dirty tree on top of this commit.
    Dirty,
    /// Newest commit before the generation, and its flake.lock pins the same nixpkgs.
    Likely,
    /// Newest commit before the generation; nothing else confirms it.
    Guess,
}

impl Confidence {
    pub fn marker(self) -> &'static str {
        match self {
            Self::Exact => "=",
            Self::Dirty => "+",
            Self::Likely => "≈",
            Self::Guess => "?",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Link {
    pub commit: Commit,
    pub confidence: Confidence,
}

impl Link {
    /// `≈9465368`, `=9465368`, …
    pub fn label(&self) -> String {
        format!("{}{}", self.confidence.marker(), self.commit.short())
    }
}

/// What a generation says about where it came from.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct GenInfo {
    pub time: Option<i64>,
    /// Full or abbreviated nixpkgs revision.
    pub nixpkgs: Option<String>,
    /// `system.configurationRevision`, possibly ending in `-dirty`.
    pub revision: Option<String>,
}

fn json_field(text: &str, field: &str) -> Option<String> {
    let start = text.find(&format!("\"{field}\":\""))? + field.len() + 4;
    let len = text[start..].find('"')?;
    Some(text[start..start + len].to_owned()).filter(|v| !v.is_empty())
}

/// The trailing `.<hex>` of a NixOS version, e.g. `26.11.20260926.e158d9e` -> `e158d9e`.
fn nixpkgs_from_name(store_path: &str) -> Option<String> {
    let (_, version) = store_path.rsplit_once('-')?;
    let rev = version.rsplit('.').next()?;
    (rev.len() >= 7 && rev.chars().all(|c| c.is_ascii_hexdigit())).then(|| rev.to_owned())
}

impl GenInfo {
    /// Reads the generation's `nixos-version` script (which embeds the nixpkgs revision and
    /// any configuration revision) and its creation time.
    pub fn of(g: &Generation) -> Self {
        let script = fs::read_to_string(g.path.join("sw/bin/nixos-version")).unwrap_or_default();
        Self {
            time: g
                .created
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64),
            nixpkgs: json_field(&script, "nixpkgsRevision")
                .or_else(|| nixpkgs_from_name(&g.store_path)),
            revision: json_field(&script, "configurationRevision"),
        }
    }
}

/// nixpkgs revision locked by a flake.lock: the root's `nixpkgs` input.
fn locked_nixpkgs(lock: &Value) -> Option<String> {
    let nodes = lock.get("nodes")?;
    let root = lock.get("root")?.as_str()?;
    let input = nodes.get(root)?.get("inputs")?.get("nixpkgs")?.as_str()?;
    let rev = nodes.get(input)?.get("locked")?.get("rev")?.as_str()?;
    Some(rev.to_owned())
}

fn revs_match(a: &str, b: &str) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

pub struct Repo {
    pub path: PathBuf,
    /// Newest first.
    pub commits: Vec<Commit>,
}

fn git(repo: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(repo);
    cmd
}

/// Reads `<commit>:flake.lock` for every commit through one `git cat-file --batch`.
fn flake_locks(repo: &Path, commits: &[Commit]) -> Result<Vec<Option<Value>>> {
    let mut child = git(repo)
        .args(["cat-file", "--batch"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .context("failed to run git cat-file")?;
    let mut stdin = child.stdin.take().context("git stdin")?;
    let input: String = commits
        .iter()
        .map(|c| format!("{}:flake.lock\n", c.hash))
        .collect();
    // Write from another thread so a full stdout pipe can't deadlock us.
    let writer = thread::spawn(move || stdin.write_all(input.as_bytes()));

    let mut out = BufReader::new(child.stdout.take().context("git stdout")?);
    let mut locks = Vec::with_capacity(commits.len());
    let mut header = String::new();
    for _ in commits {
        header.clear();
        out.read_line(&mut header)?;
        // `<oid> blob <size>` or `<name> missing`
        let size = match header.trim_end().rsplit_once(' ') {
            Some((_, size)) if !header.trim_end().ends_with(" missing") => size.parse::<usize>()?,
            _ => {
                locks.push(None);
                continue;
            }
        };
        let mut body = vec![0; size + 1]; // content plus trailing newline
        out.read_exact(&mut body)?;
        locks.push(serde_json::from_slice(&body[..size]).ok());
    }
    writer.join().expect("writer thread panicked")?;
    child.wait()?;
    Ok(locks)
}

impl Repo {
    pub fn load(path: &Path) -> Result<Self> {
        let out = git(path)
            .args(["log", "-z", "--format=%H%x1f%ct%x1f%s"])
            .arg(format!("--max-count={MAX_COMMITS}"))
            .output()
            .context("failed to run git (is it installed?)")?;
        if !out.status.success() {
            bail!(
                "git log failed in {}: {}",
                path.display(),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        let text = String::from_utf8_lossy(&out.stdout);
        let mut commits: Vec<Commit> = text
            .split('\0')
            .filter_map(|record| {
                let mut fields = record.trim_start_matches('\n').splitn(3, '\x1f');
                Some(Commit {
                    hash: fields.next()?.to_owned(),
                    time: fields.next()?.parse().ok()?,
                    subject: fields.next().unwrap_or("").to_owned(),
                    nixpkgs: None,
                })
            })
            .collect();
        let locks = flake_locks(path, &commits)?;
        for (commit, lock) in commits.iter_mut().zip(locks) {
            commit.nixpkgs = lock.as_ref().and_then(locked_nixpkgs);
        }
        commits.sort_by_key(|c| std::cmp::Reverse(c.time));
        Ok(Self {
            path: path.to_owned(),
            commits,
        })
    }

    pub fn find(&self, rev: &str) -> Option<&Commit> {
        self.commits.iter().find(|c| c.hash.starts_with(rev))
    }

    pub fn correlate(&self, info: &GenInfo) -> Option<Link> {
        if let Some(rev) = &info.revision {
            let (rev, dirty) = match rev.strip_suffix("-dirty") {
                Some(rev) => (rev, true),
                None => (rev.as_str(), false),
            };
            if let Some(commit) = self.find(rev) {
                let confidence = if dirty {
                    Confidence::Dirty
                } else {
                    Confidence::Exact
                };
                return Some(Link {
                    commit: commit.clone(),
                    confidence,
                });
            }
        }
        let time = info.time?;
        let commit = self.commits.iter().find(|c| c.time <= time)?;
        let confirmed = match (&info.nixpkgs, &commit.nixpkgs) {
            (Some(built), Some(locked)) => revs_match(built, locked),
            _ => false,
        };
        Some(Link {
            commit: commit.clone(),
            confidence: if confirmed {
                Confidence::Likely
            } else {
                Confidence::Guess
            },
        })
    }

    /// Links for each generation number.
    pub fn links(&self, gens: &[Generation]) -> HashMap<u64, Link> {
        gens.iter()
            .filter_map(|g| Some((g.number, self.correlate(&GenInfo::of(g))?)))
            .collect()
    }

    /// `git log` arguments for the commits between two links, oldest side first. Returns
    /// `(args, reversed)`: reversed when `old` is newer, i.e. comparing backwards in time.
    pub fn log_args(&self, old: &Commit, new: &Commit, color: bool) -> (Vec<String>, bool) {
        let reversed = old.time > new.time;
        let (from, to) = if reversed { (new, old) } else { (old, new) };
        let args = vec![
            "-C".to_owned(),
            self.path.display().to_string(),
            "log".to_owned(),
            format!("--color={}", if color { "always" } else { "never" }),
            "--stat".to_owned(),
            "--format=%C(yellow)%h%C(reset) %C(dim)%ad%C(reset)  %s".to_owned(),
            "--date=format:%Y-%m-%d %H:%M".to_owned(),
            format!("{}..{}", from.hash, to.hash),
        ];
        (args, reversed)
    }

    /// Subjects of the commits between two links, newest first (for plain-text output).
    pub fn subjects_between(&self, old: &Commit, new: &Commit) -> Result<Vec<String>> {
        let (from, to) = if old.time > new.time {
            (new, old)
        } else {
            (old, new)
        };
        let out = git(&self.path)
            .args(["log", "--format=%h %s"])
            .arg(format!("{}..{}", from.hash, to.hash))
            .output()?;
        if !out.status.success() {
            bail!(
                "git log failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::to_owned)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn commit(hash: &str, time: i64, nixpkgs: Option<&str>) -> Commit {
        Commit {
            hash: hash.into(),
            time,
            subject: format!("commit {hash}"),
            nixpkgs: nixpkgs.map(str::to_owned),
        }
    }

    fn repo() -> Repo {
        Repo {
            path: PathBuf::from("/nonexistent"),
            commits: vec![
                commit("cccccccccc", 300, Some("e158d9ed9b51")),
                commit("bbbbbbbbbb", 200, Some("e158d9ed9b51")),
                commit("aaaaaaaaaa", 100, Some("8ce4ef6aaaaa")),
            ],
        }
    }

    fn info(time: i64, nixpkgs: &str, revision: Option<&str>) -> GenInfo {
        GenInfo {
            time: Some(time),
            nixpkgs: Some(nixpkgs.into()),
            revision: revision.map(str::to_owned),
        }
    }

    fn link(repo: &Repo, info: &GenInfo) -> (String, Confidence) {
        let link = repo.correlate(info).unwrap();
        (link.commit.hash, link.confidence)
    }

    #[test]
    fn correlates_generations() {
        let r = repo();
        // Recorded revision wins over timing.
        assert_eq!(
            link(&r, &info(250, "e158d9e", Some("aaaaaaa"))),
            ("aaaaaaaaaa".into(), Confidence::Exact)
        );
        assert_eq!(
            link(&r, &info(250, "e158d9e", Some("bbbbbbb-dirty"))),
            ("bbbbbbbbbb".into(), Confidence::Dirty)
        );
        // Otherwise the newest commit before the generation, checked against nixpkgs.
        assert_eq!(
            link(&r, &info(250, "e158d9e", None)),
            ("bbbbbbbbbb".into(), Confidence::Likely)
        );
        assert_eq!(
            link(&r, &info(150, "e158d9e", None)),
            ("aaaaaaaaaa".into(), Confidence::Guess)
        );
        assert!(r.correlate(&info(50, "e158d9e", None)).is_none());
    }

    #[test]
    fn reads_version_metadata() {
        let script = r#"{"nixosVersion":"26.11.20260926.e158d9e","nixpkgsRevision":"e158d9ed9b","configurationRevision":"abc123-dirty"}"#;
        assert_eq!(
            json_field(script, "nixpkgsRevision").as_deref(),
            Some("e158d9ed9b")
        );
        assert_eq!(
            json_field(script, "configurationRevision").as_deref(),
            Some("abc123-dirty")
        );
        assert_eq!(json_field(script, "missing"), None);
        assert_eq!(
            nixpkgs_from_name("/nix/store/x-nixos-system-host-4090-26.11.20260926.e158d9e")
                .as_deref(),
            Some("e158d9e")
        );
        assert_eq!(
            nixpkgs_from_name("/nix/store/x-home-manager-generation"),
            None
        );
    }

    #[test]
    fn reads_locked_nixpkgs() {
        let lock = json!({
            "root": "root",
            "nodes": {
                "root": { "inputs": { "nixpkgs": "nixpkgs_2", "home-manager": "home-manager" } },
                "nixpkgs_2": { "locked": { "rev": "e158d9ed9b51" } }
            }
        });
        assert_eq!(locked_nixpkgs(&lock).as_deref(), Some("e158d9ed9b51"));
        assert_eq!(locked_nixpkgs(&json!({})), None);
    }

    /// Builds a throwaway repo with real git to exercise log parsing and cat-file batching.
    #[test]
    fn loads_a_real_repo() {
        if Command::new("git").arg("--version").output().is_err() {
            return; // git isn't available (e.g. a minimal sandbox)
        }
        let dir = std::env::temp_dir().join(format!("nixi-git-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let run = |args: &[&str], time: &str| {
            let status = git(&dir)
                .args(["-c", "commit.gpgsign=false"])
                .args(args)
                .env("GIT_AUTHOR_DATE", time)
                .env("GIT_COMMITTER_DATE", time)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .status()
                .unwrap();
            assert!(status.success(), "git {args:?}");
        };
        run(&["init", "-q"], "");
        fs::write(dir.join("README"), "x").unwrap();
        run(&["add", "."], "");
        run(&["commit", "-qm", "no lock yet"], "@1000 +0000");
        let lock = json!({"root": "root", "nodes": {
            "root": {"inputs": {"nixpkgs": "nixpkgs"}},
            "nixpkgs": {"locked": {"rev": "e158d9ed9b51"}}}});
        fs::write(dir.join("flake.lock"), lock.to_string()).unwrap();
        run(&["add", "."], "");
        run(&["commit", "-qm", "Flake Update"], "@2000 +0000");

        let repo = Repo::load(&dir).unwrap();
        let summary: Vec<(i64, &str, Option<&str>)> = repo
            .commits
            .iter()
            .map(|c| (c.time, c.subject.as_str(), c.nixpkgs.as_deref()))
            .collect();
        assert_eq!(
            summary,
            [
                (2000, "Flake Update", Some("e158d9ed9b51")),
                (1000, "no lock yet", None)
            ]
        );
        let (old, new) = (&repo.commits[1], &repo.commits[0]);
        assert_eq!(repo.subjects_between(old, new).unwrap().len(), 1);
        assert_eq!(
            repo.subjects_between(new, old).unwrap().len(),
            1,
            "order-insensitive"
        );
        fs::remove_dir_all(dir).unwrap();
    }
}

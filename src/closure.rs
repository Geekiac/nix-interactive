//! Loading a store path's runtime closure via `nix path-info`, with an on-disk cache.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::store_path::{self, STORE_DIR};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PathInfo {
    pub path: String,
    pub nar_size: u64,
    pub references: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Closure {
    pub root: String,
    /// `<root>/sw` resolved, when the root is a NixOS system. Its direct references are
    /// what nvd calls the "selected" packages (`environment.systemPackages`).
    pub sw: Option<String>,
    pub paths: HashMap<String, PathInfo>,
}

impl Closure {
    /// Resolves `target` (a profile link, `./result`, or store path) and loads its closure.
    pub fn load(target: &Path, cache: Option<&Cache>) -> Result<Self> {
        let root = resolve_store_path(target)?;
        let infos = match cache.and_then(|c| c.get(&root)) {
            Some(infos) => infos,
            None => {
                let infos = query(&root)?;
                if let Some(cache) = cache {
                    cache.put(&root, &infos);
                }
                infos
            }
        };
        let sw = fs::canonicalize(Path::new(&root).join("sw"))
            .ok()
            .filter(|p| p.is_dir())
            .and_then(|p| store_path::base_path(p.to_str()?).map(str::to_owned));
        Ok(Self::new(root, sw, infos))
    }

    pub fn new(root: String, sw: Option<String>, infos: Vec<PathInfo>) -> Self {
        let paths = infos.into_iter().map(|i| (i.path.clone(), i)).collect();
        Self { root, sw, paths }
    }

    pub fn references(&self, path: &str) -> &[String] {
        self.paths.get(path).map_or(&[], |i| &i.references)
    }

    /// Sum of NAR sizes over the closure, i.e. `nix path-info --closure-size`.
    pub fn total_nar_size(&self) -> u64 {
        self.paths.values().map(|i| i.nar_size).sum()
    }
}

/// Follows symlinks from `target` to the top-level store path it lives in.
pub fn resolve_store_path(target: &Path) -> Result<String> {
    let resolved =
        fs::canonicalize(target).with_context(|| format!("cannot resolve {}", target.display()))?;
    let resolved = resolved
        .to_str()
        .with_context(|| format!("non-UTF-8 path {}", resolved.display()))?;
    match store_path::base_path(resolved) {
        Some(base) => Ok(base.to_owned()),
        None => bail!("{} is not in {STORE_DIR}", target.display()),
    }
}

fn nix_path_info(root: &str, json_format_2: bool) -> Result<Output> {
    let mut cmd = Command::new("nix");
    cmd.args([
        "--extra-experimental-features",
        "nix-command",
        "path-info",
        "--recursive",
        "--json",
    ]);
    if json_format_2 {
        cmd.args(["--json-format", "2"]);
    }
    cmd.arg(root)
        .output()
        .context("failed to run `nix path-info` (is nix on PATH?)")
}

fn query(root: &str) -> Result<Vec<PathInfo>> {
    let mut out = nix_path_info(root, true)?;
    if !out.status.success() {
        // Older Nix has no --json-format; its default output is parsed just as well.
        out = nix_path_info(root, false)?;
    }
    if !out.status.success() {
        bail!(
            "nix path-info failed for {root}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let json: Value =
        serde_json::from_slice(&out.stdout).context("parsing nix path-info output")?;
    let infos = parse_path_info(&json)?;
    if !infos.iter().any(|i| i.path == root) {
        bail!("nix path-info output does not include {root}");
    }
    Ok(infos)
}

fn normalize(path: &str) -> String {
    if path.starts_with('/') {
        path.to_owned()
    } else {
        format!("{STORE_DIR}/{path}")
    }
}

/// Accepts every shape `nix path-info --json` has produced: a list of objects with a
/// `path` field (Nix < 2.19), an object keyed by path (format 1), or an object whose
/// `info` field is keyed by store path basename (format 2).
fn parse_path_info(json: &Value) -> Result<Vec<PathInfo>> {
    let entries: Vec<(&str, &Value)> = match json {
        Value::Object(map) => {
            let map = match map.get("info") {
                Some(Value::Object(info)) => info,
                _ => map,
            };
            map.iter().map(|(k, v)| (k.as_str(), v)).collect()
        }
        Value::Array(items) => items
            .iter()
            .filter_map(|item| Some((item.get("path")?.as_str()?, item)))
            .collect(),
        _ => bail!("unexpected nix path-info output"),
    };
    Ok(entries
        .into_iter()
        .filter(|(_, info)| info.is_object() && info.get("valid") != Some(&Value::Bool(false)))
        .map(|(path, info)| PathInfo {
            path: normalize(path),
            nar_size: info["narSize"].as_u64().unwrap_or(0),
            references: info["references"]
                .as_array()
                .map(|refs| {
                    refs.iter()
                        .filter_map(Value::as_str)
                        .map(normalize)
                        .collect()
                })
                .unwrap_or_default(),
        })
        .collect())
}

/// Closure cache keyed by root store path. Store paths are immutable, so entries never go stale.
pub struct Cache {
    dir: PathBuf,
}

impl Cache {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// `$XDG_CACHE_HOME/nix-interactive/closures-v1`, falling back to `~/.cache`.
    pub fn default_dir() -> Option<PathBuf> {
        let base = std::env::var_os("XDG_CACHE_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
        Some(base.join("nix-interactive").join("closures-v1"))
    }

    fn file(&self, root: &str) -> PathBuf {
        let name = root.rsplit('/').next().unwrap_or(root);
        self.dir.join(format!("{name}.json"))
    }

    pub fn get(&self, root: &str) -> Option<Vec<PathInfo>> {
        let bytes = fs::read(self.file(root)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// Best effort: a cache that can't be written just means re-querying next time.
    pub fn put(&self, root: &str, infos: &[PathInfo]) {
        let file = self.file(root);
        let tmp = file.with_extension(format!("json.tmp.{}", std::process::id()));
        let written = fs::create_dir_all(&self.dir)
            .and_then(|()| fs::write(&tmp, serde_json::to_vec(infos).unwrap_or_default()))
            .and_then(|()| fs::rename(&tmp, &file));
        if written.is_err() {
            let _ = fs::remove_file(&tmp);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn expected() -> Vec<PathInfo> {
        vec![PathInfo {
            path: "/nix/store/aaa-foo-1.0".into(),
            nar_size: 42,
            references: vec!["/nix/store/bbb-glibc-2.42".into()],
        }]
    }

    #[test]
    fn parses_format_2() {
        let json = json!({
            "version": 2,
            "info": { "aaa-foo-1.0": { "narSize": 42, "references": ["bbb-glibc-2.42"] } }
        });
        assert_eq!(parse_path_info(&json).unwrap(), expected());
    }

    #[test]
    fn parses_format_1() {
        let json = json!({
            "/nix/store/aaa-foo-1.0": { "narSize": 42, "references": ["/nix/store/bbb-glibc-2.42"] },
            "/nix/store/ccc-missing": null
        });
        assert_eq!(parse_path_info(&json).unwrap(), expected());
    }

    #[test]
    fn parses_legacy_array() {
        let json = json!([
            { "path": "/nix/store/aaa-foo-1.0", "narSize": 42, "references": ["/nix/store/bbb-glibc-2.42"] },
            { "path": "/nix/store/ccc-missing", "valid": false }
        ]);
        assert_eq!(parse_path_info(&json).unwrap(), expected());
    }

    #[test]
    fn cache_round_trips() {
        let dir = std::env::temp_dir().join(format!("nixi-cache-test-{}", std::process::id()));
        let cache = Cache::new(dir.clone());
        assert!(cache.get("/nix/store/aaa-foo-1.0").is_none());
        cache.put("/nix/store/aaa-foo-1.0", &expected());
        assert_eq!(cache.get("/nix/store/aaa-foo-1.0"), Some(expected()));
        fs::remove_dir_all(dir).unwrap();
    }
}

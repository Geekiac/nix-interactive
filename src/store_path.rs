//! Store path name parsing and Nix-style version comparison, matching nvd's rules.

use std::cmp::Ordering;
use std::fmt;

pub const STORE_DIR: &str = "/nix/store";

/// Truncates a path inside the store to its top-level store path
/// (`/nix/store/<hash>-<name>/bin/foo` -> `/nix/store/<hash>-<name>`).
pub fn base_path(path: &str) -> Option<&str> {
    let rest = path.strip_prefix(STORE_DIR)?.strip_prefix('/')?;
    if rest.is_empty() {
        return None;
    }
    let end = rest
        .find('/')
        .map_or(path.len(), |i| STORE_DIR.len() + 1 + i);
    Some(&path[..end])
}

/// The name part of a store path, after the hash (`/nix/store/<hash>-foo-1.0` -> `foo-1.0`).
pub fn name(path: &str) -> &str {
    let file = path.rsplit('/').next().unwrap_or(path);
    file.split_once('-').map_or(file, |(_, name)| name)
}

/// Splits a store path into `(pname, version)` the way nvd does: the name after the hash
/// is cut at the first `-` that is followed by a digit.
pub fn parse_name(path: &str) -> (&str, Option<&str>) {
    let name = name(path);
    let name = match name.strip_suffix(".drv") {
        Some(stripped) if !stripped.is_empty() => stripped,
        _ => name,
    };
    let bytes = name.as_bytes();
    for i in 1..bytes.len().saturating_sub(1) {
        if bytes[i] == b'-' && bytes[i + 1].is_ascii_digit() {
            return (&name[..i], Some(&name[i + 1..]));
        }
    }
    (name, None)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Chunk {
    /// Digit run with leading zeros stripped, so numeric order is (length, lexical).
    Num(String),
    Word(String),
}

impl Ord for Chunk {
    fn cmp(&self, other: &Self) -> Ordering {
        use Chunk::*;
        match (self, other) {
            (Num(a), Num(b)) => a.len().cmp(&b.len()).then_with(|| a.cmp(b)),
            (Word(a), Word(b)) if a == b => Ordering::Equal,
            // "pre" sorts before everything else, as in `nix-env --upgrade`.
            (Word(a), _) if a == "pre" => Ordering::Less,
            (_, Word(b)) if b == "pre" => Ordering::Greater,
            (Num(_), Word(_)) => Ordering::Greater,
            (Word(_), Num(_)) => Ordering::Less,
            (Word(a), Word(b)) => a.cmp(b),
        }
    }
}

impl PartialOrd for Chunk {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// A package version, compared chunk-wise like nvd / `nix-env`.
/// Equality is by chunks, so `1.0` and `1-0` are the same version.
#[derive(Debug, Clone)]
pub struct Version {
    text: String,
    chunks: Vec<Chunk>,
}

impl Version {
    pub fn new(text: Option<&str>) -> Self {
        let text = text.unwrap_or("").to_owned();
        let mut chunks = Vec::new();
        let mut chars = text.chars().peekable();
        while let Some(&c) = chars.peek() {
            if c.is_ascii_digit() {
                let mut run = String::new();
                while let Some(&d) = chars.peek().filter(|d| d.is_ascii_digit()) {
                    run.push(d);
                    chars.next();
                }
                chunks.push(Chunk::Num(run.trim_start_matches('0').to_owned()));
            } else if c.is_alphabetic() {
                let mut run = String::new();
                while let Some(&a) = chars.peek().filter(|a| a.is_alphabetic()) {
                    run.push(a);
                    chars.next();
                }
                chunks.push(Chunk::Word(run));
            } else {
                chars.next();
            }
        }
        Self { text, chunks }
    }

    /// The version string; empty if the package has no version.
    pub fn text(&self) -> &str {
        &self.text
    }
}

impl PartialEq for Version {
    fn eq(&self, other: &Self) -> bool {
        self.chunks == other.chunks
    }
}

impl Eq for Version {}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        self.chunks.cmp(&other.chunks)
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.text.is_empty() {
            f.write_str("<none>")
        } else {
            f.write_str(&self.text)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::new(Some(s))
    }

    #[test]
    fn splits_pname_and_version() {
        let cases = [
            (
                "/nix/store/abc-firefox-141.0.2",
                ("firefox", Some("141.0.2")),
            ),
            (
                "/nix/store/abc-krb5-1.22.2-lib",
                ("krb5", Some("1.22.2-lib")),
            ),
            (
                "/nix/store/abc-nixos-system-host-26.11.2026",
                ("nixos-system-host", Some("26.11.2026")),
            ),
            ("/nix/store/abc-etc", ("etc", None)),
            ("/nix/store/abc-hello-2.12.drv", ("hello", Some("2.12"))),
            ("/nix/store/abc-foo.drv", ("foo", None)),
            ("/nix/store/abc-7zz-24.09", ("7zz", Some("24.09"))),
            ("/nix/store/abc-x-", ("x-", None)),
        ];
        for (path, expected) in cases {
            assert_eq!(parse_name(path), expected, "{path}");
        }
    }

    #[test]
    fn base_path_truncates() {
        assert_eq!(
            base_path("/nix/store/abc-foo/bin/foo"),
            Some("/nix/store/abc-foo")
        );
        assert_eq!(base_path("/nix/store/abc-foo"), Some("/nix/store/abc-foo"));
        assert_eq!(base_path("/nix/store/"), None);
        assert_eq!(base_path("/etc/foo"), None);
    }

    #[test]
    fn version_ordering() {
        assert!(v("1.2") < v("1.10"));
        assert!(v("1.0") < v("1.0.1"));
        // Matches nvd, which compares chunk lists directly; nix itself would pad with an
        // empty chunk and put 1.0pre first.
        assert!(v("1.0pre") > v("1.0"));
        assert!(v("1.0pre") < v("1.0.0"));
        assert!(v("1.0pre1") < v("1.0alpha"));
        assert!(v("1.0a") < v("1.0.1"));
        assert!(v("2.0") > v("1.99"));
        assert_eq!(v("1.007"), v("1.7"));
        assert_eq!(v("1-0"), v("1.0"));
        assert!(Version::new(None) < v("0"));
        assert!(v("20260508") < v("123456789012345678901234567890"));
    }
}

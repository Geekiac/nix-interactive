//! `OLD:NEW` generation ranges, shared by `nixi diff` and the viewer's `:` prompt.
//!
//! Each side is `0` (the current generation) or `-N` (N before it), a positive generation
//! number, or a path.

use anyhow::{bail, Context, Result};

use crate::sources::{self, Generation};

/// One side of an `OLD:NEW` range.
#[derive(Debug, PartialEq)]
pub enum SideSpec {
    /// Steps back from the current generation (`0`, `-1`, …).
    Back(u64),
    /// A generation number.
    Number(u64),
    Path(String),
}

impl SideSpec {
    pub fn parse(s: &str) -> Result<Self> {
        if s.is_empty() {
            bail!("empty side in range; expected OLD:NEW, e.g. -1:0");
        }
        Ok(match s.parse::<i64>() {
            Ok(n) if n <= 0 => Self::Back(n.unsigned_abs()),
            Ok(n) => Self::Number(n.unsigned_abs()),
            Err(_) => Self::Path(s.to_owned()),
        })
    }

    /// Index into `gens` of the generation this side names; paths name none.
    pub fn index(&self, gens: &[Generation]) -> Result<usize> {
        match self {
            Self::Back(back) => sources::back_from_current(gens, *back),
            Self::Number(n) => sources::position(gens, *n),
            Self::Path(path) => bail!("{path} is a path, not a generation"),
        }
    }
}

pub fn parse_range(range: &str) -> Result<(SideSpec, SideSpec)> {
    let (old, new) = range
        .trim()
        .split_once(':')
        .with_context(|| format!("expected OLD:NEW, e.g. -1:0 (got {range:?})"))?;
    Ok((SideSpec::parse(old)?, SideSpec::parse(new)?))
}

/// The `-N` of generation `index` relative to the current one, when it's at or before it.
pub fn back_label(gens: &[Generation], index: usize) -> Option<String> {
    let current = gens.iter().position(|g| g.current)?;
    let back = current.checked_sub(index)?;
    Some(if back == 0 {
        "0".into()
    } else {
        format!("-{back}")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn parses_ranges() {
        use SideSpec::*;
        assert_eq!(parse_range("-1:0").unwrap(), (Back(1), Back(0)));
        assert_eq!(parse_range(" -3:-2 ").unwrap(), (Back(3), Back(2)));
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
    fn resolves_sides_to_indices() {
        let gens: Vec<Generation> = [40, 42, 43, 44]
            .into_iter()
            .map(|n| Generation {
                number: n,
                last_number: n,
                path: PathBuf::from(format!("/p/system-{n}-link")),
                store_path: format!("/nix/store/s{n}"),
                created: None,
                current: n == 43, // rolled back: 44 is newer than current
            })
            .collect();
        assert_eq!(SideSpec::Back(0).index(&gens).unwrap(), 2);
        assert_eq!(SideSpec::Back(2).index(&gens).unwrap(), 0);
        assert!(SideSpec::Back(3).index(&gens).is_err());
        assert_eq!(SideSpec::Number(44).index(&gens).unwrap(), 3);
        assert!(SideSpec::Number(41).index(&gens).is_err());
        assert!(SideSpec::Path("./result".into()).index(&gens).is_err());
        let labels: Vec<Option<String>> = (0..4).map(|i| back_label(&gens, i)).collect();
        assert_eq!(
            labels,
            [Some("-2".into()), Some("-1".into()), Some("0".into()), None]
        );
    }
}

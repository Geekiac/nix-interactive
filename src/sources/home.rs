//! Home-manager generations embedded in NixOS system generations by the home-manager NixOS
//! module. Each system closure contains `unit-home-manager-<user>.service`, which references
//! that user's `home-manager-generation` store path.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::thread;

use anyhow::{bail, Result};

use super::Generation;
use crate::closure::{Cache, Closure};

const UNIT_PREFIX: &str = "unit-home-manager-";
const UNIT_SUFFIX: &str = ".service";

/// Closure queries run in parallel batches of this size on a cold cache.
const LOAD_BATCH: usize = 8;

fn unit_user(path: &str) -> Option<&str> {
    let (_, name) = path.rsplit('/').next()?.split_once('-')?;
    name.strip_prefix(UNIT_PREFIX)?.strip_suffix(UNIT_SUFFIX)
}

/// The home-manager generation `user` gets from this system closure, if any.
pub fn find_in_closure<'a>(closure: &'a Closure, user: &str) -> Option<&'a str> {
    closure
        .paths
        .keys()
        .filter(|p| unit_user(p) == Some(user))
        .flat_map(|unit| closure.references(unit))
        .find(|r| r.ends_with("-home-manager-generation"))
        .map(String::as_str)
}

/// Users with an embedded home-manager configuration in this closure.
pub fn users(closure: &Closure) -> impl Iterator<Item = &str> {
    closure.paths.keys().filter_map(|p| unit_user(p))
}

fn load_all(system_gens: &[Generation], cache: Option<&Cache>) -> Vec<Result<Closure>> {
    system_gens
        .chunks(LOAD_BATCH)
        .flat_map(|batch| {
            thread::scope(|s| {
                let handles: Vec<_> = batch
                    .iter()
                    .map(|g| s.spawn(|| Closure::load(&g.path, cache)))
                    .collect();
                handles
                    .into_iter()
                    .map(|h| h.join().expect("closure loader panicked"))
                    .collect::<Vec<_>>()
            })
        })
        .collect()
}

/// `user`'s home-manager generations, oldest first. Consecutive system generations that
/// share a home-manager generation collapse into one entry spanning their numbers.
pub fn generations(
    system_gens: &[Generation],
    user: &str,
    cache: Option<&Cache>,
) -> Result<Vec<Generation>> {
    collapse(system_gens, load_all(system_gens, cache), user)
}

fn collapse(
    system_gens: &[Generation],
    closures: Vec<Result<Closure>>,
    user: &str,
) -> Result<Vec<Generation>> {
    let mut gens: Vec<Generation> = Vec::new();
    let mut other_users = BTreeSet::new();
    let mut previous: Option<String> = None;
    for (sys, closure) in system_gens.iter().zip(closures) {
        let closure = closure?;
        let Some(hm) = find_in_closure(&closure, user) else {
            other_users.extend(users(&closure).map(str::to_owned));
            previous = None;
            continue;
        };
        match gens.last_mut() {
            Some(last) if previous.as_deref() == Some(hm) => {
                last.last_number = sys.number;
                last.current |= sys.current;
            }
            _ => gens.push(Generation {
                number: sys.number,
                last_number: sys.number,
                path: PathBuf::from(hm),
                store_path: hm.to_owned(),
                created: sys.created,
                current: sys.current,
            }),
        }
        previous = Some(hm.to_owned());
    }
    if gens.is_empty() {
        if other_users.is_empty() {
            bail!("no system generation contains a home-manager configuration (NixOS module)");
        }
        let found: Vec<_> = other_users.into_iter().collect();
        bail!(
            "no home-manager generations for user {user}; found: {}",
            found.join(", ")
        );
    }
    Ok(gens)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::closure::PathInfo;

    fn system(root: &str, units: &[(&str, &str)]) -> Closure {
        let mut infos = vec![PathInfo {
            path: root.into(),
            nar_size: 1,
            references: units.iter().map(|(unit, _)| unit.to_string()).collect(),
        }];
        for (unit, hm) in units {
            infos.push(PathInfo {
                path: unit.to_string(),
                nar_size: 1,
                references: vec![hm.to_string(), "/nix/store/x-bash-5.3".into()],
            });
            infos.push(PathInfo {
                path: hm.to_string(),
                nar_size: 1,
                references: vec![],
            });
        }
        Closure::new(root.into(), None, infos)
    }

    #[test]
    fn finds_generation_per_user() {
        let c = system(
            "/nix/store/s-nixos-system",
            &[
                (
                    "/nix/store/u1-unit-home-manager-bob.service",
                    "/nix/store/h1-home-manager-generation",
                ),
                (
                    "/nix/store/u2-unit-home-manager-jim-bob.service",
                    "/nix/store/h2-home-manager-generation",
                ),
            ],
        );
        assert_eq!(
            find_in_closure(&c, "bob"),
            Some("/nix/store/h1-home-manager-generation")
        );
        assert_eq!(
            find_in_closure(&c, "jim-bob"),
            Some("/nix/store/h2-home-manager-generation")
        );
        assert_eq!(find_in_closure(&c, "alice"), None);
        let mut found: Vec<_> = users(&c).collect();
        found.sort();
        assert_eq!(found, ["bob", "jim-bob"]);
    }

    #[test]
    fn collapses_unchanged_generations() {
        let unit = "/nix/store/u-unit-home-manager-bob.service";
        let (a, b) = (
            "/nix/store/ha-home-manager-generation",
            "/nix/store/hb-home-manager-generation",
        );
        // System generations 1..=6 using home-manager a, a, b, (none), b, a; 5 is current.
        let hms = [Some(a), Some(a), Some(b), None, Some(b), Some(a)];
        let sys: Vec<Generation> = (1..=6)
            .map(|n| Generation {
                number: n,
                last_number: n,
                path: PathBuf::from(format!("/nix/store/s{n}-nixos-system")),
                store_path: format!("/nix/store/s{n}-nixos-system"),
                created: None,
                current: n == 5,
            })
            .collect();
        let closures = sys
            .iter()
            .zip(hms)
            .map(|(g, hm)| {
                let units: Vec<(&str, &str)> = hm.map(|hm| (unit, hm)).into_iter().collect();
                Ok(system(&g.store_path, &units))
            })
            .collect();

        let gens = collapse(&sys, closures, "bob").unwrap();
        let summary: Vec<(u64, u64, &str, bool)> = gens
            .iter()
            .map(|g| (g.number, g.last_number, g.store_path.as_str(), g.current))
            .collect();
        assert_eq!(
            summary,
            [
                (1, 2, a, false),
                (3, 3, b, false),
                (5, 5, b, true),
                (6, 6, a, false)
            ]
        );

        let err = collapse(
            &sys[..1],
            vec![Ok(system("/nix/store/s", &[(unit, a)]))],
            "eve",
        );
        assert_eq!(
            err.unwrap_err().to_string(),
            "no home-manager generations for user eve; found: bob"
        );
    }
}

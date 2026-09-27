//! Where generations come from: Nix profiles, and home-manager generations embedded in
//! NixOS system generations.

pub mod home;
pub mod profile;

use std::path::PathBuf;
use std::time::SystemTime;

use anyhow::{Context, Result};
use chrono::{DateTime, Local};

use crate::closure::Cache;

#[derive(Debug, Clone)]
pub struct Generation {
    /// Generation number. For embedded home-manager: the first system generation using it.
    pub number: u64,
    /// Last number covered. Embedded home-manager generations span every consecutive system
    /// generation that left them unchanged; for profiles this equals `number`.
    pub last_number: u64,
    /// What to diff: the profile link, or the store path for embedded home-manager.
    pub path: PathBuf,
    pub store_path: String,
    pub created: Option<SystemTime>,
    pub current: bool,
}

impl Generation {
    pub fn covers(&self, n: u64) -> bool {
        (self.number..=self.last_number).contains(&n)
    }

    /// Local creation time as `YYYY-MM-DD HH:MM`, or `?` when unknown.
    pub fn created_label(&self) -> String {
        self.created.map_or_else(
            || "?".to_owned(),
            |t| {
                DateTime::<Local>::from(t)
                    .format("%Y-%m-%d %H:%M")
                    .to_string()
            },
        )
    }

    pub fn number_label(&self) -> String {
        if self.number == self.last_number {
            self.number.to_string()
        } else {
            format!("{}-{}", self.number, self.last_number)
        }
    }
}

pub enum Source {
    Profile(PathBuf),
    /// `user`'s home-manager generations inside `system_profile`'s generations.
    Home {
        system_profile: PathBuf,
        user: String,
    },
}

impl Source {
    /// All generations, oldest first.
    pub fn generations(&self, cache: Option<&Cache>) -> Result<Vec<Generation>> {
        match self {
            Self::Profile(profile) => profile::generations(profile),
            Self::Home {
                system_profile,
                user,
            } => home::generations(&profile::generations(system_profile)?, user, cache),
        }
    }
}

/// Index of the generation numbered `n`; for embedded home-manager, the one system
/// generation `n` used.
pub fn position(gens: &[Generation], n: u64) -> Result<usize> {
    gens.iter()
        .position(|g| g.covers(n))
        .with_context(|| format!("no generation {n}"))
}

/// Index of the generation `back` steps before the current one (0 = current). Steps follow
/// the list, so they skip numbers removed by garbage collection.
pub fn back_from_current(gens: &[Generation], back: u64) -> Result<usize> {
    let current = gens
        .iter()
        .position(|g| g.current)
        .context("no current generation to count back from")?;
    usize::try_from(back)
        .ok()
        .and_then(|back| current.checked_sub(back))
        .with_context(|| {
            format!("can't go back {back}: only {current} generation(s) before the current one")
        })
}

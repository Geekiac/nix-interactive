# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

`nixi` (crate `nix-interactive`) is a Rust CLI and ratatui TUI for browsing Nix generations
and diffing their closures. See `README.md` for user-facing behavior and `plan.md` for the
original design, milestones, and where the build departed from it.

## Commands

The Rust toolchain comes from the flake; there is none on the system PATH.

```sh
nix develop -c cargo test                          # all tests
nix develop -c cargo test range::tests::parses_ranges   # one test (module path + name)
nix develop -c cargo clippy --all-targets -- -D warnings
nix develop -c cargo fmt
nix develop -c cargo build --release               # ./target/release/nixi
nix flake check                                    # sandboxed build + tests, clippy, rustfmt checks
nix build                                          # ./result/bin/nixi (wrapped with nix, nvd, git)
nix fmt flake.nix                                  # format the flake; bare `nix fmt` waits on stdin
```

Gotchas:

- Flakes only see git-tracked files: `git add` new files before `nix build` / `nix flake check`.
- `nix flake check` does not update `./result`. After changes, rerun `nix build` (or use
  `nix run .`) before trying `./result/bin/nixi`, or you'll be testing a stale binary.
- Tests run inside the Nix build sandbox during `nix flake check`: no `/run/current-system`,
  no git, `HOME=/homeless-shelter`. Tests needing a real store path take one from `$PATH`;
  the git integration test skips itself when git is missing.

Driving the TUI for a manual check (tmux isn't installed globally):

```sh
nix shell nixpkgs#tmux -c bash -c '
  tmux -L t new-session -d -s t -x 140 -y 30 "./target/release/nixi"; sleep 1.5
  tmux -L t send-keys -t t k Tab j Enter; sleep 0.5
  tmux -L t capture-pane -p -t t; tmux -L t kill-server'
```

## Architecture

Data flows **sources → closure → diff → render (CLI) / ui (TUI)**, with `git` and `range`
feeding both front ends.

- `sources/` finds generations. `profile.rs` reads `<name>-<N>-link` symlinks (dates are
  symlink mtimes). `home.rs` finds home-manager generations *embedded in system closures*:
  the NixOS module's `unit-home-manager-<user>.service` path references
  `…-home-manager-generation`. Consecutive system generations sharing one collapse into a
  single `Generation` with `number..=last_number`, so home-manager numbers are system
  generation numbers throughout (CLI arguments, commit links, the TUI).
- `closure.rs` loads a runtime closure with `nix path-info --recursive --json` and must
  accept all three JSON shapes (legacy array, format 1 keyed by path, format 2 under `info`
  keyed by basename). It tries `--json-format 2` and falls back for older Nix. Results are
  cached per root store path in `~/.cache/nix-interactive/closures-v1/`. Store paths are
  immutable, so the cache never invalidates; bump the directory version if the cached shape changes.
- `diff.rs` + `store_path.rs` implement **nvd's semantics exactly**: pname/version split at
  the first `-<digit>`, nvd's chunk-wise version ordering (including its quirk that
  `1.0pre` > `1.0`), selected packages = direct references of `<root>/sw` when both sides
  have it. `render.rs` output is meant to match `nvd diff` line for line (plus our
  "Rebuilt" line). After changing either, compare against `nvd --color never diff A B` on
  real generation pairs. The "rebuilt" category and per-package `size_delta` are additions.
- `range.rs` parses `OLD:NEW` (`0`/`-N` count back from the current generation, positive =
  generation number, else a path; a lone side means `SIDE:0`). Shared by `nixi diff`,
  `--range`, and the TUI's `:` prompt.
- `git.rs` links generations to config-repo commits: exact via `configurationRevision`
  (read from the generation's `sw/bin/nixos-version` script), else the newest commit
  before the generation's mtime, confirmed if that commit's `flake.lock` pins the same
  nixpkgs. All `flake.lock`s are read through one `git cat-file --batch`. Links are keyed
  by generation number.
- `delete.rs` deletes generations via `nix-env --profile P --delete-generations N` (with
  `sudo` when the profile's directory belongs to another user, i.e. the system profile).
  `refusal()` is the single guard (current generation, non-profile paths); the CLI
  (`nixi delete`, type `yes`) and the TUI (`D`, type the number) both go through it.
- `gc.rs` runs `nix-store --gc` with stdout piped (Nix prints its `N store paths deleted,
  X freed` summary there; progress goes to stderr) and parses the summary. Used by `nixi gc`,
  the offer after `nixi delete`, and the TUI's `C`. To test it for real without touching
  the user's store, point it at a throwaway store: `NIX_REMOTE="local?root=$DIR" nixi gc`.
- `config.rs`: `~/.config/nix-interactive/config.toml`. Precedence is flag > env
  (`NIXI_REPO`) > config > default, applied in `SourceArgs::apply` in `main.rs`.
- `main.rs` holds the clap CLI. `SourceArgs` (`--profile/--home/--user/--repo`) is
  flattened with `global = true`, so it applies to the TUI and every subcommand. A single
  range argument uses `allow_hyphen_values` so `-1` isn't parsed as a flag; multi-value
  arguments (`nixi delete GEN...`) must use `allow_negative_numbers` instead, because
  `allow_hyphen_values` there swallows later flags like `--yes`.

### TUI (`ui/`)

- `app.rs` is all state and key handling with no drawing; `view.rs` only draws from `App`
  (it takes `&mut App` just to record pane heights and stateful widget offsets). Keep that
  split: tests drive `App` directly and render `view::draw` into ratatui's `TestBackend`.
- Nothing blocks the UI thread. `loader.rs` has a worker pool for closures (`request` +
  `prioritize` for the on-screen pair) and `spawn_job` for external commands (nvd,
  `nix why-depends`, `git log`). Everything reports back as a `Msg` on one channel,
  drained in the event loop in `ui/mod.rs`. Job results live in `App.jobs` keyed by
  `JobKey`, so revisiting a pair is instant.
- `App::sync()` is the central reconciliation step, called after every key and message:
  it requests the closures the current pair needs, starts jobs for the active diff mode,
  and recomputes `CurrentDiff` when the `(old, new)` store paths change.
- Tabs: `[profile, home (<user>), extra profiles from config…, paths]`. Commit links for the
  home tab come from tab 0's links, since both share system generation numbers.
- Actions that need the real terminal (deleting, where sudo may prompt; garbage
  collection, which shows Nix's progress) don't run inside `App`: it records a `Pending`
  (`Delete` or `Gc`), and the event loop in `ui/mod.rs` calls
  `ratatui::restore()`, runs the command on the plain terminal, re-inits, and reports back
  with `App::deleted`. Deleting from the system tab also recomputes the home-manager tab.
- `detail.rs` is the package popup model, and `ansi.rs` converts `nvd --color always` SGR output to
  ratatui `Text`.
- Colors use the terminal's 16-color palette (`Color::Red` etc.) so light and dark themes both work.
- Test fixtures: `ui::app::tests::app()` builds three fake system generations (41–43, current
  43) with preloaded closures and a zero-worker `Loader`. `view::tests::screen()` renders
  it to a string for assertions.

## Conventions

- Features bump the minor version, fixes the patch version, in `Cargo.toml`, `Cargo.lock`
  and `flake.nix` together (`nixi --version` and nixpkgs' `versionCheckHook` read it). Then
  tag `vX.Y.Z` and push the tag.
- ratatui 0.30: import crossterm through `ratatui::crossterm`, not a separate dependency.
- The package wrapper adds `nix`, `nvd`, `git` with `--suffix PATH`, so the user's own
  binaries win.
- When adding keys, update the key handling in `app.rs`, the status hints (most useful
  first, since narrow terminals truncate the end) and help popup in `view.rs`, and the key
  table in `README.md`. The help popup must fit in 20 rows.

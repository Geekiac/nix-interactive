# nix-interactive — interactive historical diff viewer for Nix

## Status (2026-09-27)

All six milestones are done. The sections below are the plan as approved; this section
records what was added afterwards and where the build differs.

### Added after the milestones

- **`OLD:NEW` ranges** replace `nixi diff A B`. `0` is the current generation and `-N` the
  Nth before it, so `-1:0` is the default (previous vs current) and `-2:-1` is the switch
  before. Positive numbers are generation numbers (`40:43`); anything else is a path
  (`0:./result`). A lone side is compared with the current generation, so `-1` = `-1:0`.
  Counting back follows existing generations, skipping gaps left by garbage collection;
  with `--home` it steps through distinct home-manager generations. Parsing lives in
  `src/range.rs`, shared by the CLI and the viewer.
- **Ranges in the viewer**: `:` opens a prompt that pins both sides to a range on the
  active tab (errors show in the status bar); `nixi --range OLD:NEW` opens on one,
  waiting for home-manager generations to load if that tab is active. The diff title shows
  the pair as a range, e.g. `system 42 → 43 (-1:0)`.
- **Discoverability fixes**: the status bar leads with `? help` and `: range` so narrow
  terminals don't truncate them, and the help popup is one line per key and fits 20 rows.
- **`nixi list`** subcommand, and **`nixi config`** (settings in effect; `--example` prints a
  starter config file).
- **`CLAUDE.md`** for future agent sessions: commands, architecture, and gotchas (flakes
  only see git-tracked files; `nix flake check` doesn't refresh `./result`; tests also run
  in the build sandbox).

### Differences from the plan

- Sources are a `Source` enum plus functions, not a trait. Ad-hoc paths are the viewer's
  `--path` tab and path sides of a range, not a separate source module.
- UI modules are `ui/{app,view,loader,detail,ansi}.rs` rather than one file per pane.
  nvd, `nix why-depends`, and `git log` all run as background jobs through `loader.rs`
  (no separate `nvd.rs`); nvd's colors go through a small SGR parser (`ansi.rs`).
- The commits view is on `L` (`c` toggles "changed" packages) and shows `git log --stat`
  between the two commits rather than `git diff --stat`. Git is driven through the CLI,
  not gix, and all `flake.lock`s are read with one `git cat-file --batch`.
- Category toggles are `u d c a r b` (upgraded, downgraded, changed, added, removed,
  rebuilt; rebuilt hidden by default). `enter` in the diff pane opens package details
  (`w` runs why-depends), while in the list it pins the new side.
- Closures come from `nix path-info --recursive --json` without `--closure-size`; sizes are
  summed from each path's NAR size. All three JSON output shapes are accepted.
- Version ordering follows nvd, which ranks `1.0pre` above `1.0` (Nix ranks it below), so
  output matches `nvd diff` line for line. System generation descriptions use the text
  after the last `-`, because a hostname like `nixos-desktop-4090` breaks nvd's split.
- Commit links: exact via `configurationRevision` (`+` when dirty), `≈` when the newest
  earlier commit's `flake.lock` pins the generation's nixpkgs, `?` otherwise.
- The config file also accepts `profile`, `user`, and named `[[profiles]]` shown as tabs and
  selectable with `--profile <name>`. Precedence: flag > `NIXI_REPO` > config > default.
- Packaging uses `rustPlatform.buildRustPackage` (not crane); runtime tools are added with
  `--suffix PATH` so the user's own `nix`/`nvd`/`git` win. Tests are inline unit tests plus
  ratatui `TestBackend` rendering tests, not a `tests/` fixture directory.

### Distribution

- **Repo and release**: github.com/Geekiac/nix-interactive, MIT `LICENSE`, tag `v0.1.0`.
  Anyone can run it with `nix run github:Geekiac/nix-interactive`. The flake's package has
  full `meta` (description, homepage, license, platforms, mainProgram).
- **nixpkgs**: draft PR [NixOS/nixpkgs#567518](https://github.com/NixOS/nixpkgs/pull/567518),
  two commits: `maintainers: add geekiac` and `nix-interactive: init at 0.1.0`
  (`pkgs/by-name/ni/nix-interactive/package.nix`). The work lives in a worktree,
  `~/repos/nixpkgs-nix-interactive`, branch `nix-interactive-init`, pushed to the
  `Geekiac/nixpkgs` fork. CI passes on every job.
  - The package sets `__structuredAttrs = true` (required for new packages), runs a
    `versionCheckHook` install check, has `passthru.updateScript = nix-update-script { }`, and
    wraps only `nvd` and `git` (not `nix`, so the user's own nix, matching their daemon, is used).
  - nixpkgs' AI policy applies: commits carry `Assisted-by:` trailers (a `Co-authored-by:`
    trailer doesn't satisfy it) and the PR description discloses AI use.
  - Kept as a draft by choice: pkgs/README asks whether a new package is mature and used by
    more than a handful of people, and the project is brand new.
  - Lessons from CI: check against *current* upstream CONTRIBUTING.md / pkgs/README.md (the
    local checkout was too old to know the structuredAttrs rule), and never use GitHub's
    "Update branch" button, because the commit lint rejects merge commits. Rebase and
    `git push --force-with-lease` instead.
- **search.nixos.org**: PR [NixOS/nixos-search#1574](https://github.com/NixOS/nixos-search/pull/1574)
  adds the flake to `flakes/manual.toml` (clone at `~/repos/nixos-search`, branch
  `add-nix-interactive`). `flake-info` indexes it cleanly. Their criteria exclude programs
  already in nixpkgs, so the listing may be removed once the nixpkgs PR merges.

### Not done yet

- **nixpkgs PR is still a draft.** To mark it ready: rebase onto current master, review
  `package.nix` and the maintainer entry personally, tick the "Fits CONTRIBUTING.md" and
  "Follows the automation/AI policy" boxes, and optionally note who uses it.
- **nixos-search PR** is awaiting a maintainer's review and merge.
- **nix-config follow-ups** (set `system.configurationRevision` for exact commit links,
  install `nixi` from this flake) haven't been made.
- **New releases**: tag `vX.Y.Z`, then bump `version`, `hash` and `cargoHash` in the nixpkgs
  package, or let the update bot do it after merge.

## Context
`nvd diff` answers "what changed between these two closures", but only for two paths you
already picked, as static text. There's no way to browse generation history, flip between
pairs, filter, or see *why* something changed (which nix-config commit / flake.lock bump).
Goal: a terminal app that lists every generation (NixOS system, home-manager, arbitrary
store paths) and interactively shows package-level diffs between any two, tied back to
`~/repos/nix-config` git history. `~/repos/nix-interactive` is an empty repo (no commits).

Environment facts found:
- NixOS system profiles: `/nix/var/nix/profiles/system-{34..43}-link`; store name embeds
  the nixpkgs version (`nixos-system-<host>-26.11.20260926.e158d9e`).
- `nixos-version --json` exposes `nixpkgsRevision`; `system.configurationRevision` is **not**
  set in nix-config, so there's no exact commit link today.
- home-manager runs as a NixOS module (`flake.nix:15`), so HM generations live *inside*
  each system closure (`home-manager-generation` path), not as standalone profiles.
- nvd 0.2.4 installed; no Rust toolchain on PATH (will come from the flake devShell).

## Decisions (from Q&A)
- **UI:** full TUI. **Language:** Rust + ratatui (+ crossterm).
- **Scope:** system gens, HM gens, arbitrary store paths/`./result`, nix-config git history.
- **Diff engine:** own engine built on `nix path-info` JSON; nvd output as an extra view.

## Architecture (Rust crate, single binary `nix-interactive` / alias `nixi`)
```
src/
  main.rs          clap CLI: `nixi` (TUI), `nixi diff A B` (one-shot), `--profile PATH`, `--repo PATH`
  sources/
    mod.rs         trait GenerationSource -> Vec<Generation{id, label, store_path, created, meta}>
    profile.rs     scan any profile dir for `<name>-<N>-link` symlinks (system, HM standalone,
                   `nix profile` user profiles); created = symlink mtime
    hm_embedded.rs resolve `<system>/…/home-manager-generation` inside system closures
    adhoc.rs       user-supplied store paths / `./result` symlinks
  closure.rs       `nix path-info --recursive --json --closure-size <path>` -> parse into
                   map pname -> {versions, narSize}; cache results on disk
                   (~/.cache/nix-interactive/<hash>.json, keyed by store path — immutable, so
                   cache never invalidates)
  diff.rs          pure fn diff(a, b) -> {added, removed, upgraded, downgraded, rebuilt(same
                   version, new hash), size_delta}; pname/version split using the same
                   heuristic as nvd (first `-` followed by a digit)
  git.rs           correlate generations with ~/repos/nix-config commits (via `git` CLI or
                   gix): 1) exact via configurationRevision if present, 2) else match nixpkgs
                   rev from generation name against flake.lock history + nearest commit
                   before the generation's timestamp (marked "≈" in UI)
  nvd.rs           spawn `nvd diff A B`, capture ANSI output for the raw-view tab
  ui/
    app.rs         state machine: selected base/target, filter, focused pane, async jobs
    gen_list.rs    left pane: generations (source tabs: System | Home | Ad-hoc), current marked
    diff_view.rs   right pane: grouped, colored diff table + summary header (counts, Δsize)
    detail.rs      popup: package detail (all versions, store paths, why-depends shortcut)
    commits.rs     commit list between the two generations' correlated commits + `git diff
                   --stat` of flake.lock / *.nix
```
Closure queries run on a background thread (tokio or std::thread + channel) so the UI never
blocks; results are cached in memory and on disk.

## Key interactions
- `j/k` move, `space` mark base, `enter` mark target (default: target = selected, base = previous gen → "what did this switch change")
- `tab` cycle panes; `1/2/3` source tabs; `/` filter packages; `a/r/u/d` toggle added/removed/upgraded/downgraded
- `n` raw nvd view; `c` commits between gens; `w` run `nix why-depends` for selected pkg; `s` sort by name/size-delta; `q` quit

## Packaging
- `flake.nix` in the repo: `packages.default` via `crane` (or `rustPlatform.buildRustPackage`),
  `devShells.default` with rustc/cargo/clippy/rustfmt/rust-analyzer, runtime deps wrapped
  onto PATH (`nix`, `nvd`, `git`) via `makeWrapper`.
- Suggested follow-up in `~/repos/nix-config` (separate commit, optional): add
  `system.configurationRevision = self.rev or self.dirtyRev or null;` so git correlation is
  exact, and add this flake as an input to install `nixi`.

## Milestones
1. Scaffold: flake + devShell, cargo project, `nixi diff A B` printing a text diff (engine + cache) — verify parity with `nvd diff`.
2. Sources: profile scanning (system + any profile dir), ad-hoc paths, HM-embedded.
3. TUI: gen list + diff pane + filter/sort, background loading.
4. nvd raw view + package detail/why-depends popup.
5. nix-config git correlation + commits pane.
6. Polish: config file (`~/.config/nix-interactive/config.toml` for repo path / extra profiles), README, `--help`.

## Files to create
`flake.nix`, `flake.lock`, `Cargo.toml`, `src/**` (above), `tests/` (fixture JSON for
`diff.rs` / version-split), `README.md`, and `plan.md` (copy of this plan, as requested),
plus the Notes vault copy at `~/repos/Notes/claude_plans/nix-interactive-historical-diff-viewer_<ts>/plan.md`.

## Verification
- `nix develop -c cargo test` — unit tests for version splitting and diff classification against fixture closures.
- `nix develop -c cargo run -- diff /nix/var/nix/profiles/system-42-link /nix/var/nix/profiles/system-43-link`
  and compare added/removed/changed sets with `nvd diff` on the same pair.
- `nix build && ./result/bin/nixi` — browse gens 34–43, flip pairs, filter, open nvd view, check commit pane matches `git log` in nix-config around those dates.
- `nix flake check` (clippy + fmt checks wired in).

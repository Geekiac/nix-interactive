# nix-interactive — interactive historical diff viewer for Nix

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

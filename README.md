# nix-interactive

Interactive historical diff viewer for Nix generations (work in progress — see `plan.md`).

The binary is `nixi`. Run it with no arguments for the interactive viewer:

```sh
nixi                      # system and home-manager generations, side by side with diffs
nixi --home               # start on the home-manager tab
nixi --path ./result      # also browse ./result (repeatable) in a "paths" tab
```

The left pane lists generations; the right pane shows what changed. Unpinned, it shows
what the highlighted generation changed compared with the one before it, so walking the
list replays your history switch by switch. `space` pins the old side and `enter` the new
side for arbitrary comparisons, `/` filters packages, `s` sorts by size change,
`u d c a r b` show or hide upgraded, downgraded, changed, added, removed and rebuilt
packages, and `?` lists every key. Closures load in the background, newest first.

In the diff pane, `enter` opens a package's details: every store path on each side with
its size, and what directly requires it. `w` runs `nix why-depends` from the generation to
the selected path, showing how the package gets pulled in. `n` swaps the package table for
`nvd diff`'s own output (the Nix package bundles nvd).

There are also one-shot commands:

```sh
nixi diff                 # current system generation vs the one before it
nixi diff 40 43           # system generations by number
nixi diff 42 ./result     # numbers, profile links, ./result and store paths mix freely
nixi list                 # system generations, * = current

nixi list --home          # home-manager generations (NixOS module), per user
nixi diff --home          # current home-manager generation vs the previous different one
nixi diff --home 34 43    # home-manager as used by system generations 34 and 43

nixi list --profile ~/.local/state/nix/profiles/home-manager   # any other profile
```

With `--home`, generation numbers refer to system generations: home-manager configurations
are read from each system closure (`unit-home-manager-<user>.service`), and consecutive
system generations that left home-manager unchanged are listed as one range (`35-37`).
`--user` picks another user (default `$USER`). Standalone home-manager and `nix profile`
profiles work through `--profile`.

Diff output matches `nvd diff` line for line, plus a count of packages rebuilt with
unchanged versions. Closures are read with `nix path-info --recursive --json` and cached in
`~/.cache/nix-interactive/` (store paths are immutable, so the cache never goes stale;
`--no-cache` bypasses it).

## Development

```sh
nix develop          # rust toolchain
cargo test
nix flake check      # build + tests, clippy, rustfmt
```

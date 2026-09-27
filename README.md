# nix-interactive

Interactive historical diff viewer for Nix generations (work in progress — see `plan.md`).

The binary is `nixi`. So far it has nvd-compatible diffs and a generation list:

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

# nix-interactive

Interactive historical diff viewer for Nix generations (work in progress — see `plan.md`).

The binary is `nixi`. So far it has a one-shot, nvd-compatible diff. With no arguments it
compares the current system generation with the one before it (`--profile` picks another
profile):

```sh
nix run . -- diff
```

Or compare any two closures:

```sh
nix run . -- diff /nix/var/nix/profiles/system-42-link /nix/var/nix/profiles/system-43-link
```

Either side can be a profile link, `./result`, or any store path. Output matches `nvd diff`
line for line, plus a count of packages rebuilt with unchanged versions. Closures are read
with `nix path-info --recursive --json` and cached in `~/.cache/nix-interactive/` (store
paths are immutable, so the cache never goes stale; `--no-cache` bypasses it).

## Development

```sh
nix develop          # rust toolchain
cargo test
nix flake check      # build + tests, clippy, rustfmt
```

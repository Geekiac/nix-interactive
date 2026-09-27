# nix-interactive

An interactive, historical diff viewer for Nix generations. Walk through your NixOS and
home-manager generations and see, switch by switch, which packages changed, how the closure
grew or shrank, why a package is there, and which commits of your configuration did it.

The binary is `nixi`. Its package diffs match `nvd diff` line for line.

```
 1 system   2 home (alice)
┌ Generations ───────────────────────────────────────────────┐┌ system 40 → 41 ──────────────────────────────────────┐
│      39  2026-09-18 23:32  26.11.20260917.e554fab  ≈e748059││25 upgraded  0 downgraded  0 changed  2 added  …      │
│old   40  2026-09-19 13:36  26.11.20260917.e554fab  ≈6cbf44c││paths 2027 → 2027 (+134 −134)  disk -54.2MiB  …       │
│new   41  2026-09-26 12:34  26.11.20260925.e94cb15  ≈ab83f58││   Package          Old          New           Size   │
│      42  2026-09-27 12:10  26.11.20260926.e158d9e  ≈498d118││U* bind             9.20.26-…    9.20.29-…    +8.1KiB │
│    * 43  2026-09-27 12:16  26.11.20260926.e158d9e  ≈9465368││U. claude-code      2.1.272      2.1.280      +6.3MiB │
```

## Install

With flakes:

```sh
nix run github:Geekiac/nix-interactive           # try it
nix profile install github:Geekiac/nix-interactive
```

Or add it as an input to your NixOS/home-manager flake and put
`inputs.nix-interactive.packages.${system}.default` in your packages. The package brings
`nix`, `nvd` and `git` along as fallbacks; the ones already on your `PATH` win.

## The viewer

```sh
nixi                      # system and home-manager generations
nixi --home               # start on the home-manager tab
nixi --path ./result      # also browse ./result (repeatable), e.g. before switching
nixi --repo ~/nix-config  # link generations to commits (see below)
nixi --range -2:-1        # open on a range, as in `nixi diff` (see Commands)
```

The left pane lists generations (`*` is the current one); the right pane shows what changed.
Unpinned, it compares the highlighted generation with the one before it, so walking the list
replays your history one switch at a time. Pin either side to compare any two, or press `:`
and type a range like `nixi diff` takes (`-2:-1`, `-5:0`, `40:43`) to pin both at once.
The diff title shows the pair as a range, e.g. `system 42 → 43 (-1:0)`.

| Key | Action |
| --- | --- |
| `j`/`k`, `↑`/`↓`, `g`/`G`, `PgUp`/`PgDn` | move in the focused pane |
| `tab`, `h`/`l` | switch between the list and the diff |
| `1`–`9` | switch tab: system, home-manager, extra profiles, paths |
| `space` | pin the old side to the highlighted generation (again: unpin) |
| `enter` | in the list: pin the new side; in the diff: package details |
| `:` | compare a range: `-1` (= `-1:0`), `-2:-1`, `40:43`, … (pins both sides) |
| `esc` | clear the filter, else unpin both sides |
| `/` | filter packages by name |
| `s` | sort by name or by size change |
| `u` `d` `c` `a` `r` `b` | show/hide upgraded, downgraded, changed, added, removed, rebuilt |
| `w` | `nix why-depends` for the selected package |
| `n` | show `nvd diff`'s own output instead |
| `L` | show the configuration commits between the two generations |
| `?` | help |
| `q` | quit |

Rows are marked like nvd's: the change (`U`pgraded, `D`owngraded, `C`hanged, `A`dded,
`R`emoved, re`B`uilt with the same version) and whether the package is directly selected
(`*` selected, `+` newly selected, `-` newly unselected, `.` dependency). Rebuilt packages
are hidden by default; there are usually dozens per switch.

Package details list every store path of the package on both sides with its size, and what
directly requires it. `w` then shows the chain from the generation down to the package.

Home-manager generations are found inside each system generation when home-manager runs as
a NixOS module. Consecutive system generations that didn't change home-manager collapse into
one entry (`35-37`), and numbers refer to system generations. For standalone home-manager,
pass its profile with `--profile` or add it to the config file.

Closures are read with `nix path-info --recursive --json` in the background and cached in
`~/.cache/nix-interactive/`. Store paths never change, so the cache never goes stale.

## Commands

```sh
nixi diff                 # what the last switch changed: same as -1:0
nixi diff -2:-1           # the switch before that
nixi diff -5              # everything since five generations ago (same as -5:0)
nixi diff 40:43           # generations 40 and 43, by number
nixi diff 0:./result      # what switching to ./result would change
nixi diff --home          # the last home-manager change
nixi list                 # generations with dates (and commits, with --repo)
nixi config               # config file location and the settings in effect
```

`diff` takes one `OLD:NEW` range. Each side is `0` (the current generation) or `-N` (N
generations before it), a positive generation number, or a path: a profile link, `./result`,
or a store path. A single side is compared with the current generation, so `-1` means `-1:0`. Counting back skips numbers removed by garbage collection; with `--home` it
steps through distinct home-manager generations.

`--profile`, `--home`, `--user` and `--repo` work with the viewer and every command.
`--color` controls colors (`auto` by default, honoring `NO_COLOR`), and `--no-cache` skips
the closure cache.

## Linking generations to commits

Point nixi at your configuration repo to see which commit each generation came from:

```sh
nixi --repo ~/nix-config        # or NIXI_REPO=~/nix-config, or `repo` in the config file
```

The list gains a commit column, the diff summary shows the commit range, and `L` shows
`git log --stat` between the two generations' commits. `nixi list` and `nixi diff` include
commits too.

Links are marked by how sure they are:

- `=` the generation records its commit (`system.configurationRevision`); `+` if it was
  built from a dirty tree on top of that commit
- `≈` the newest commit made before the generation, and its `flake.lock` pins the same
  nixpkgs the generation was built from
- `?` the newest commit made before the generation, unconfirmed

Without a recorded revision, links assume you commit before switching. For exact links,
record the revision in your flake's NixOS configuration:

```nix
system.configurationRevision = self.rev or self.dirtyRev or null;
```

## Configuration

`~/.config/nix-interactive/config.toml` (or `--config PATH`) sets defaults. Flags and
environment variables take precedence. `nixi config --example` prints this:

```toml
# Configuration repo, to link generations to commits (like --repo / NIXI_REPO).
repo = "~/repos/nix-config"

# Profile to browse (like --profile). Default: /nix/var/nix/profiles/system
# profile = "/nix/var/nix/profiles/system"

# User whose embedded home-manager generations to show (like --user). Default: $USER
# user = "alice"

# Extra profiles, each shown in its own tab. `--profile <name>` selects one on the
# command line, e.g. `nixi list --profile hm`.
# [[profiles]]
# name = "hm"
# path = "~/.local/state/nix/profiles/home-manager"
```

## Development

```sh
nix develop          # Rust toolchain
cargo test
cargo run -- diff
nix flake check      # build + tests, clippy, rustfmt
```

The design and milestones are in `plan.md`.

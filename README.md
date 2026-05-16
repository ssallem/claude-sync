# claude-sync

> Sync your `~/.claude/` across machines. Like chezmoi, but it understands `settings.json`.

[![CI](https://github.com/ssallem/claude-sync/actions/workflows/ci.yml/badge.svg)](https://github.com/ssallem/claude-sync/actions/workflows/ci.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

<!-- TODO: demo.gif -->

## Why

If you use Claude Code on more than one machine, your `~/.claude/` folder drifts. Agents, hooks, skills, `settings.json` — they get hand-copied, forgotten, or quietly diverge across PCs. Generic dotfile managers don't help much because they treat `settings.json` as an opaque blob.

`claude-sync` makes `~/.claude/` a git repo with:

- **Sensible defaults** for what to track (agents, hooks, skills, rules, `CLAUDE.md`, `settings.json`).
- **Automatic exclusion** of `projects/`, OAuth tokens, and known secret patterns.
- **`settings.json` deep merge** — three-way merge with conflict markers, not a "last write wins" overwrite.

## Installation

`claude-sync` is not yet published to crates.io. Pick one of:

**Prebuilt binary (recommended).** Grab the latest `claude-sync` executable for your OS from the [Releases page](https://github.com/ssallem/claude-sync/releases) and drop it on your `PATH`.

**Install from git with cargo.**

```sh
cargo install --git https://github.com/ssallem/claude-sync
```

**Build from source.**

```sh
git clone https://github.com/ssallem/claude-sync
cd claude-sync
cargo build --release
# binary at target/release/claude-sync
```

## Quick start

```sh
claude-sync init https://github.com/you/dotclaude.git
# edit ~/.claude/... as usual
claude-sync push

# on another machine
claude-sync init https://github.com/you/dotclaude.git
claude-sync pull
```

## What it tracks

- `agents/`, `hooks/`, `skills/`, `rules/`, `commands/`
- `CLAUDE.md`, `MEMORY.md`, top-level `*.md`
- `settings.json` (deep-merged, not overwritten)

## What it ignores

- `projects/` (per-project chat history, large and private)
- `.credentials.json`, OAuth tokens, anything matching built-in secret patterns
- Anything matched by your `.stowignore` (gitignore-style)
- `todos/`, `shell-snapshots/`, `statsig/`, log files, caches

## Commands

| Command | What it does |
|---------|--------------|
| `claude-sync init <remote>` | Initialize `~/.claude/` as a git repo against `<remote>` (clones if remote is non-empty). |
| `claude-sync push [-m MSG]` | Stage tracked files, scan for secrets, commit, push. |
| `claude-sync pull` | Fetch remote, 3-way merge `settings.json`, fast-forward or merge other files. |
| `claude-sync status` | Show changed files, ahead/behind counts, secret-scan summary. |
| `claude-sync doctor` | Diagnose env: git presence, remote reachability, settings.json validity, secret hits. |

## vs chezmoi

| | `claude-sync` | chezmoi |
|---|---|---|
| Scope | `~/.claude/` only | Any dotfiles |
| `settings.json` handling | JSON deep-merge with conflict markers | Generic text / template |
| Secret handling | Auto-redact 13 built-in patterns | Manual (`chezmoi encrypt` opt-in) |
| Default ignore list | Domain-aware (`projects/`, tokens) | Generic |
| Binary | Single static binary | Single static binary |

`chezmoi` is the right answer for general dotfiles. `claude-sync` is the right answer when the file you actually care about is `settings.json` and you don't want to think about it.

## Roadmap

- `v0.2` — `cargo-dist` prebuilt binaries, SOPS-based secret encryption, `MEMORY.md` semantic merge, `claude-sync keep` / `claude-sync revert` commands.
- Later — Homebrew / Scoop taps, optional GUI status indicator.

## Contributing

PRs welcome. See open issues.

## License

Dual-licensed under either of:

- MIT license ([LICENSE-MIT](LICENSE-MIT) or <https://opensource.org/licenses/MIT>)
- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or <https://www.apache.org/licenses/LICENSE-2.0>)

at your option.

# Changelog

All notable changes to `claude-sync` are documented in this file. The format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- `init` performs a best-effort `git fetch` after `remote add`, so `status`
  reports `ahead`/`behind` immediately on the very first run instead of
  stalling at `(unborn)`/0. Uses the remote's default refspec to cover both
  `main` and `master` defaults. Silent on failure to keep empty-remote init
  valid.

### Changed
- `.stowignore.default` expanded from 21 to ~50 lines, adding
  `.credentials.json`, `history.jsonl`, `daemon/`, `daemon.lock`,
  `daemon.status.json`, `sessions/`, `session-env/`, `session-data/`,
  `image-cache/`, `paste-cache/`, `backups/`, `ide/`, `.serena/`,
  `plugins/marketplaces/`, `.last-cleanup`, `mcp-needs-auth-cache.json`,
  `debug/`, `jobs/`, `file-history/`, `shell-snapshots/` to align with the
  UI's `DEFAULT_STOWIGNORE`. Leading-dot files like `.credentials.json` were
  not matched by the previous `credentials*` glob.

### Tests
- 4 new cargo tests: `default_excludes_dot_credentials`,
  `default_excludes_history_and_daemon`,
  `default_does_not_match_normal_sync_paths`,
  `try_initial_fetch_does_not_panic_on_no_remote`.

Candidates targeted for `v0.2`:

- Secret scanner: detect UTF-16-encoded files (BOM-aware) so leaks in
  Windows-native text aren't silently skipped.
- Auth: explicit SSH known-hosts certificate-check callback instead of
  relying on the default agent behaviour.
- Repo hygiene: ship a default `.gitattributes` so line-ending normalisation
  is consistent across Windows / macOS / Linux checkouts.

## [0.1.2] - 2026-05-17

Hotfix release addressing review findings from `v0.1.1`.

### Fixed
- `push`: corrected misleading comment in `stage_files` about
  `index.write()` lifecycle. The on-disk `.git/index` is updated once
  `stage_files` finishes; if a later step fails, the next push's
  `index.clear()` + re-stage flow self-heals the state.
- `pull`: fallback conflict marker now uses the current branch name
  (`>>>>>>> origin/<branch>`) instead of hardcoded `origin/main`, matching
  the dynamic-branch behaviour added in `v0.1.1`.
- `ci`: `cargo build` and `cargo test` jobs now pass `--locked` so CI fails
  fast if `Cargo.lock` drifts from `Cargo.toml`.

### Documentation
- `README`: clarified that `claude-sync init` always seeds the first
  commit on the `main` branch regardless of the user's
  `init.defaultBranch` setting; subsequent `push` / `pull` use the current
  branch.

## [0.1.1] - 2026-05-17

Patch release addressing the post-v0.1.0 critical review.

### Added
- Secret patterns: `gitlab_pat`, `aws_access_key`, `slack_token`,
  `huggingface_token`, and `github_pat_fine_grained` (real `github_pat_*`
  prefix). Total built-in patterns: 13.
- `CHANGELOG.md`.

### Changed
- Renamed the `gho_*` pattern from `github_pat_fine_grained` to
  `github_oauth_token` so leak reports name the credential type correctly.
- `claude-sync pull` now creates a real two-parent merge commit after a clean
  3-way merge, instead of leaving the worktree merged but uncommitted.
- `claude-sync pull` conflict guidance now explicitly names the `_conflicts`
  array key and the standard `<<<<<<<`/`=======`/`>>>>>>>` markers.
- `claude-sync push` and `claude-sync pull` use the current HEAD branch name
  instead of hardcoded `main` (init still seeds the first commit on `main`).
- `claude-sync doctor` ignore-files check now requires both `.stowignore` and
  `.gitignore` for OK; one missing → WARN; both missing → FAIL.
- README: replaced `cargo install claude-sync` with prebuilt binary /
  `cargo install --git` / build-from-source instructions (not on crates.io
  yet). Removed broken crates.io badge.

### Fixed
- `src/stowignore.rs`: removed dead `dir_only` branch whose two arms produced
  identical output.
- `.gitignore`: dropped the internal-process comment line above `.claude/`.

## [0.1.0] - 2026-05-16

Initial public release.

### Added
- `claude-sync init <remote>` — initialize `~/.claude/` as a git repo with
  sensible default `.gitignore` and `.stowignore`.
- `claude-sync push [-m MSG]` — secret pre-scan, stage, commit, push to
  `origin/main`.
- `claude-sync pull` — fetch + 3-way merge with JSON deep-merge of
  `settings.json` and conflict markers for unresolved keys.
- `claude-sync status` — show changed files, ahead/behind, and inline secret
  hits.
- `claude-sync doctor` — diagnose environment: git binary, identity, remote,
  ignore files, recognized subfolders, secrets.
- Secret detection for 8 patterns (GitHub PAT/OAuth, Anthropic, OpenAI,
  Google, JSON-keyed Anthropic/OpenAI/OAuth).
- Default `.stowignore` excludes `projects/`, tokens, logs, caches.

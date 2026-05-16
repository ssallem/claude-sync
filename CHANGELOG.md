# Changelog

All notable changes to `claude-sync` are documented in this file. The format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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

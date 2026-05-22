use std::path::{Path, PathBuf};

use anyhow::{Context, anyhow};
use git2::{Repository, StatusOptions};
use walkdir::WalkDir;

use crate::commands::util;
use crate::secrets::{self, SecretMatch};
use crate::stowignore;

pub fn run() -> anyhow::Result<()> {
    let claude_dir = util::claude_dir()?;
    let repo = match util::open_repo(&claude_dir) {
        Ok(r) => r,
        Err(_) => {
            println!("Not initialized. Run `claude-sync init <remote>` first.");
            return Ok(());
        }
    };

    let stow = stowignore::load(&claude_dir).context("load stowignore rules")?;
    let changes = collect_changes(&repo, &stow, &claude_dir)?;
    report_changes(&changes);

    let counts = count_tracking(&claude_dir, &repo, &stow);
    println!(
        "Tracking {} file(s) (excluded: {} by .stowignore / {} by .gitignore)",
        counts.tracked, counts.stow_excluded, counts.git_excluded
    );

    let scan_targets = changes_to_scan(&claude_dir, &changes);
    report_secrets(&scan_targets);

    report_ahead_behind(&repo)?;
    Ok(())
}

struct ChangeEntry {
    code: char,
    rel: String,
}

fn collect_changes(
    repo: &Repository,
    stow: &stowignore::Stowignore,
    claude_dir: &Path,
) -> anyhow::Result<Vec<ChangeEntry>> {
    let mut opts = StatusOptions::new();
    opts.include_untracked(true).include_ignored(false);
    let statuses = repo.statuses(Some(&mut opts)).context("read repo status")?;

    let mut out = Vec::new();
    for s in statuses.iter() {
        let st = s.status();
        // Ordered most-specific first so workdir mods don't mask staged adds.
        let code = if st.is_index_new() || st.is_wt_new() {
            // Distinguish committed-but-new (A) from purely untracked (?).
            if st.is_index_new() { 'A' } else { '?' }
        } else if st.is_index_deleted() || st.is_wt_deleted() {
            'D'
        } else if st.is_index_modified() || st.is_wt_modified() {
            'M'
        } else {
            continue;
        };
        let Some(path) = s.path() else { continue };
        // Filter out stowignore matches so secrets like `.credentials.json`
        // never leak into the user-visible change list (and never get scanned
        // / pushed downstream).
        let abs = claude_dir.join(path);
        if stow.is_ignored(&abs, claude_dir) {
            continue;
        }
        out.push(ChangeEntry {
            code,
            rel: path.to_string(),
        });
    }
    Ok(out)
}

fn report_changes(changes: &[ChangeEntry]) {
    if changes.is_empty() {
        // Explicit message so callers (humans and the integration test) can grep
        // for a single literal string instead of inferring from absence.
        println!("Nothing changed");
        return;
    }
    println!("Changes:");
    for c in changes {
        println!("  {}  {}", c.code, c.rel);
    }
}

struct TrackingCounts {
    tracked: usize,
    stow_excluded: usize,
    git_excluded: usize,
}

fn count_tracking(
    claude_dir: &Path,
    repo: &Repository,
    stow: &stowignore::Stowignore,
) -> TrackingCounts {
    let mut tracked = 0usize;
    let mut stow_excluded = 0usize;
    let mut git_excluded = 0usize;

    for entry in WalkDir::new(claude_dir).into_iter().filter_entry(|e| {
        // `.git/` is repo metadata and never counted.
        !(e.depth() > 0 && e.file_name() == ".git")
    }) {
        let Ok(entry) = entry else { continue };
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if stow.is_ignored(path, claude_dir) {
            stow_excluded += 1;
            continue;
        }
        if repo.status_should_ignore(path).unwrap_or(false) {
            git_excluded += 1;
            continue;
        }
        tracked += 1;
    }

    TrackingCounts {
        tracked,
        stow_excluded,
        git_excluded,
    }
}

fn changes_to_scan(claude_dir: &Path, changes: &[ChangeEntry]) -> Vec<PathBuf> {
    changes
        .iter()
        // Deleted files have no on-disk content left to scan.
        .filter(|c| c.code != 'D')
        .map(|c| claude_dir.join(&c.rel))
        .collect()
}

fn report_secrets(targets: &[PathBuf]) {
    let mut found = Vec::<(PathBuf, SecretMatch)>::new();
    for path in targets {
        let Ok(matches) = secrets::scan_file(path) else {
            continue;
        };
        for m in matches {
            found.push((path.clone(), m));
        }
    }
    if found.is_empty() {
        return;
    }
    println!("!! SECRET DETECTED:");
    for (path, m) in &found {
        println!(
            "  {} L{}: {} — {}",
            path.display(),
            m.line,
            m.pattern_name,
            m.redacted_snippet
        );
    }
}

fn report_ahead_behind(repo: &Repository) -> anyhow::Result<()> {
    let head = match repo.head() {
        Ok(h) => h,
        Err(_) => {
            println!("Branch: (unborn)");
            return Ok(());
        }
    };
    let branch_name = head
        .shorthand()
        .map(|s| s.to_string())
        .unwrap_or_else(|| "(detached)".to_string());

    let local_oid = match head.target() {
        Some(o) => o,
        None => {
            println!("Branch: {branch_name}");
            return Ok(());
        }
    };

    let upstream_oid = upstream_oid(repo, &branch_name);
    let Some(upstream_oid) = upstream_oid else {
        println!("Branch: {branch_name} (no upstream)");
        return Ok(());
    };

    let (ahead, behind) = repo
        .graph_ahead_behind(local_oid, upstream_oid)
        .map_err(|e| anyhow!("ahead/behind: {e}"))?;
    println!("Branch: {branch_name} (ahead {ahead}, behind {behind})");
    Ok(())
}

fn upstream_oid(repo: &Repository, branch_name: &str) -> Option<git2::Oid> {
    // Prefer the local branch's configured upstream, then fall back to the
    // FETCH_HEAD recorded by the most recent `claude-sync pull`/`fetch`.
    if let Ok(branch) = repo.find_branch(branch_name, git2::BranchType::Local)
        && let Ok(up) = branch.upstream()
        && let Some(oid) = up.get().target()
    {
        return Some(oid);
    }
    repo.refname_to_id("FETCH_HEAD").ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn secret_files_excluded_from_collect_changes() {
        let tmp = tempfile::tempdir().expect("tmp");
        let repo = git2::Repository::init(tmp.path()).expect("git init");
        // Normal file and secret file side-by-side.
        fs::write(tmp.path().join("CLAUDE.md"), "# test").expect("write CLAUDE.md");
        fs::write(
            tmp.path().join(".credentials.json"),
            r#"{"key":"sk-ant"}"#,
        )
        .expect("write .credentials.json");
        let stow = crate::stowignore::load(tmp.path()).expect("load stowignore");
        let changes = collect_changes(&repo, &stow, tmp.path()).expect("collect_changes");
        let paths: Vec<&str> = changes.iter().map(|c| c.rel.as_str()).collect();
        assert!(
            paths.contains(&"CLAUDE.md"),
            "CLAUDE.md should appear in changes, got {:?}",
            paths
        );
        assert!(
            !paths.iter().any(|p| p.contains(".credentials.json")),
            ".credentials.json must be excluded by stowignore, got {:?}",
            paths
        );
    }
}

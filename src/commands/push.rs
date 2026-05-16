use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, anyhow};
use git2::{Diff, Index, ObjectType, RemoteCallbacks, Repository};
use walkdir::WalkDir;

use crate::commands::util;
use crate::secrets::{self, SecretMatch};
use crate::stowignore;

pub fn run(message: Option<&str>) -> anyhow::Result<()> {
    let claude_dir = util::claude_dir()?;
    let repo = util::open_repo(&claude_dir)?;

    let stow = stowignore::load(&claude_dir).context("load stowignore rules")?;
    let candidates =
        collect_candidates(&repo, &claude_dir, &stow).context("enumerate candidate files")?;

    let findings = scan_for_secrets(&candidates).context("secret pre-scan")?;
    if !findings.is_empty() {
        report_secrets(&findings);
        return Err(anyhow!(
            "Aborting push. Edit the files above or add them to ~/.claude/.stowignore, then retry."
        ));
    }

    let mut index = repo.index().context("open repo index")?;
    // Clear first so files newly added to stowignore/gitignore (or deleted from
    // disk) drop out of the index instead of lingering as stale entries.
    //
    // Safety: index.clear() and add_path() mutate the in-memory Index only. If
    // stage_files errors mid-loop we early-return before index.write() ever
    // runs, so the on-disk .git/index file keeps its previous contents and the
    // user can simply retry. The in-memory Index object is dropped at function
    // exit and is never reused after an error.
    index.clear().context("clear index")?;
    let staged =
        stage_files(&mut index, &candidates, &claude_dir).context("stage files into index")?;

    if !has_pending_changes(&repo, &mut index)? {
        println!("Nothing to push");
        return Ok(());
    }

    let msg = match message {
        Some(m) => m.to_string(),
        None => auto_message(&repo, &mut index).context("derive auto commit message")?,
    };

    let (commit_oid, short) = do_commit(&repo, &mut index, &msg).context("create commit")?;
    let branch_name = current_branch_name(&repo);
    do_push(&repo, &branch_name).with_context(|| format!("push to origin/{branch_name}"))?;

    println!("Pushed {} file(s) -> origin/{}", staged, branch_name);
    println!("  commit {short} {msg}");
    let _ = commit_oid; // silence unused warning across cfg variants
    Ok(())
}

/// Resolve the active branch's short name. Falls back to "main" only when HEAD
/// is unborn or detached — `do_commit` always lands the first commit on main,
/// so by the time we push we'll usually have a real branch shorthand.
fn current_branch_name(repo: &Repository) -> String {
    repo.head()
        .ok()
        .and_then(|h| h.shorthand().map(|s| s.to_string()))
        .unwrap_or_else(|| "main".to_string())
}

fn collect_candidates(
    repo: &Repository,
    claude_dir: &Path,
    stow: &stowignore::Stowignore,
) -> anyhow::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let walker = WalkDir::new(claude_dir).into_iter().filter_entry(|e| {
        // `.git/` is repo metadata, never a candidate.
        if e.depth() > 0 && e.file_name() == ".git" {
            return false;
        }
        if e.depth() == 0 {
            return true;
        }
        !stow.is_ignored(e.path(), claude_dir)
    });

    for entry in walker {
        let entry = entry.context("walking ~/.claude")?;
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        // git2 owns .gitignore semantics — defer to it instead of re-implementing.
        if repo.status_should_ignore(path).unwrap_or(false) {
            continue;
        }
        out.push(path.to_path_buf());
    }
    Ok(out)
}

fn scan_for_secrets(files: &[PathBuf]) -> anyhow::Result<Vec<(PathBuf, SecretMatch)>> {
    let mut findings = Vec::new();
    for path in files {
        let matches =
            secrets::scan_file(path).with_context(|| format!("scan {}", path.display()))?;
        for m in matches {
            findings.push((path.clone(), m));
        }
    }
    Ok(findings)
}

fn report_secrets(findings: &[(PathBuf, SecretMatch)]) {
    eprintln!(
        "Refusing to push: found {} potential secret(s):",
        findings.len()
    );
    for (path, m) in findings {
        eprintln!(
            "  {}:{}:{} [{}] {}",
            path.display(),
            m.line,
            m.column,
            m.pattern_name,
            m.redacted_snippet
        );
    }
}

fn stage_files(index: &mut Index, files: &[PathBuf], claude_dir: &Path) -> anyhow::Result<usize> {
    let mut count = 0usize;
    for path in files {
        let rel = path
            .strip_prefix(claude_dir)
            .with_context(|| format!("path outside claude dir: {}", path.display()))?;
        // git stores forward-slash paths regardless of host OS.
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        index
            .add_path(Path::new(&rel_str))
            .with_context(|| format!("git add {rel_str}"))?;
        count += 1;
    }
    index.write().context("write index")?;
    Ok(count)
}

fn has_pending_changes(repo: &Repository, index: &mut Index) -> anyhow::Result<bool> {
    let head_tree = util::head_tree(repo)?;
    let diff = diff_head_to_index(repo, head_tree.as_ref(), index)?;
    Ok(diff.deltas().len() > 0)
}

fn diff_head_to_index<'a>(
    repo: &'a Repository,
    head_tree: Option<&git2::Tree<'a>>,
    index: &mut Index,
) -> anyhow::Result<Diff<'a>> {
    repo.diff_tree_to_index(head_tree, Some(index), None)
        .context("diff HEAD vs index")
}

fn auto_message(repo: &Repository, index: &mut Index) -> anyhow::Result<String> {
    let head_tree = util::head_tree(repo)?;
    if head_tree.is_none() {
        return Ok("initial sync".to_string());
    }
    let diff = diff_head_to_index(repo, head_tree.as_ref(), index)?;
    Ok(format_diff_summary(&diff))
}

/// Group deltas by top-level path component and summarize add/modify/delete counts.
/// Example output: `sync: agents(+2) hooks(+1) settings.json(M)`.
fn format_diff_summary(diff: &Diff<'_>) -> String {
    // category -> (add, modify, delete)
    let mut buckets: BTreeMap<String, [u32; 3]> = BTreeMap::new();
    for delta in diff.deltas() {
        let path = delta.new_file().path().or_else(|| delta.old_file().path());
        let Some(path) = path else { continue };
        let category = top_segment(path);
        let slot = buckets.entry(category).or_insert([0, 0, 0]);
        match delta.status() {
            git2::Delta::Added | git2::Delta::Copied | git2::Delta::Untracked => slot[0] += 1,
            git2::Delta::Deleted => slot[2] += 1,
            _ => slot[1] += 1,
        }
    }

    if buckets.is_empty() {
        return "sync: no-op".to_string();
    }

    let parts: Vec<String> = buckets
        .iter()
        .map(|(name, [a, m, d])| format_bucket(name, *a, *m, *d))
        .collect();
    format!("sync: {}", parts.join(" "))
}

fn format_bucket(name: &str, add: u32, modify: u32, del: u32) -> String {
    let mut tags = Vec::new();
    if add > 0 {
        tags.push(format!("+{add}"));
    }
    if modify > 0 {
        // Single-letter M for compactness; counts are usually 1 for root files.
        tags.push(if modify == 1 {
            "M".to_string()
        } else {
            format!("M{modify}")
        });
    }
    if del > 0 {
        tags.push(format!("-{del}"));
    }
    format!("{name}({})", tags.join(" "))
}

fn top_segment(path: &Path) -> String {
    let s = path.to_string_lossy().replace('\\', "/");
    match s.split_once('/') {
        Some((head, _)) => head.to_string(),
        None => s,
    }
}

fn do_commit(
    repo: &Repository,
    index: &mut Index,
    message: &str,
) -> anyhow::Result<(git2::Oid, String)> {
    let sig = repo.signature().map_err(|_| {
        anyhow!("git user.name/email not configured. Run: git config --global user.name <name>")
    })?;
    let tree_oid = index.write_tree().context("write tree from index")?;
    let tree = repo.find_tree(tree_oid).context("find written tree")?;

    let parents = match repo.head().ok().and_then(|h| h.target()) {
        Some(oid) => vec![repo.find_commit(oid).context("find parent commit")?],
        None => {
            // Force the first commit onto refs/heads/main so the hardcoded push
            // refspec works regardless of the platform's init.defaultBranch.
            repo.set_head("refs/heads/main")
                .context("set HEAD to main")?;
            Vec::new()
        }
    };
    let parent_refs: Vec<&git2::Commit> = parents.iter().collect();

    let oid = repo
        .commit(Some("HEAD"), &sig, &sig, message, &tree, &parent_refs)
        .context("write commit")?;
    let short = short_oid(repo, oid);
    Ok((oid, short))
}

fn short_oid(repo: &Repository, oid: git2::Oid) -> String {
    repo.find_object(oid, Some(ObjectType::Commit))
        .and_then(|o| o.short_id())
        .ok()
        .and_then(|buf| buf.as_str().map(|s| s.to_string()))
        .unwrap_or_else(|| oid.to_string().chars().take(7).collect())
}

fn do_push(repo: &Repository, branch: &str) -> anyhow::Result<()> {
    let mut remote = repo
        .find_remote("origin")
        .context("no remote 'origin' — re-run init?")?;

    let mut cbs = RemoteCallbacks::new();
    cbs.credentials(util::auth_callback);

    let mut opts = git2::PushOptions::new();
    opts.remote_callbacks(cbs);

    let refspec = format!("refs/heads/{branch}:refs/heads/{branch}");
    remote
        .push(&[refspec.as_str()], Some(&mut opts))
        .map_err(|e| {
            anyhow!(
                "push failed: {e}. Hint: configure git credential helper or use HTTPS PAT / SSH agent."
            )
        })?;
    Ok(())
}

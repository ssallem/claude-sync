use std::collections::HashSet;
use std::path::Path;

use anyhow::{Context, anyhow};
use git2::{
    IndexConflict, IndexEntry, ObjectType, Oid, RemoteCallbacks, Repository, ResetType, Tree,
};

use crate::commands::util;
use crate::merge;

pub fn run() -> anyhow::Result<()> {
    let claude_dir = util::claude_dir()?;
    let repo = util::open_repo(&claude_dir)?;

    let branch_name = current_branch_name(&repo);
    fetch_origin_branch(&repo, &branch_name)?;

    let head_oid = match repo.head().ok().and_then(|h| h.target()) {
        Some(oid) => oid,
        // Unborn local branch: this is effectively the first sync after `init`,
        // so the user's intent is "seed from remote". Skip the dirty check —
        // the seed files written by init (.gitignore/.stowignore) would
        // otherwise wedge the workflow.
        None => return adopt_fetch_head(&repo, &branch_name),
    };
    // Past the unborn branch case: now refuse a true merge against a dirty
    // worktree, since that would silently drop the user's local edits.
    abort_if_dirty(&repo)?;
    let fetch_oid = repo
        .refname_to_id("FETCH_HEAD")
        .context("read FETCH_HEAD (did fetch return anything?)")?;

    if head_oid == fetch_oid {
        println!("Already up to date.");
        return Ok(());
    }

    let merge_base = repo
        .merge_base(head_oid, fetch_oid)
        .map_err(|_| anyhow!("Unrelated histories — manual fix needed"))?;

    if merge_base == head_oid {
        let count = fast_forward(&repo, fetch_oid, &branch_name)?;
        println!("Fast-forwarded {count} file(s)");
        return Ok(());
    }

    let stats = three_way_merge(&repo, &claude_dir, head_oid, fetch_oid, merge_base)?;
    finalize_merge(&repo, head_oid, fetch_oid, &stats)?;
    Ok(())
}

/// Resolve the active branch's short name. Falls back to "main" only when HEAD
/// is unborn or detached — both are unusual states for a sync repo but cheap to
/// handle safely.
fn current_branch_name(repo: &Repository) -> String {
    repo.head()
        .ok()
        .and_then(|h| h.shorthand().map(|s| s.to_string()))
        .unwrap_or_else(|| "main".to_string())
}

fn abort_if_dirty(repo: &Repository) -> anyhow::Result<()> {
    let statuses = repo.statuses(None).context("read repo status")?;
    if statuses.iter().any(|s| !s.status().is_ignored()) {
        return Err(anyhow!(
            "Uncommitted changes. Run `claude-sync push` first or stash."
        ));
    }
    Ok(())
}

fn fetch_origin_branch(repo: &Repository, branch: &str) -> anyhow::Result<()> {
    let mut remote = repo
        .find_remote("origin")
        .context("no remote 'origin' — re-run init?")?;
    let mut cbs = RemoteCallbacks::new();
    cbs.credentials(util::auth_callback);
    let mut opts = git2::FetchOptions::new();
    opts.remote_callbacks(cbs);
    remote
        .fetch(&[branch], Some(&mut opts), None)
        .map_err(|e| anyhow!("fetch failed: {e}"))?;
    Ok(())
}

/// Used when the local branch has no commits but FETCH_HEAD does — equivalent
/// to a fresh clone's first checkout.
fn adopt_fetch_head(repo: &Repository, branch: &str) -> anyhow::Result<()> {
    let fetch_oid = repo
        .refname_to_id("FETCH_HEAD")
        .context("read FETCH_HEAD")?;
    let count = fast_forward(repo, fetch_oid, branch)?;
    println!("Initialized from origin/{branch} ({count} file(s))");
    Ok(())
}

fn fast_forward(repo: &Repository, target: Oid, branch: &str) -> anyhow::Result<usize> {
    let commit = repo.find_commit(target).context("find FETCH_HEAD commit")?;
    let tree = commit.tree().context("peel FETCH_HEAD to tree")?;
    let file_count = count_tree_files(&tree);

    let ref_name = format!("refs/heads/{branch}");
    // Move the branch ref and HEAD before resetting working tree, so the
    // index/worktree state matches the new branch tip atomically.
    repo.reference(&ref_name, target, true, "claude-sync pull fast-forward")
        .with_context(|| format!("update {ref_name}"))?;
    repo.set_head(&ref_name)
        .with_context(|| format!("set HEAD to {branch}"))?;

    let obj = commit.into_object();
    repo.reset(&obj, ResetType::Hard, None)
        .context("hard reset to FETCH_HEAD")?;
    Ok(file_count)
}

fn count_tree_files(tree: &Tree<'_>) -> usize {
    let mut n = 0usize;
    let _ = tree.walk(git2::TreeWalkMode::PreOrder, |_, entry| {
        if entry.kind() == Some(git2::ObjectType::Blob) {
            n += 1;
        }
        git2::TreeWalkResult::Ok
    });
    n
}

#[derive(Default)]
struct MergeStats {
    auto_files: usize,
    json_conflicts: usize,
    text_conflicts: usize,
}

fn three_way_merge(
    repo: &Repository,
    claude_dir: &Path,
    head_oid: Oid,
    fetch_oid: Oid,
    base_oid: Oid,
) -> anyhow::Result<MergeStats> {
    let our_tree = repo.find_commit(head_oid)?.tree()?;
    let their_tree = repo.find_commit(fetch_oid)?.tree()?;
    let base_tree = repo.find_commit(base_oid)?.tree()?;
    let merged = repo
        .merge_trees(&base_tree, &our_tree, &their_tree, None)
        .context("merge_trees")?;

    let conflicted = collect_conflicted_paths(&merged)?;
    let mut stats = MergeStats::default();
    apply_auto_merged(
        repo,
        claude_dir,
        &merged,
        &our_tree,
        &conflicted,
        &mut stats,
    )?;
    apply_deletions(claude_dir, &merged, &our_tree, &conflicted)?;
    if merged.has_conflicts() {
        apply_conflicts(repo, claude_dir, &merged, &mut stats)?;
    }
    Ok(stats)
}

fn collect_conflicted_paths(index: &git2::Index) -> anyhow::Result<HashSet<Vec<u8>>> {
    let mut set = HashSet::new();
    if !index.has_conflicts() {
        return Ok(set);
    }
    for c in index.conflicts()? {
        let c = c?;
        for entry in [&c.ancestor, &c.our, &c.their].into_iter().flatten() {
            set.insert(entry.path.clone());
        }
    }
    Ok(set)
}

fn apply_auto_merged(
    repo: &Repository,
    claude_dir: &Path,
    merged: &git2::Index,
    our_tree: &Tree<'_>,
    conflicted: &HashSet<Vec<u8>>,
    stats: &mut MergeStats,
) -> anyhow::Result<()> {
    for entry in merged.iter() {
        if conflicted.contains(&entry.path) {
            continue;
        }
        let path_str = std::str::from_utf8(&entry.path)
            .with_context(|| format!("non-utf8 path in merged index: {:?}", entry.path))?;
        let our_id = our_tree.get_path(Path::new(path_str)).ok().map(|e| e.id());
        if our_id == Some(entry.id) {
            continue;
        }
        write_blob(repo, claude_dir, path_str, entry.id)?;
        stats.auto_files += 1;
    }
    Ok(())
}

fn write_blob(
    repo: &Repository,
    claude_dir: &Path,
    rel: &str,
    blob_oid: Oid,
) -> anyhow::Result<()> {
    let blob = repo.find_blob(blob_oid)?;
    let abs = claude_dir.join(rel);
    if let Some(parent) = abs.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&abs, blob.content()).with_context(|| format!("write {}", abs.display()))?;
    Ok(())
}

fn apply_deletions(
    claude_dir: &Path,
    merged: &git2::Index,
    our_tree: &Tree<'_>,
    conflicted: &HashSet<Vec<u8>>,
) -> anyhow::Result<()> {
    let our_paths = list_tree_paths(our_tree);
    for path in our_paths {
        if conflicted.contains(path.as_bytes()) {
            continue;
        }
        if merged.get_path(Path::new(&path), 0).is_some() {
            continue;
        }
        let abs = claude_dir.join(&path);
        if abs.exists() {
            std::fs::remove_file(&abs).with_context(|| format!("remove {}", abs.display()))?;
        }
    }
    Ok(())
}

fn list_tree_paths(tree: &Tree<'_>) -> Vec<String> {
    let mut out = Vec::new();
    let _ = tree.walk(git2::TreeWalkMode::PreOrder, |dir, entry| {
        if entry.kind() == Some(git2::ObjectType::Blob)
            && let Some(name) = entry.name()
        {
            out.push(format!("{dir}{name}"));
        }
        git2::TreeWalkResult::Ok
    });
    out
}

fn apply_conflicts(
    repo: &Repository,
    claude_dir: &Path,
    merged: &git2::Index,
    stats: &mut MergeStats,
) -> anyhow::Result<()> {
    for c in merged.conflicts()? {
        let c = c?;
        let rel = conflict_path(&c)?;
        let abs = claude_dir.join(&rel);
        if rel.to_ascii_lowercase().ends_with(".json") {
            apply_json_conflict(repo, &abs, &c)?;
            stats.json_conflicts += 1;
        } else {
            apply_text_conflict(repo, &abs, &c)?;
            stats.text_conflicts += 1;
        }
    }
    Ok(())
}

fn conflict_path(c: &IndexConflict) -> anyhow::Result<String> {
    let bytes = c
        .our
        .as_ref()
        .or(c.their.as_ref())
        .or(c.ancestor.as_ref())
        .map(|e| e.path.as_slice())
        .ok_or_else(|| anyhow!("conflict entry without any side"))?;
    let s =
        std::str::from_utf8(bytes).with_context(|| format!("non-utf8 conflict path: {bytes:?}"))?;
    Ok(s.to_string())
}

fn apply_json_conflict(repo: &Repository, abs: &Path, c: &IndexConflict) -> anyhow::Result<()> {
    let base = load_json_side(repo, c.ancestor.as_ref())?;
    let ours = load_json_side(repo, c.our.as_ref())?;
    let theirs = load_json_side(repo, c.their.as_ref())?;
    let outcome = merge::deep_merge(&base, &ours, &theirs);
    merge::write_with_conflict_markers(abs, &outcome)?;
    Ok(())
}

fn load_json_side(
    repo: &Repository,
    entry: Option<&IndexEntry>,
) -> anyhow::Result<serde_json::Value> {
    let Some(entry) = entry else {
        return Ok(serde_json::Value::Null);
    };
    let blob = repo.find_blob(entry.id)?;
    let text = std::str::from_utf8(blob.content())
        .with_context(|| format!("non-utf8 JSON blob {}", entry.id))?;
    serde_json::from_str(text).with_context(|| format!("parse JSON from blob {}", entry.id))
}

fn apply_text_conflict(repo: &Repository, abs: &Path, c: &IndexConflict) -> anyhow::Result<()> {
    if let Some(parent) = abs.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let (ancestor, ours, theirs) = match (&c.ancestor, &c.our, &c.their) {
        (Some(a), Some(o), Some(t)) => (a, o, t),
        // Modify/delete or add/add — write whichever side exists with a banner.
        _ => return write_fallback_conflict(repo, abs, c),
    };
    let result = repo
        .merge_file_from_index(ancestor, ours, theirs, None)
        .map_err(|e| anyhow!("merge_file_from_index for {}: {e}", abs.display()))?;
    std::fs::write(abs, result.content()).with_context(|| format!("write {}", abs.display()))?;
    Ok(())
}

fn write_fallback_conflict(repo: &Repository, abs: &Path, c: &IndexConflict) -> anyhow::Result<()> {
    let mut body = String::from("<<<<<<< claude-sync (modify/delete conflict)\n");
    if let Some(e) = &c.our {
        append_blob_text(repo, &mut body, e)?;
    }
    body.push_str("=======\n");
    if let Some(e) = &c.their {
        append_blob_text(repo, &mut body, e)?;
    }
    body.push_str(">>>>>>> origin/main\n");
    std::fs::write(abs, body).with_context(|| format!("write {}", abs.display()))?;
    Ok(())
}

fn append_blob_text(
    repo: &Repository,
    sink: &mut String,
    entry: &IndexEntry,
) -> anyhow::Result<()> {
    let blob = repo.find_blob(entry.id)?;
    match std::str::from_utf8(blob.content()) {
        Ok(s) => sink.push_str(s),
        Err(_) => sink.push_str("<binary content omitted>\n"),
    }
    Ok(())
}

/// After a 3-way merge, commit the result when clean (two-parent merge commit)
/// or leave conflicts on disk for the user to resolve before the next push.
fn finalize_merge(
    repo: &Repository,
    head_oid: Oid,
    fetch_oid: Oid,
    stats: &MergeStats,
) -> anyhow::Result<()> {
    let total_conflicts = stats.json_conflicts + stats.text_conflicts;
    if total_conflicts == 0 {
        let short = commit_merge(repo, head_oid, fetch_oid)?;
        println!(
            "Merged {} file(s) and committed as {short}",
            stats.auto_files
        );
        return Ok(());
    }
    println!(
        "Merged with conflicts in {} file(s) ({} JSON, {} text)",
        total_conflicts, stats.json_conflicts, stats.text_conflicts
    );
    if stats.json_conflicts > 0 {
        println!(
            "  JSON conflict files contain a `_conflicts` array key — review and remove it, then run `claude-sync push`."
        );
    }
    if stats.text_conflicts > 0 {
        println!(
            "  Text conflict files use standard <<<<<<< / ======= / >>>>>>> markers — resolve, then run `claude-sync push`."
        );
    }
    if stats.auto_files > 0 {
        println!("  ({} other file(s) auto-merged)", stats.auto_files);
    }
    Ok(())
}

/// Build the merge commit from the current on-disk worktree (which already
/// reflects auto-merged content) with both pre-merge tips as parents.
fn commit_merge(repo: &Repository, head_oid: Oid, fetch_oid: Oid) -> anyhow::Result<String> {
    let sig = repo.signature().map_err(|_| {
        anyhow!("git user.name/email not configured. Run: git config --global user.name <name>")
    })?;
    // Re-stage the worktree so the tree we commit reflects auto-merged files
    // written by `apply_auto_merged` / `apply_deletions`.
    let mut index = repo.index().context("open repo index")?;
    index.read(true).context("reload index from disk")?;
    index
        .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
        .context("re-stage merged worktree")?;
    // Remove anything that no longer exists on disk (e.g. files deleted on the
    // remote side and applied by `apply_deletions`).
    index
        .update_all(["*"].iter(), None)
        .context("drop deleted files from index")?;
    let tree_oid = index.write_tree().context("write merged tree")?;
    index.write().context("write index")?;
    let tree = repo.find_tree(tree_oid).context("find merged tree")?;
    let head_commit = repo.find_commit(head_oid).context("find HEAD commit")?;
    let fetch_commit = repo
        .find_commit(fetch_oid)
        .context("find FETCH_HEAD commit")?;
    let oid = repo
        .commit(
            Some("HEAD"),
            &sig,
            &sig,
            "merge: pull from origin",
            &tree,
            &[&head_commit, &fetch_commit],
        )
        .context("write merge commit")?;
    Ok(short_oid(repo, oid))
}

fn short_oid(repo: &Repository, oid: Oid) -> String {
    repo.find_object(oid, Some(ObjectType::Commit))
        .and_then(|o| o.short_id())
        .ok()
        .and_then(|buf| buf.as_str().map(|s| s.to_string()))
        .unwrap_or_else(|| oid.to_string().chars().take(7).collect())
}

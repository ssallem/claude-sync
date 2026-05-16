use std::path::PathBuf;

use anyhow::{Context, anyhow};
use walkdir::WalkDir;

use crate::commands::util;
use crate::secrets::{self, SecretMatch};
use crate::stowignore::{self, DEFAULT_STOWIGNORE};

const DEFAULT_GITIGNORE: &str = "\
projects/
*.token
oauth_*
credentials*
cache/
tmp/
.env*
";

pub fn run(remote: &str) -> anyhow::Result<()> {
    let claude_dir = util::claude_dir()?;

    if !claude_dir.exists() {
        return Err(anyhow!(
            "{} does not exist; nothing to initialize",
            claude_dir.display()
        ));
    }

    // Idempotent: opening succeeds iff a repo already exists at the path.
    if git2::Repository::open(&claude_dir).is_ok() {
        println!("already initialized: {}", claude_dir.display());
        return Ok(());
    }

    // Load ignore rules first — we want the same filter the eventual git index
    // would use, so scan results match what will actually get committed.
    let stow = stowignore::load(&claude_dir).context("load stowignore rules")?;

    let findings = scan_for_secrets(&claude_dir, &stow)?;
    if !findings.is_empty() {
        eprintln!(
            "Refusing to initialize: found {} potential secret(s) in {}:",
            findings.len(),
            claude_dir.display()
        );
        for (path, m) in &findings {
            eprintln!(
                "  {}:{}:{} [{}] {}",
                path.display(),
                m.line,
                m.column,
                m.pattern_name,
                m.redacted_snippet
            );
        }
        return Err(anyhow!(
            "remove or ignore the values above (e.g. via ~/.claude/.stowignore) before retrying"
        ));
    }

    let repo = git2::Repository::init(&claude_dir)
        .with_context(|| format!("git init {}", claude_dir.display()))?;

    let gi_path = claude_dir.join(".gitignore");
    if !gi_path.exists() {
        std::fs::write(&gi_path, DEFAULT_GITIGNORE)
            .with_context(|| format!("write {}", gi_path.display()))?;
    }

    let si_path = claude_dir.join(".stowignore");
    if !si_path.exists() {
        std::fs::write(&si_path, DEFAULT_STOWIGNORE)
            .with_context(|| format!("write {}", si_path.display()))?;
    }

    repo.remote("origin", remote)
        .with_context(|| format!("add remote origin={remote}"))?;

    println!(
        "Initialized {} as git repo, remote={}",
        claude_dir.display(),
        remote
    );
    Ok(())
}

fn scan_for_secrets(
    claude_dir: &std::path::Path,
    stow: &stowignore::Stowignore,
) -> anyhow::Result<Vec<(PathBuf, SecretMatch)>> {
    let mut findings = Vec::new();
    let walker = WalkDir::new(claude_dir).into_iter().filter_entry(|e| {
        // Never descend into an existing .git directory — that's metadata, not
        // user content.
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
        let matches = secrets::scan_file(entry.path())
            .with_context(|| format!("scan {}", entry.path().display()))?;
        for m in matches {
            findings.push((entry.path().to_path_buf(), m));
        }
    }
    Ok(findings)
}

use std::path::{Path, PathBuf};

use anyhow::{Context, anyhow};
use git2::{Cred, Repository};

/// Resolve the user's home directory. Honors `HOME` and `USERPROFILE` env vars
/// first so tools and tests can redirect the lookup without monkey-patching the
/// real user profile — `dirs::home_dir()` on Windows bypasses USERPROFILE and
/// goes straight to FOLDERID_Profile, which is impossible to override.
pub fn home_dir() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
    {
        return Some(p);
    }
    dirs::home_dir()
}

/// Resolve `~/.claude/` for the current user. Errors instead of guessing so we
/// never read or write the wrong directory on misconfigured systems.
pub fn claude_dir() -> anyhow::Result<PathBuf> {
    let home = home_dir().ok_or_else(|| anyhow!("could not determine home directory"))?;
    Ok(home.join(".claude"))
}

/// Open the sync repo or surface a friendly "not initialized" message —
/// callers should not need to know git2 error codes.
pub fn open_repo(claude_dir: &Path) -> anyhow::Result<Repository> {
    Repository::open(claude_dir)
        .map_err(|_| anyhow!("Not initialized. Run `claude-sync init <remote>` first."))
}

/// Resolve HEAD to its tree, treating "no commits yet" as `None` rather than
/// an error — that's a valid state on a fresh repo.
pub fn head_tree(repo: &Repository) -> anyhow::Result<Option<git2::Tree<'_>>> {
    match repo.head() {
        Ok(head) => {
            let tree = head.peel_to_tree().context("peel HEAD to tree")?;
            Ok(Some(tree))
        }
        Err(e)
            if e.code() == git2::ErrorCode::UnbornBranch
                || e.code() == git2::ErrorCode::NotFound =>
        {
            Ok(None)
        }
        Err(e) => Err(e).context("read HEAD"),
    }
}

/// Credential helper used for both push and fetch. Tries SSH agent, then the
/// configured credential helper, then a default-credential fallback (matters
/// for Windows manager-core which advertises DEFAULT for HTTPS).
pub fn auth_callback(
    url: &str,
    username_from_url: Option<&str>,
    allowed: git2::CredentialType,
) -> Result<Cred, git2::Error> {
    if allowed.contains(git2::CredentialType::SSH_KEY)
        && let Some(user) = username_from_url
        && let Ok(cred) = Cred::ssh_key_from_agent(user)
    {
        return Ok(cred);
    }
    if allowed.contains(git2::CredentialType::USER_PASS_PLAINTEXT) {
        let config = git2::Config::open_default()?;
        if let Ok(cred) = Cred::credential_helper(&config, url, username_from_url) {
            return Ok(cred);
        }
    }
    if allowed.contains(git2::CredentialType::DEFAULT) {
        return Cred::default();
    }
    Err(git2::Error::from_str(
        "no usable git credentials (tried ssh-agent and credential helper)",
    ))
}

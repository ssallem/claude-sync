//! End-to-end CLI tests. Each test runs the compiled `claude-sync` binary
//! against a throwaway tempdir that masquerades as `$HOME`, so no test touches
//! the developer's real `~/.claude`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use assert_cmd::prelude::*;
use tempfile::TempDir;

/// Create a tempdir, populate `<tmp>/.claude/` with a minimal fake tree, and
/// return both the tempdir guard and the claude path. The caller hands the
/// tempdir's path to child processes as `HOME` / `USERPROFILE`.
fn fake_home() -> (TempDir, PathBuf) {
    let tmp = tempfile::tempdir().expect("create tempdir");
    let claude = tmp.path().join(".claude");
    fs::create_dir_all(claude.join("agents")).expect("create agents dir");
    fs::write(
        claude.join("settings.json"),
        r#"{"theme":"dark","editor":{"font":12}}"#,
    )
    .expect("write settings.json");
    fs::write(claude.join("agents").join("hello.md"), "# hello\nbody\n").expect("write hello.md");
    (tmp, claude)
}

/// Build a `Command` for the compiled binary with the fake home environment
/// injected. We override BOTH `HOME` and `USERPROFILE` so the same test code
/// works on Unix and Windows — `dirs::home_dir()` consults the platform-native
/// variable.
fn claude_sync(home: &Path) -> Command {
    let mut cmd = Command::cargo_bin("claude-sync").expect("locate claude-sync binary");
    cmd.env("HOME", home).env("USERPROFILE", home);
    cmd
}

/// Most tests need a configured git identity so commits can be created.
/// Writing to the per-repo config keeps the system git config untouched.
fn set_git_identity(claude_dir: &Path) {
    let repo = git2::Repository::open(claude_dir).expect("open repo");
    let mut cfg = repo.config().expect("repo config");
    cfg.set_str("user.name", "Test User").expect("set name");
    cfg.set_str("user.email", "test@example.com")
        .expect("set email");
}

/// Initialize a bare repo to act as a fake remote, returning a git2-friendly
/// URL. Local paths with backslashes confuse libgit2's URL parser on Windows,
/// so we normalize to forward slashes.
fn init_bare_remote(parent: &Path, name: &str) -> String {
    let path = parent.join(name);
    git2::Repository::init_bare(&path).expect("init bare repo");
    path.to_string_lossy().replace('\\', "/")
}

#[test]
fn test_help_lists_all_commands() {
    let mut cmd = Command::cargo_bin("claude-sync").expect("bin");
    let assert = cmd.arg("--help").assert().success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).to_string();
    for name in ["init", "push", "pull", "status", "doctor"] {
        assert!(
            stdout.contains(name),
            "expected `{name}` in --help output, got:\n{stdout}"
        );
    }
}

#[test]
fn test_init_requires_remote() {
    let mut cmd = Command::cargo_bin("claude-sync").expect("bin");
    let assert = cmd.arg("init").assert().failure();
    let code = assert.get_output().status.code().unwrap_or(-1);
    assert_eq!(code, 2, "clap should exit 2 for missing required arg");
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).to_string();
    assert!(
        stderr.to_uppercase().contains("REMOTE"),
        "stderr should reference REMOTE arg, got:\n{stderr}"
    );
}

#[test]
fn test_init_on_clean_dir_creates_repo() {
    let (tmp, claude) = fake_home();
    let remote = "https://example.com/fake.git";
    claude_sync(tmp.path())
        .args(["init", remote])
        .assert()
        .success();

    assert!(claude.join(".git").is_dir(), ".git dir should exist");
    let repo = git2::Repository::open(&claude).expect("open repo");
    let origin = repo.find_remote("origin").expect("origin remote");
    assert_eq!(origin.url(), Some(remote));
}

#[test]
fn test_init_aborts_on_secret() {
    let (tmp, claude) = fake_home();
    // Overwrite with a fake but pattern-matching anthropic key.
    fs::write(
        claude.join("settings.json"),
        r#"{"anthropic_api_key": "sk-ant-FAKEKEYAAAAAAAAAAAAAAAAAAA"}"#,
    )
    .expect("write secret");

    let assert = claude_sync(tmp.path())
        .args(["init", "https://example.com/fake.git"])
        .assert()
        .failure();

    let code = assert.get_output().status.code().unwrap_or(-1);
    assert_eq!(code, 1, "secret should abort with exit 1");
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).to_lowercase();
    assert!(
        stderr.contains("secret") || stderr.contains("anthropic"),
        "stderr should mention the secret, got:\n{stderr}"
    );
    assert!(
        !claude.join(".git").is_dir(),
        "init must not create a repo when secrets are present"
    );
}

#[test]
fn test_push_pull_roundtrip() {
    let bare_parent = tempfile::tempdir().expect("bare parent");
    let remote_url = init_bare_remote(bare_parent.path(), "fake.git");

    // PC1: init, write a unique payload, push.
    let (pc1, pc1_claude) = fake_home();
    let payload = "{\"theme\":\"dark\",\"marker\":\"pc1-was-here\"}";
    fs::write(pc1_claude.join("settings.json"), payload).expect("payload");

    claude_sync(pc1.path())
        .args(["init", &remote_url])
        .assert()
        .success();
    set_git_identity(&pc1_claude);
    claude_sync(pc1.path()).arg("push").assert().success();

    // PC2: fresh home, init same remote, pull, expect the same payload.
    let (pc2, pc2_claude) = fake_home();
    claude_sync(pc2.path())
        .args(["init", &remote_url])
        .assert()
        .success();
    set_git_identity(&pc2_claude);
    claude_sync(pc2.path()).arg("pull").assert().success();

    let got =
        fs::read_to_string(pc2_claude.join("settings.json")).expect("read pulled settings.json");
    assert_eq!(got, payload, "PC2 should mirror PC1 after pull");
}

#[test]
fn test_pull_aborts_on_uncommitted() {
    // First push so HEAD is valid; pull's dirty check only runs once HEAD
    // exists (the unborn-branch case is treated as a first-time seed).
    let bare_parent = tempfile::tempdir().expect("bare parent");
    let remote_url = init_bare_remote(bare_parent.path(), "fake.git");
    let (tmp, claude) = fake_home();

    claude_sync(tmp.path())
        .args(["init", &remote_url])
        .assert()
        .success();
    set_git_identity(&claude);
    claude_sync(tmp.path()).arg("push").assert().success();

    // Now dirty a tracked file without committing.
    fs::write(claude.join("settings.json"), r#"{"theme":"light"}"#).expect("dirty edit");

    let assert = claude_sync(tmp.path()).arg("pull").assert().failure();
    let code = assert.get_output().status.code().unwrap_or(-1);
    assert_eq!(code, 1, "pull should exit 1 on dirty worktree");
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).to_string();
    assert!(
        stderr.contains("Uncommitted"),
        "stderr should mention 'Uncommitted', got:\n{stderr}"
    );
}

#[test]
fn test_status_clean_output() {
    let bare_parent = tempfile::tempdir().expect("bare parent");
    let remote_url = init_bare_remote(bare_parent.path(), "fake.git");
    let (tmp, claude) = fake_home();

    claude_sync(tmp.path())
        .args(["init", &remote_url])
        .assert()
        .success();
    set_git_identity(&claude);
    claude_sync(tmp.path()).arg("push").assert().success();

    let assert = claude_sync(tmp.path()).arg("status").assert().success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).to_string();
    assert!(
        stdout.contains("Nothing changed"),
        "status on clean repo should print 'Nothing changed', got:\n{stdout}"
    );
}

#[test]
fn test_doctor_on_uninitialized_dir() {
    let (tmp, _claude) = fake_home();
    let assert = claude_sync(tmp.path()).arg("doctor").assert().failure();

    let code = assert.get_output().status.code().unwrap_or(-1);
    assert_eq!(code, 1, "doctor should exit 1 when not initialized");
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).to_string();
    let last = stdout.lines().rfind(|l| !l.is_empty()).unwrap_or("");
    assert_eq!(
        last, "[FAIL]",
        "last non-empty line should be [FAIL], full stdout:\n{stdout}"
    );
}

#[test]
fn test_doctor_passes_after_init() {
    let bare_parent = tempfile::tempdir().expect("bare parent");
    let remote_url = init_bare_remote(bare_parent.path(), "fake.git");
    let (tmp, claude) = fake_home();

    claude_sync(tmp.path())
        .args(["init", &remote_url])
        .assert()
        .success();
    set_git_identity(&claude);

    let assert = claude_sync(tmp.path()).arg("doctor").assert().success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).to_string();
    let last = stdout.lines().rfind(|l| !l.is_empty()).unwrap_or("");
    assert!(
        last == "[PASS]" || last == "[WARN]",
        "last line should be [PASS] or [WARN], got `{last}`. Full stdout:\n{stdout}"
    );
}

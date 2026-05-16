use std::path::{Path, PathBuf};
use std::process::Command;

use git2::Repository;
use walkdir::WalkDir;

use crate::commands::util;
use crate::secrets;
use crate::stowignore;

/// Severity of a single diagnostic check.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Level {
    Ok,
    Warn,
    Fail,
}

struct CheckResult {
    level: Level,
    label: &'static str,
    detail: String,
}

pub fn run() -> anyhow::Result<()> {
    let mut results = Vec::<CheckResult>::new();
    results.push(check_git_binary());

    let home = util::home_dir();
    results.push(check_home_dir(home.as_ref()));

    let claude_dir = home.as_ref().map(|h| h.join(".claude"));
    results.push(check_claude_dir(claude_dir.as_ref()));

    let repo = claude_dir.as_ref().and_then(|d| Repository::open(d).ok());
    results.push(check_git_repo(claude_dir.as_ref(), repo.is_some()));

    if let Some(repo) = repo.as_ref() {
        results.push(check_user_identity(repo));
        results.push(check_origin_remote(repo));
    }
    if let Some(dir) = claude_dir.as_ref() {
        results.push(check_ignore_files(dir));
        results.push(check_subfolders(dir));
        if repo.is_some() {
            results.push(check_secrets(dir));
        }
    }

    for r in &results {
        println!("[{}] {} — {}", tag(r.level), r.label, r.detail);
    }
    print_verdict(&results);
    Ok(())
}

fn tag(level: Level) -> &'static str {
    match level {
        Level::Ok => "OK",
        Level::Warn => "WARN",
        Level::Fail => "FAIL",
    }
}

fn print_verdict(results: &[CheckResult]) {
    let has_fail = results.iter().any(|r| r.level == Level::Fail);
    let has_warn = results.iter().any(|r| r.level == Level::Warn);
    let verdict = if has_fail {
        "[FAIL]"
    } else if has_warn {
        "[WARN]"
    } else {
        "[PASS]"
    };
    println!("{verdict}");
    if has_fail {
        std::process::exit(1);
    }
}

fn check_git_binary() -> CheckResult {
    match Command::new("git").arg("--version").output() {
        Ok(out) if out.status.success() => CheckResult {
            level: Level::Ok,
            label: "git binary",
            detail: String::from_utf8_lossy(&out.stdout).trim().to_string(),
        },
        _ => CheckResult {
            level: Level::Warn,
            label: "git binary",
            // git2 covers most operations; the system git is only needed for
            // certain credential helpers, hence warn rather than fail.
            detail: "not found in PATH (some credential flows may break)".to_string(),
        },
    }
}

fn check_home_dir(home: Option<&PathBuf>) -> CheckResult {
    match home {
        Some(p) => CheckResult {
            level: Level::Ok,
            label: "home directory",
            detail: p.display().to_string(),
        },
        None => CheckResult {
            level: Level::Fail,
            label: "home directory",
            detail: "could not be resolved".to_string(),
        },
    }
}

fn check_claude_dir(dir: Option<&PathBuf>) -> CheckResult {
    match dir {
        Some(p) if p.exists() => CheckResult {
            level: Level::Ok,
            label: "~/.claude",
            detail: p.display().to_string(),
        },
        Some(p) => CheckResult {
            level: Level::Fail,
            label: "~/.claude",
            detail: format!("missing: {}", p.display()),
        },
        None => CheckResult {
            level: Level::Fail,
            label: "~/.claude",
            detail: "no home dir".to_string(),
        },
    }
}

fn check_git_repo(dir: Option<&PathBuf>, is_repo: bool) -> CheckResult {
    match (dir, is_repo) {
        (Some(_), true) => CheckResult {
            level: Level::Ok,
            label: "git repo",
            detail: "initialized".to_string(),
        },
        _ => CheckResult {
            level: Level::Fail,
            label: "git repo",
            detail: "not initialized (run `claude-sync init <remote>`)".to_string(),
        },
    }
}

fn check_user_identity(repo: &Repository) -> CheckResult {
    let cfg = match repo.config() {
        Ok(c) => c,
        Err(e) => {
            return CheckResult {
                level: Level::Fail,
                label: "git identity",
                detail: format!("read config: {e}"),
            };
        }
    };
    let name = cfg.get_string("user.name").ok();
    let email = cfg.get_string("user.email").ok();
    match (name, email) {
        (Some(n), Some(e)) => CheckResult {
            level: Level::Ok,
            label: "git identity",
            detail: format!("{n} <{e}>"),
        },
        _ => CheckResult {
            level: Level::Fail,
            label: "git identity",
            detail: "user.name/user.email not set".to_string(),
        },
    }
}

fn check_origin_remote(repo: &Repository) -> CheckResult {
    match repo.find_remote("origin") {
        Ok(r) => CheckResult {
            level: Level::Ok,
            label: "remote origin",
            detail: r.url().unwrap_or("(no url)").to_string(),
        },
        Err(_) => CheckResult {
            level: Level::Fail,
            label: "remote origin",
            detail: "not configured".to_string(),
        },
    }
}

fn check_ignore_files(dir: &Path) -> CheckResult {
    let has_stow = dir.join(".stowignore").exists();
    let has_git = dir.join(".gitignore").exists();
    match (has_stow, has_git) {
        (true, true) => CheckResult {
            level: Level::Ok,
            label: "ignore files",
            detail: ".stowignore and .gitignore both present".to_string(),
        },
        (false, false) => CheckResult {
            level: Level::Fail,
            label: "ignore files",
            detail: "missing both .stowignore and .gitignore (run `claude-sync init` to seed)"
                .to_string(),
        },
        (false, true) => CheckResult {
            level: Level::Warn,
            label: "ignore files",
            detail: ".stowignore missing (only .gitignore present)".to_string(),
        },
        (true, false) => CheckResult {
            level: Level::Warn,
            label: "ignore files",
            detail: ".gitignore missing (only .stowignore present)".to_string(),
        },
    }
}

fn check_subfolders(dir: &Path) -> CheckResult {
    const KNOWN: &[&str] = &["agents", "skills", "commands", "hooks"];
    let found: Vec<&str> = KNOWN
        .iter()
        .copied()
        .filter(|name| dir.join(name).is_dir())
        .collect();
    if found.is_empty() {
        CheckResult {
            level: Level::Warn,
            label: "structure",
            detail: "no recognized claude-code subfolders found".to_string(),
        }
    } else {
        CheckResult {
            level: Level::Ok,
            label: "structure",
            detail: format!("found: {}", found.join(", ")),
        }
    }
}

fn check_secrets(dir: &Path) -> CheckResult {
    let stow = match stowignore::load(dir) {
        Ok(s) => s,
        Err(e) => {
            return CheckResult {
                level: Level::Warn,
                label: "secrets scan",
                detail: format!("could not load stowignore: {e}"),
            };
        }
    };
    let findings = scan_tracked(dir, &stow);
    if findings.is_empty() {
        CheckResult {
            level: Level::Ok,
            label: "secrets scan",
            detail: "no secrets in tracked files".to_string(),
        }
    } else {
        let preview = findings
            .iter()
            .take(3)
            .map(|(p, name)| format!("{} [{}]", p.display(), name))
            .collect::<Vec<_>>()
            .join("; ");
        CheckResult {
            level: Level::Fail,
            label: "secrets scan",
            detail: format!("{} finding(s): {}", findings.len(), preview),
        }
    }
}

fn scan_tracked(dir: &Path, stow: &stowignore::Stowignore) -> Vec<(PathBuf, String)> {
    let mut out = Vec::new();
    let walker = WalkDir::new(dir).into_iter().filter_entry(|e| {
        if e.depth() > 0 && e.file_name() == ".git" {
            return false;
        }
        if e.depth() == 0 {
            return true;
        }
        !stow.is_ignored(e.path(), dir)
    });
    for entry in walker.flatten() {
        if !entry.file_type().is_file() {
            continue;
        }
        let Ok(matches) = secrets::scan_file(entry.path()) else {
            continue;
        };
        for m in matches {
            out.push((entry.path().to_path_buf(), m.pattern_name));
        }
    }
    out
}

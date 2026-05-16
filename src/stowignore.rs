use std::path::Path;

use anyhow::Context;
use regex::Regex;

/// Default patterns baked into the binary. Lives at project root so it ships
/// with `cargo install` users too.
pub const DEFAULT_STOWIGNORE: &str = include_str!("../.stowignore.default");

struct PatternRule {
    regex: Regex,
    negate: bool,
}

pub struct Stowignore {
    patterns: Vec<PatternRule>,
}

pub fn load(claude_dir: &Path) -> anyhow::Result<Stowignore> {
    let mut patterns = parse(DEFAULT_STOWIGNORE).context("parse default stowignore")?;

    let local = claude_dir.join(".stowignore");
    if local.exists() {
        let content =
            std::fs::read_to_string(&local).with_context(|| format!("read {}", local.display()))?;
        // Local rules come after defaults so they can override (last match wins).
        patterns.extend(parse(&content).context("parse local stowignore")?);
    }

    Ok(Stowignore { patterns })
}

impl Stowignore {
    pub fn is_ignored(&self, path: &Path, claude_dir: &Path) -> bool {
        let rel = match path.strip_prefix(claude_dir) {
            Ok(r) => r,
            // Outside the tree — not our business.
            Err(_) => return false,
        };
        if rel.as_os_str().is_empty() {
            return false;
        }
        // Normalize Windows backslashes to forward slashes so patterns are
        // portable across platforms.
        let rel_str = rel.to_string_lossy().replace('\\', "/");

        let mut ignored = false;
        for rule in &self.patterns {
            if rule.regex.is_match(&rel_str) {
                ignored = !rule.negate;
            }
        }
        ignored
    }
}

fn parse(content: &str) -> anyhow::Result<Vec<PatternRule>> {
    let mut out = Vec::new();
    for raw in content.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (negate, body) = match line.strip_prefix('!') {
            Some(rest) => (true, rest),
            None => (false, line),
        };
        let re_src = glob_to_regex(body);
        let regex =
            Regex::new(&re_src).with_context(|| format!("invalid stowignore pattern: {body}"))?;
        out.push(PatternRule { regex, negate });
    }
    Ok(out)
}

/// Translate a gitignore-style glob to a regex anchored with `^...$`.
///
/// Supports `*`, `**`, `?`, leading `/` for root anchoring, trailing `/` for
/// directory-only matching. Patterns with no embedded slash match at any depth.
fn glob_to_regex(pattern: &str) -> String {
    let (anchored, p) = match pattern.strip_prefix('/') {
        Some(rest) => (true, rest),
        None => (false, pattern),
    };
    let (dir_only, p) = match p.strip_suffix('/') {
        Some(rest) => (true, rest),
        None => (false, p),
    };

    let has_slash = p.contains('/');

    let mut out = String::from("^");
    if !anchored && !has_slash {
        // Match at any depth, e.g. `*.log` matches `a.log` and `sub/a.log`.
        out.push_str("(?:.*/)?");
    }

    let bytes = p.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        match c {
            '*' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'*' {
                    // `**/foo` -> zero or more directory segments.
                    if i + 2 < bytes.len() && bytes[i + 2] == b'/' {
                        out.push_str("(?:.*/)?");
                        i += 3;
                        continue;
                    }
                    // Bare `**` or trailing `**` -> anything including slashes.
                    out.push_str(".*");
                    i += 2;
                    continue;
                }
                out.push_str("[^/]*");
            }
            '?' => out.push_str("[^/]"),
            // Regex metacharacters that must be escaped.
            '.' | '+' | '(' | ')' | '|' | '[' | ']' | '{' | '}' | '^' | '$' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
        i += 1;
    }

    if dir_only {
        // `cache/` matches the directory itself and everything inside.
        out.push_str("(?:/.*)?$");
    } else {
        // Files match exactly; directories match themselves and contents.
        out.push_str("(?:/.*)?$");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn ignore_with(rules: &str) -> Stowignore {
        Stowignore {
            patterns: parse(rules).expect("parse"),
        }
    }

    fn is_ignored(rules: &str, rel: &str) -> bool {
        let s = ignore_with(rules);
        let root = PathBuf::from("/root");
        s.is_ignored(&root.join(rel), &root)
    }

    #[test]
    fn matches_simple_glob() {
        assert!(is_ignored("*.log", "a.log"));
        assert!(is_ignored("*.log", "sub/dir/a.log"));
        assert!(!is_ignored("*.log", "a.txt"));
    }

    #[test]
    fn matches_dir_prefix() {
        assert!(is_ignored("projects/**", "projects/foo"));
        assert!(is_ignored("projects/**", "projects/foo/bar.json"));
        assert!(!is_ignored("projects/**", "other/foo"));
    }

    #[test]
    fn matches_trailing_slash_dir() {
        assert!(is_ignored("cache/", "cache"));
        assert!(is_ignored("cache/", "cache/x.bin"));
        assert!(is_ignored("cache/", "sub/cache/x.bin"));
    }

    #[test]
    fn negation_overrides() {
        let rules = "*.log\n!keep.log\n";
        assert!(is_ignored(rules, "a.log"));
        assert!(!is_ignored(rules, "keep.log"));
    }

    #[test]
    fn skips_comments_and_blanks() {
        let rules = "# comment\n\n*.tmp\n";
        assert!(is_ignored(rules, "a.tmp"));
    }

    fn load_in(claude_dir: &Path) -> Stowignore {
        load(claude_dir).expect("load defaults")
    }

    #[test]
    fn default_excludes_projects() {
        let tmp = tempfile::tempdir().expect("tmp");
        let s = load_in(tmp.path());
        assert!(s.is_ignored(&tmp.path().join("projects/foo.jsonl"), tmp.path()));
    }

    #[test]
    fn default_excludes_tokens() {
        let tmp = tempfile::tempdir().expect("tmp");
        let s = load_in(tmp.path());
        assert!(s.is_ignored(&tmp.path().join("auth.token"), tmp.path()));
    }

    #[test]
    fn negation_pattern_works() {
        let tmp = tempfile::tempdir().expect("tmp");
        // Local stowignore overrides — last match wins.
        std::fs::write(tmp.path().join(".stowignore"), "*.log\n!keep.log\n").expect("write");
        let s = load_in(tmp.path());
        assert!(s.is_ignored(&tmp.path().join("a.log"), tmp.path()));
        assert!(!s.is_ignored(&tmp.path().join("keep.log"), tmp.path()));
    }

    #[test]
    fn tracked_files_are_not_ignored() {
        let tmp = tempfile::tempdir().expect("tmp");
        let s = load_in(tmp.path());
        assert!(!s.is_ignored(&tmp.path().join("agents/foo.md"), tmp.path()));
        assert!(!s.is_ignored(&tmp.path().join("settings.json"), tmp.path()));
    }

    #[test]
    fn double_star_matches_nested() {
        let tmp = tempfile::tempdir().expect("tmp");
        let s = load_in(tmp.path());
        assert!(s.is_ignored(&tmp.path().join("projects/sub/dir/file.jsonl"), tmp.path()));
    }
}

use std::path::Path;
use std::sync::LazyLock;

use anyhow::Context;
use regex::Regex;

#[derive(Debug, Clone)]
pub struct SecretMatch {
    pub pattern_name: String,
    pub line: usize,
    pub column: usize,
    pub redacted_snippet: String,
}

// Patterns are compiled once. Order matters only for reporting, not detection;
// every pattern is evaluated against every line.
static PATTERNS: LazyLock<Vec<(&'static str, Regex)>> = LazyLock::new(|| {
    let raw: &[(&str, &str)] = &[
        ("github_pat_fine_grained", r"gho_[A-Za-z0-9]{36,}"),
        ("github_pat_classic", r"ghp_[A-Za-z0-9]{36,}"),
        ("anthropic_api_key", r"sk-ant-[A-Za-z0-9_\-]{20,}"),
        ("openai_style_key", r"sk-[A-Za-z0-9]{20,}"),
        (
            "json_anthropic_api_key",
            r#"(?i)"anthropic_api_key"\s*:\s*"[^"]+""#,
        ),
        (
            "json_openai_api_key",
            r#"(?i)"openai_api_key"\s*:\s*"[^"]+""#,
        ),
        ("json_oauth_token", r#"(?i)"oauth_token"\s*:\s*"[^"]+""#),
        ("google_api_key", r"AIza[0-9A-Za-z_\-]{35}"),
    ];
    raw.iter()
        .map(|(name, pat)| {
            let re = Regex::new(pat).expect("built-in secret regex must compile");
            (*name, re)
        })
        .collect()
});

pub fn scan_text(content: &str) -> Vec<SecretMatch> {
    let mut out = Vec::new();
    for (line_idx, line) in content.lines().enumerate() {
        for (name, re) in PATTERNS.iter() {
            for m in re.find_iter(line) {
                out.push(SecretMatch {
                    pattern_name: (*name).to_string(),
                    line: line_idx + 1,
                    column: m.start() + 1,
                    redacted_snippet: redact(m.as_str()),
                });
            }
        }
    }
    out
}

pub fn scan_file(path: &Path) -> anyhow::Result<Vec<SecretMatch>> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => {
            return Err(e).with_context(|| format!("read {}", path.display()));
        }
    };

    // Treat anything with a NUL byte in the head as binary. Cheap and matches
    // what git itself does.
    let probe_len = bytes.len().min(8192);
    if bytes[..probe_len].contains(&0u8) {
        return Ok(Vec::new());
    }

    let content = match std::str::from_utf8(&bytes) {
        Ok(s) => s,
        Err(_) => return Ok(Vec::new()),
    };

    Ok(scan_text(content))
}

fn redact(matched: &str) -> String {
    let chars: Vec<char> = matched.chars().collect();
    if chars.len() < 6 {
        return "***".to_string();
    }
    let first: String = chars[..4].iter().collect();
    let last: String = chars[chars.len() - 2..].iter().collect();
    format!("{first}***{last}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn redacts_short_matches() {
        assert_eq!(redact("abc"), "***");
        assert_eq!(redact("abcdef"), "abcd***ef");
    }

    #[test]
    fn detects_github_pat() {
        let txt = "token=gho_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789ab";
        let hits = scan_text(txt);
        assert!(
            hits.iter()
                .any(|h| h.pattern_name == "github_pat_fine_grained")
        );
    }

    #[test]
    fn skips_clean_text() {
        assert!(scan_text("hello world\nno secrets here").is_empty());
    }

    #[test]
    fn gho_token_is_detected() {
        let hits = scan_text("gho_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].pattern_name, "github_pat_fine_grained");
    }

    #[test]
    fn sk_ant_key_is_detected() {
        let hits = scan_text("export ANTHROPIC=sk-ant-api03_Abc123xyzDEF456ghiJKL");
        assert!(hits.iter().any(|h| h.pattern_name == "anthropic_api_key"));
    }

    #[test]
    fn redact_keeps_first_4_last_2() {
        let r = redact("gho_ABCDEFGHIJKLMNOPQRSTUVWXyz");
        assert!(r.starts_with("gho_"), "got {r}");
        assert!(r.ends_with("yz"), "got {r}");
        assert!(r.contains("***"));
    }

    #[test]
    fn binary_file_is_skipped() {
        let mut f = tempfile::NamedTempFile::new().expect("tmp");
        // NUL byte in head triggers the binary heuristic.
        f.write_all(b"\x00gho_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
            .expect("write");
        let hits = scan_file(f.path()).expect("scan");
        assert!(hits.is_empty(), "binary file should yield no matches");
    }

    #[test]
    fn no_false_positive_in_normal_text() {
        let hits = scan_text("This is just text, no keys.");
        assert!(hits.is_empty());
    }

    #[test]
    fn multibyte_text_does_not_panic() {
        // Mixing multi-byte chars with a real secret must not panic on the
        // column/redaction math.
        let txt = "한글 설정 token=gho_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA 끝";
        let hits = scan_text(txt);
        assert!(
            hits.iter()
                .any(|h| h.pattern_name == "github_pat_fine_grained")
        );
    }
}

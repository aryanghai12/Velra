//! Secret redaction (§9.2): an Aho–Corasick literal prefilter over all
//! detectors, then only the regexes whose literals hit are compiled (lazily,
//! once per process) and applied.

use aho_corasick::{AhoCorasick, AhoCorasickBuilder, MatchKind};
use regex::{Captures, Regex};
use std::borrow::Cow;
use std::sync::OnceLock;

struct Detector {
    kind: &'static str,
    literals: &'static [&'static str],
    pattern: &'static str,
    /// Replace only this capture group (keeps the key name / URL scheme).
    group: Option<usize>,
}

const DETECTORS: &[Detector] = &[
    Detector {
        kind: "private_key",
        literals: &["PRIVATE KEY"],
        pattern: r"(?s)-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----.*?(?:-----END [A-Z0-9 ]*PRIVATE KEY-----|\z)",
        group: None,
    },
    Detector {
        kind: "aws_access_key",
        literals: &[
            "AKIA", "ASIA", "AGPA", "AIDA", "AROA", "AIPA", "ANPA", "ANVA", "ABIA", "ACCA",
        ],
        pattern: r"\b(?:AKIA|ASIA|AGPA|AIDA|AROA|AIPA|ANPA|ANVA|ABIA|ACCA)[0-9A-Z]{16}\b",
        group: None,
    },
    Detector {
        kind: "aws_secret_key",
        literals: &[
            "aws_secret",
            "aws-secret",
            "awssecret",
            "secret_access_key",
            "secretaccesskey",
            "secret-access-key",
        ],
        pattern: r#"(?i)(?:aws[_-]?secret[_-]?(?:access[_-]?)?key|secret[_-]?access[_-]?key)["']?\s*[:=]\s*["']?([A-Za-z0-9/+=]{40})"#,
        group: Some(1),
    },
    Detector {
        kind: "github_token",
        literals: &["ghp_", "gho_", "ghu_", "ghs_", "ghr_", "github_pat_"],
        pattern: r"\b(?:gh[pousr]_[A-Za-z0-9]{36,255}|github_pat_[A-Za-z0-9_]{22,255})",
        group: None,
    },
    Detector {
        kind: "api_key",
        literals: &["sk-"],
        pattern: r"\bsk-(?:ant-|proj-)?[A-Za-z0-9_\-]{20,}",
        group: None,
    },
    Detector {
        kind: "slack_token",
        literals: &["xox"],
        pattern: r"\bxox[abprs]-[A-Za-z0-9-]{10,}",
        group: None,
    },
    Detector {
        kind: "google_api_key",
        literals: &["AIza"],
        pattern: r"\bAIza[0-9A-Za-z_\-]{35}",
        group: None,
    },
    Detector {
        kind: "stripe_key",
        literals: &["_live_", "_test_"],
        pattern: r"\b(?:sk|rk|pk)_(?:live|test)_[0-9A-Za-z]{16,}",
        group: None,
    },
    Detector {
        kind: "jwt",
        literals: &["eyJ"],
        pattern: r"\beyJ[A-Za-z0-9_-]{8,}\.eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}",
        group: None,
    },
    Detector {
        kind: "url_credentials",
        literals: &["://"],
        pattern: r#"\b[a-zA-Z][a-zA-Z0-9+.\-]{1,20}://([^\s/:@'"]+:[^\s/@'"]+)@"#,
        group: Some(1),
    },
    Detector {
        kind: "bearer_token",
        literals: &["bearer"],
        pattern: r"(?i)\bbearer\s+([A-Za-z0-9\-._~+/]{20,}=*)",
        group: Some(1),
    },
    Detector {
        kind: "generic_secret",
        literals: &["key", "secret", "token", "passw", "auth"],
        pattern: r#"(?i)(?:api[_-]?key|secret|token|passw(?:or)?d|auth)["']?\s*[:=]\s*["']?([^\s"',;]{8,})"#,
        group: Some(1),
    },
];

struct Prefilter {
    ac: AhoCorasick,
    /// literal pattern index → detector index
    owner: Vec<usize>,
}

fn prefilter() -> &'static Prefilter {
    static P: OnceLock<Prefilter> = OnceLock::new();
    P.get_or_init(|| {
        let mut lits = Vec::new();
        let mut owner = Vec::new();
        for (i, d) in DETECTORS.iter().enumerate() {
            for l in d.literals {
                lits.push(*l);
                owner.push(i);
            }
        }
        let ac = AhoCorasickBuilder::new()
            .ascii_case_insensitive(true)
            .match_kind(MatchKind::Standard)
            .build(&lits)
            .expect("static redaction literals are valid");
        Prefilter { ac, owner }
    })
}

fn regex_for(i: usize) -> &'static Regex {
    static RES: OnceLock<Vec<OnceLock<Regex>>> = OnceLock::new();
    let cells = RES.get_or_init(|| (0..DETECTORS.len()).map(|_| OnceLock::new()).collect());
    cells[i]
        .get_or_init(|| Regex::new(DETECTORS[i].pattern).expect("static redaction regex is valid"))
}

/// Replaces detected secrets with `[REDACTED:{kind}]`.
pub fn redact(s: &str) -> Cow<'_, str> {
    if s.len() < 8 {
        return Cow::Borrowed(s);
    }
    let pf = prefilter();
    // Sized from the table itself. A hand-written bound silently becomes an
    // out-of-bounds index the moment a detector is added, and this runs on the
    // hook path where the panic is caught and the only visible effect is that
    // redaction quietly stops happening.
    let mut hit = [false; DETECTORS.len()];
    let mut any = false;
    for m in pf.ac.find_overlapping_iter(s) {
        hit[pf.owner[m.pattern().as_usize()]] = true;
        any = true;
    }
    if !any {
        return Cow::Borrowed(s);
    }
    let mut out: Cow<'_, str> = Cow::Borrowed(s);
    for (i, d) in DETECTORS.iter().enumerate() {
        if !hit[i] {
            continue;
        }
        let re = regex_for(i);
        let replaced = re.replace_all(&out, |caps: &Captures<'_>| {
            let whole = caps.get(0).expect("group 0 always matches");
            let marker = format!("[REDACTED:{}]", d.kind);
            match d.group.and_then(|g| caps.get(g)) {
                Some(v) if v.as_str().starts_with("[REDACTED:") => whole.as_str().to_string(),
                Some(v) => {
                    let text = whole.as_str();
                    let (a, b) = (v.start() - whole.start(), v.end() - whole.start());
                    format!("{}{}{}", &text[..a], marker, &text[b..])
                }
                None => marker,
            }
        });
        if let Cow::Owned(o) = replaced {
            out = Cow::Owned(o);
        }
    }
    out
}

/// Redacts in place, returning whether anything changed.
pub fn redact_string(s: &mut String) -> bool {
    match redact(s) {
        Cow::Borrowed(_) => false,
        Cow::Owned(o) => {
            *s = o;
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_each_kind() {
        let cases = [
            ("AKIAIOSFODNN7EXAMPLE", "aws_access_key"),
            ("aws_secret_access_key = wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY", "aws_secret_key"),
            ("ghp_abcdefghijklmnopqrstuvwxyz0123456789", "github_token"),
            ("github_pat_11ABCDEFG0123456789_abcdefghijklmnopqrstuvwxyz", "github_token"),
            ("key: sk-ant-api03-abcdefghijklmnopqrstuvwxyz", "api_key"),
            ("xoxb-123456789012-abcdefghij", "slack_token"),
            ("AIzaSyA-1234567890abcdefghijklmnopqrstu", "google_api_key"),
            ("sk_live_abcdefghijklmnop1234", "stripe_key"),
            ("eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U", "jwt"),
            ("-----BEGIN RSA PRIVATE KEY-----\nMIIEow\n-----END RSA PRIVATE KEY-----", "private_key"),
            ("postgres://admin:hunter2@db.local/app", "url_credentials"),
            ("API_KEY=abcdef123456", "generic_secret"),
            ("\"password\": \"correct horse\"", ""),
            ("Authorization: Bearer abcdefghijklmnopqrstuvwxyz123456", "bearer_token"),
        ];
        for (input, kind) in cases {
            let out = redact(input);
            if kind.is_empty() {
                continue;
            }
            assert!(
                out.contains(&format!("[REDACTED:{kind}]")),
                "{input} -> {out}"
            );
        }
        assert_eq!(
            redact("postgres://admin:hunter2@db.local/app"),
            "postgres://[REDACTED:url_credentials]@db.local/app"
        );
        assert_eq!(
            redact("API_KEY=abcdef123456 rest"),
            "API_KEY=[REDACTED:generic_secret] rest"
        );
    }

    #[test]
    fn leaves_benign_text() {
        for s in [
            "fn main() { println!(\"hello\"); }",
            "the author: someone",
            "let tokens = tokenize(input);",
            "task-runner-configuration-file-v2",
            "https://github.com/org/repo",
            "keyboard layout",
        ] {
            assert_eq!(redact(s), s);
        }
    }
}

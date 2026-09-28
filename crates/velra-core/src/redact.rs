//! Secret redaction (§9.2): an Aho–Corasick literal prefilter over all
//! detectors, then only the regexes whose literals hit are compiled (lazily,
//! once per process) and applied.

use aho_corasick::{AhoCorasick, AhoCorasickBuilder, MatchKind};
use regex::{Captures, Regex};
use std::borrow::Cow;
use std::sync::OnceLock;

/// A token with a distinctive prefix (`ghp_`, `AKIA`, `sk-`, `eyJ`, …) is
/// matched after a word break *or* after an escape that ends in a word
/// character: `%3D` in URL-encoded text, `=` in JSON, a literal `\n` in
/// escaped output. A leading `\b` alone missed all three, and the whole
/// token was stored (reproduced, D140). The escape is kept; the token is
/// group 1. The word breaks are ASCII (`(?-u:\b)`): with Unicode's, a token
/// right after Japanese or Chinese text, which puts no space before it, or
/// right before an accented letter, was not a token.
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
        // `(?: BLOCK)?`: an armored PGP secret key, `-----BEGIN PGP PRIVATE
        // KEY BLOCK-----`, which the bare form never matched.
        pattern: r"(?s)-----BEGIN [A-Z0-9 ]*PRIVATE KEY(?: BLOCK)?-----.*?(?:-----END [A-Z0-9 ]*PRIVATE KEY(?: BLOCK)?-----|\z)",
        group: None,
    },
    Detector {
        // The end of a key whose `BEGIN` line is not in the text: output kept
        // from its tail, a prompt examined past its scan bound. Every whole
        // key is gone by now (the detector above runs first), so an `END`
        // line left is an orphan, and the base64 lines directly above it are
        // key material.
        kind: "private_key",
        literals: &["-----END"],
        pattern: r"(?m)(?:^[ \t]*[A-Za-z0-9+/=]+[ \t]*\r?\n)*^[ \t]*[A-Za-z0-9+/=]*[ \t]*\r?\n?-----END [A-Z0-9 ]*PRIVATE KEY(?: BLOCK)?-----",
        group: None,
    },
    Detector {
        kind: "aws_access_key",
        literals: &[
            "AKIA", "ASIA", "AGPA", "AIDA", "AROA", "AIPA", "ANPA", "ANVA", "ABIA", "ACCA",
        ],
        // Each prefixed token may follow a word break or an escape (see
        // `glued`): `%3DAKIA…` in a URL, `=AKIA…` in JSON.
        pattern: r"(?:(?-u:\b)|%[0-9A-Fa-f]{2}|\\[nrt]|\\u[0-9A-Fa-f]{4})((?:AKIA|ASIA|AGPA|AIDA|AROA|AIPA|ANPA|ANVA|ABIA|ACCA)[0-9A-Z]{16})(?:[^0-9A-Z]|$)",
        group: Some(1),
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
        pattern: r"(?:(?-u:\b)|%[0-9A-Fa-f]{2}|\\[nrt]|\\u[0-9A-Fa-f]{4})(gh[pousr]_[A-Za-z0-9]{36,255}|github_pat_[A-Za-z0-9_]{22,255})",
        group: Some(1),
    },
    Detector {
        kind: "api_key",
        literals: &["sk-"],
        pattern: r"(?:(?-u:\b)|%[0-9A-Fa-f]{2}|\\[nrt]|\\u[0-9A-Fa-f]{4})(sk-(?:ant-|proj-)?[A-Za-z0-9_\-]{20,})",
        group: Some(1),
    },
    Detector {
        kind: "slack_token",
        literals: &["xox"],
        pattern: r"(?:(?-u:\b)|%[0-9A-Fa-f]{2}|\\[nrt]|\\u[0-9A-Fa-f]{4})(xox[abprs]-[A-Za-z0-9-]{10,})",
        group: Some(1),
    },
    Detector {
        kind: "google_api_key",
        literals: &["AIza"],
        pattern: r"(?:(?-u:\b)|%[0-9A-Fa-f]{2}|\\[nrt]|\\u[0-9A-Fa-f]{4})(AIza[0-9A-Za-z_\-]{35})",
        group: Some(1),
    },
    Detector {
        kind: "stripe_key",
        literals: &["_live_", "_test_"],
        pattern: r"(?:(?-u:\b)|%[0-9A-Fa-f]{2}|\\[nrt]|\\u[0-9A-Fa-f]{4})((?:sk|rk|pk)_(?:live|test)_[0-9A-Za-z]{16,})",
        group: Some(1),
    },
    Detector {
        kind: "jwt",
        literals: &["eyJ"],
        pattern: r"(?:(?-u:\b)|%[0-9A-Fa-f]{2}|\\[nrt]|\\u[0-9A-Fa-f]{4})(eyJ[A-Za-z0-9_-]{8,}\.eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,})",
        group: Some(1),
    },
    Detector {
        kind: "url_credentials",
        literals: &["://"],
        // No leading `\b`: `next%3Dhttps://u:p@…` has a word character right
        // before the scheme. Only the credentials are replaced either way.
        pattern: r#"[a-zA-Z][a-zA-Z0-9+.\-]{1,20}://([^\s/:@'"]+:[^\s/@'"]+)@"#,
        group: Some(1),
    },
    Detector {
        kind: "bearer_token",
        literals: &["bearer"],
        pattern: r"(?i)\bbearer\s+([A-Za-z0-9\-._~+/]{20,}=*)",
        group: Some(1),
    },
    Detector {
        // `Authorization: Basic <base64 user:password>` (and
        // `Proxy-Authorization`). The generic rule needs `[:=]` straight
        // after its key word, which `Authorization:` never gives it.
        kind: "basic_auth",
        literals: &["authorization"],
        pattern: r#"(?i)authorization["']?\s*[:=]\s*["']?basic\s+([A-Za-z0-9+/]{8,}={0,2})"#,
        group: Some(1),
    },
    Detector {
        // A credential passed as a command-line flag's separate argument,
        // `--password hunter2` / `--token=…`, which the generic rule (`[:=]`
        // straight after the key word) misses when a space separates them.
        // Flags that only name where a credential is (`--token-file`,
        // `--password-stdin`) are not followed by a space or `=`.
        kind: "cli_credential",
        literals: &[
            "-password",
            "-passwd",
            "-token",
            "-secret",
            "-api-key",
            "-apikey",
        ],
        // The value must look like a credential rather than a word: it
        // starts with, or holds, an uppercase letter, a digit or a symbol a
        // credential uses, or it is a lowercase run of twelve or more. A
        // prompt that asks to "add a `--password option`." or "implement the
        // `--token flag`" is the user's task, and redacting its next word
        // would rewrite it.
        //
        // The flag may also stand in quotes, in an argument list or after an
        // escape -- PowerShell's `"--token" "X"`, JSON's `["--token", "X"]`
        // (an MCP server's `args`), `tool\n--token X` in escaped output --
        // which a leading `\s` and a `\s+|=` separator never matched
        // (Phase 11, D140). The value rule is what keeps prose untouched.
        pattern: r#"(?i)(?:^|[^A-Za-z0-9_]|\\[nrt]|%[0-9A-Fa-f]{2})--?(?:password|passwd|token|secret|api-?key|auth-token|access-token)["']?(?:\s*,\s*|\s+|=)["']?(?-i:([A-Z0-9$%@#*&^~+=/\\_][^\s"']*|[a-z][^\s"']*[A-Z0-9$%@#*&^~+=/\\_][^\s"']*|[a-z]{12,}))"#,
        group: Some(1),
    },
    Detector {
        // `curl -u user:password` / `--user user:password`.
        kind: "cli_credential",
        literals: &["curl"],
        pattern: r#"(?i)\bcurl\b[^\n|;&]*?\s(?:-u|--user)(?:\s+|=)["']?([^\s"':]+:[^\s"']+)"#,
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

    /// Phase 9: forms the table did not cover. Each was stored verbatim.
    #[test]
    fn detects_the_forms_that_used_to_pass_through() {
        let pgp = "-----BEGIN PGP PRIVATE KEY BLOCK-----\n\nlQOYBF4xyzSECRETMATERIAL\n=Ab12\n-----END PGP PRIVATE KEY BLOCK-----";
        assert_eq!(redact(pgp), "[REDACTED:private_key]");
        // The tail of a key whose BEGIN line was cut away.
        let orphan = "tail of output\nQWxhZGRpbjpvcGVuIHNlc2FtZQ0123\nMIIEowIBAAKCAQEAsecretkeybody\n-----END RSA PRIVATE KEY-----\nexit 0";
        let out = redact(orphan);
        assert_eq!(
            out, "tail of output\n[REDACTED:private_key]\nexit 0",
            "{out}"
        );
        assert_eq!(
            redact("  Zm9vYmFy\n  -----END OPENSSH PRIVATE KEY-----"),
            "[REDACTED:private_key]"
        );
        assert_eq!(
            redact("curl -H 'Authorization: Basic dXNlcjpodW50ZXIyaHVudGVyMg==' https://x"),
            "curl -H 'Authorization: Basic [REDACTED:basic_auth]' https://x"
        );
        assert_eq!(
            redact("Proxy-Authorization: basic YWRtaW46czNjcjN0"),
            "Proxy-Authorization: basic [REDACTED:basic_auth]"
        );
        for (input, expected) in [
            (
                "mysqladmin --password hunter2 status",
                "mysqladmin --password [REDACTED:cli_credential] status",
            ),
            (
                "gh api --token 'abc123def' /user",
                "gh api --token '[REDACTED:cli_credential]' /user",
            ),
            (
                "tool -secret s3cr3t",
                "tool -secret [REDACTED:cli_credential]",
            ),
            (
                "deploy --api-key=k-12345",
                "deploy --api-key=[REDACTED:cli_credential]",
            ),
            (
                "curl -u admin:hunter2 https://h/api",
                "curl -u [REDACTED:cli_credential] https://h/api",
            ),
            (
                "curl -s https://h/api --user=ci:t0ps3cret -o out",
                "curl -s https://h/api --user=[REDACTED:cli_credential] -o out",
            ),
        ] {
            assert_eq!(redact(input), expected, "{input}");
        }
    }

    /// The new detectors' neighbours that are not credentials.
    #[test]
    fn the_new_detectors_leave_their_neighbours_alone() {
        for s in [
            "docker login --password-stdin < pw.txt",
            "gh auth login --with-token < token.txt",
            "cargo publish --token-file ~/.cargo/tok",
            "tool --password --verbose",
            // A task that names a flag is the user's words, not a secret.
            "Add a --password option to the CLI.",
            "implement the --token flag in cli.py",
            "the --secret sauce, (see --api-key option.)",
            "document --token (optional) and --password flag-based auth",
            "git push -u origin main",
            "docker run -u 1000:1000 image",
            "curl -u",
            "Basic setup is described in the README",
            "Authorization header is required",
            "-----END CERTIFICATE-----",
            "the key ends with -----END RSA PRIVATE KEY----- mid-sentence",
        ] {
            assert_eq!(redact(s), s, "{s}");
        }
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

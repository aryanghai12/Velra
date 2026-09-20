//! Small text helpers shared by normalization, redaction and rendering.
//!
//! Every function here is deterministic and cuts only on UTF-8 char boundaries.

use std::borrow::Cow;

/// Truncates `s` to at most `max` chars. When cut, the result ends in `...`
/// (counted within `max`).
pub fn truncate_chars(s: &str, max: usize) -> Cow<'_, str> {
    if s.len() <= max {
        return Cow::Borrowed(s);
    }
    let count = s.chars().count();
    if count <= max {
        return Cow::Borrowed(s);
    }
    if max <= 3 {
        return Cow::Owned(s.chars().take(max).collect());
    }
    let mut out: String = s.chars().take(max - 3).collect();
    out.push_str("...");
    Cow::Owned(out)
}

/// Keeps the last `max` chars of `s`, prefixing `...` when cut.
pub fn truncate_chars_front(s: &str, max: usize) -> Cow<'_, str> {
    if s.len() <= max {
        return Cow::Borrowed(s);
    }
    let count = s.chars().count();
    if count <= max {
        return Cow::Borrowed(s);
    }
    if max <= 3 {
        return Cow::Owned(s.chars().skip(count - max).collect());
    }
    let mut out = String::from("...");
    out.extend(s.chars().skip(count - (max - 3)));
    Cow::Owned(out)
}

/// Longest prefix of `s` that is at most `max_bytes` long.
pub fn prefix_bytes(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Longest suffix of `s` that is at most `max_bytes` long.
pub fn suffix_bytes(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut start = s.len() - max_bytes;
    while start < s.len() && !s.is_char_boundary(start) {
        start += 1;
    }
    &s[start..]
}

/// Trims and collapses every whitespace run to a single space.
pub fn collapse_whitespace(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for word in s.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    out
}

/// Number of lines in `s` ("" → 0, "a" → 1, "a\n" → 1, "a\nb" → 2).
pub fn line_count(s: &str) -> u32 {
    if s.is_empty() {
        return 0;
    }
    let newlines = s.bytes().filter(|&b| b == b'\n').count();
    let n = if s.ends_with('\n') {
        newlines
    } else {
        newlines + 1
    };
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// Removes ANSI escape sequences (CSI, OSC and two-byte escapes).
pub fn strip_ansi(s: &str) -> Cow<'_, str> {
    let b = s.as_bytes();
    if !b.contains(&0x1b) {
        return Cow::Borrowed(s);
    }
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] != 0x1b {
            out.push(b[i]);
            i += 1;
            continue;
        }
        i += 1;
        let Some(&kind) = b.get(i) else { break };
        match kind {
            b'[' => {
                i += 1;
                while i < b.len() && !(0x40..=0x7e).contains(&b[i]) {
                    i += 1;
                }
                i += 1;
            }
            b']' => {
                i += 1;
                while i < b.len() {
                    if b[i] == 0x07 {
                        i += 1;
                        break;
                    }
                    if b[i] == 0x1b && b.get(i + 1) == Some(&b'\\') {
                        i += 2;
                        break;
                    }
                    i += 1;
                }
            }
            k if k.is_ascii() => i += 1,
            _ => {}
        }
    }
    match String::from_utf8(out) {
        Ok(s) => Cow::Owned(s),
        Err(e) => Cow::Owned(String::from_utf8_lossy(e.as_bytes()).into_owned()),
    }
}

/// Strips ANSI sequences and applies carriage-return overwrite semantics per
/// line, so progress bars collapse to their final state.
pub fn clean_terminal_output(s: &str) -> String {
    let stripped = strip_ansi(s);
    if !stripped.contains('\r') {
        return stripped.into_owned();
    }
    let mut out = String::with_capacity(stripped.len());
    for (i, line) in stripped.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let line = line.strip_suffix('\r').unwrap_or(line);
        match line.rfind('\r') {
            Some(p) => out.push_str(&line[p + 1..]),
            None => out.push_str(line),
        }
    }
    out
}

/// Estimated token count used for capsule budgets (§16.3).
///
/// A single characters-per-token ratio cannot serve this text. English prose
/// runs near four characters per token, but the capsule is also deliberately
/// dense — bracketed section tags, POSIX and Windows paths, a 26-character ULID
/// and quoted code fragments — and those measure 2.07–2.16. The original
/// `ceil(chars / 3.2)` therefore under-read real capsules by about a third, and
/// the truncation ladder stopped while the block was still over budget.
///
/// This walks the string and charges per run the way a byte-pair tokenizer
/// does: a word that follows a space absorbs about three letters per token; a
/// word glued to punctuation (a path segment after `/` or `_`) fragments at
/// about two; digits group in threes; each remaining ASCII byte is its own
/// token; non-ASCII costs two per character. A margin of 1/32 is added last so
/// the estimate errs high, which is the side a budget must fail on.
///
/// # Calibration
///
/// `LEAD_DIV` was 4 until v0.1.2, on the strength of the two v0.1-format
/// capsules in `tests/fixtures/tokenizer/`, where the walk over-reads by 3–6%.
/// That calibration did not survive the format change. The v0.1.1 capsule opens
/// with a ~340-character prose paragraph, and against the eight capsules Claude
/// Code actually received during the v0.1.1 efficacy benchmark the walk came in
/// *below* the real cost every single time — by up to 7.9% of its own reading,
/// which is why one delivered capsule cost 804 tokens against an 800 ceiling.
///
/// Four letters per token is simply not what Claude's tokenizer charges for
/// English; three is much closer. At `LEAD_DIV = 3` the walk reads at or above
/// the real count on all ten measured capsules, old format and new, and
/// over-reads by at most 13.6%. That over-read is content the capsule declines
/// to carry, and it is the right side to be wrong on.
///
/// `crates/velra/tests/capsule.rs::estimator_is_above_real_tokenizer_counts`
/// holds this against every measured fixture. Re-derive it whenever the capsule
/// format changes; `bench/harness/measure_tokens.py` is what measures the gap.
///
/// Deterministic and allocation-free: the same bytes always yield the same
/// number on every platform, which the capsule goldens depend on.
pub fn estimate_tokens(s: &str) -> u32 {
    /// Letters per token for a word starting at a whitespace boundary.
    /// See the calibration note above: 4 under-read the current format.
    const LEAD_DIV: u64 = 3;
    /// Letters per token for a word glued to the previous character.
    const GLUED_DIV: u64 = 2;
    const DIGIT_DIV: u64 = 3;

    let b = s.as_bytes();
    let mut tokens: u64 = 0;
    let mut i = 0usize;
    // True at the start of the string and after any whitespace, where a word's
    // leading space merges into its first token.
    let mut boundary = true;
    while i < b.len() {
        let c = b[i];
        if c == b' ' {
            let start = i;
            while i < b.len() && b[i] == b' ' {
                i += 1;
            }
            // One space merges into the following word; indentation beyond
            // that costs roughly one token per two spaces.
            let run = (i - start) as u64;
            if run > 1 {
                tokens += run.div_ceil(2);
            }
            boundary = true;
        } else if matches!(c, b'\n' | b'\r' | b'\t') {
            tokens += 1;
            i += 1;
            boundary = true;
        } else if c.is_ascii_alphabetic() {
            let start = i;
            while i < b.len() && b[i].is_ascii_alphabetic() {
                i += 1;
            }
            let div = if boundary { LEAD_DIV } else { GLUED_DIV };
            tokens += ((i - start) as u64).div_ceil(div).max(1);
            boundary = false;
        } else if c.is_ascii_digit() {
            let start = i;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
            tokens += ((i - start) as u64).div_ceil(DIGIT_DIV).max(1);
            boundary = false;
        } else if c >= 0x80 {
            // Skip the whole UTF-8 sequence; accented text, CJK and emoji all
            // cost at least two tokens per character.
            tokens += 2;
            i += 1;
            while i < b.len() && (b[i] & 0xc0) == 0x80 {
                i += 1;
            }
            boundary = false;
        } else {
            // Punctuation and symbols tokenize close to one for one.
            tokens += 1;
            i += 1;
            boundary = false;
        }
    }
    tokens += tokens.div_ceil(32);
    u32::try_from(tokens).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncation_respects_char_boundaries() {
        assert_eq!(truncate_chars("héllo wörld", 8), "héllo...");
        assert_eq!(truncate_chars("short", 10), "short");
        assert_eq!(truncate_chars_front("abcdefghij", 6), "...hij");
        assert_eq!(prefix_bytes("héllo", 2), "h");
        assert_eq!(suffix_bytes("héllo", 4), "llo");
    }

    #[test]
    fn line_counts() {
        assert_eq!(line_count(""), 0);
        assert_eq!(line_count("a"), 1);
        assert_eq!(line_count("a\n"), 1);
        assert_eq!(line_count("a\nb"), 2);
    }

    #[test]
    fn ansi_and_cr() {
        assert_eq!(strip_ansi("\x1b[31mred\x1b[0m ok"), "red ok");
        assert_eq!(strip_ansi("\x1b]0;title\x07x"), "x");
        assert_eq!(
            clean_terminal_output("10%\r50%\r100%\ndone\r\n"),
            "100%\ndone\n"
        );
    }

    #[test]
    fn token_estimate() {
        assert_eq!(estimate_tokens(""), 0);
        // Monotonic in length, and never zero for non-empty input.
        assert!(estimate_tokens("abc") >= 1);
        let mut last = 0;
        for n in [1usize, 8, 64, 512] {
            let t = estimate_tokens(&"x".repeat(n));
            assert!(t > last, "must grow with length at {n}");
            last = t;
        }
    }

    #[test]
    fn token_estimate_is_calibrated_to_the_capsule_format() {
        // Two capsules measured with Anthropic's tokenizer during the v0.1
        // benchmark, reduced to the shapes that made them dense: section tags,
        // paths, a ULID and quoted code. The estimator must land above the
        // measured count for this kind of text, never below it.
        let dense = "[FILE_ACTIVITY] (OBSERVED)\n\
             - src/ledger/validation/invoice_number_gb.py | edited 0x, read 1x\n\
             - src/ledger/importers/ledger_ofx_v3.py | edited 0x, read 1x\n\
             [FAILURE_LOCATION] (INFERRED | failure-location)\n\
             tests/test_engine.py:54\n";
        let per_token = dense.len() as f64 / f64::from(estimate_tokens(dense));
        assert!(
            (1.6..=2.2).contains(&per_token),
            "dense capsule text should estimate near 2.1 chars/token, got {per_token:.2}"
        );

        // Prose is charged more cheaply, but still conservatively: real English
        // runs near 4.0 characters per token.
        let prose = "Velra is a local tool that recorded this task state from \
             Claude Code tool events before the conversation was compacted.";
        let prose_per_token = prose.len() as f64 / f64::from(estimate_tokens(prose));
        assert!(
            (2.0..=3.2).contains(&prose_per_token),
            "prose should estimate near 2.6 chars/token, got {prose_per_token:.2}"
        );
        assert!(
            prose_per_token > per_token,
            "prose must be cheaper than paths"
        );
    }

    #[test]
    fn token_estimate_never_under_reads_dense_punctuation() {
        // A run of separators is close to one token each; the old ratio-based
        // estimate charged a third of that.
        let seps = "/_-.:|()[]{}<>";
        assert!(estimate_tokens(seps) >= seps.len() as u32);
    }
}

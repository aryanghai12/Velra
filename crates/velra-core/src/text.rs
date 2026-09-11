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

/// Estimated token count used for capsule budgets: `ceil(chars / 3.2)`.
pub fn estimate_tokens(s: &str) -> u32 {
    let chars = s.chars().count() as u64;
    u32::try_from((chars * 10).div_ceil(32)).unwrap_or(u32::MAX)
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
        assert_eq!(estimate_tokens("abc"), 1);
        assert_eq!(estimate_tokens(&"x".repeat(32)), 10);
        assert_eq!(estimate_tokens(&"x".repeat(33)), 11);
    }
}

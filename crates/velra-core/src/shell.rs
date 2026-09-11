//! Minimal shell command-line analysis: subcommand splitting, word
//! tokenization, and git restore-family detection (§13.3).
//!
//! This is intentionally not a full shell parser. It handles the quoting and
//! separators that occur in agent-issued commands for both Bash and PowerShell.

/// Splits a command line into subcommands on `&&`, `||`, `;`, `|` and
/// newlines, honoring single and double quotes. Empty parts are dropped.
pub fn split_subcommands(cmd: &str) -> Vec<&str> {
    fn push<'a>(cmd: &'a str, from: usize, to: usize, parts: &mut Vec<&'a str>) {
        let piece = cmd[from..to].trim();
        if !piece.is_empty() {
            parts.push(piece);
        }
    }
    let b = cmd.as_bytes();
    let mut parts = Vec::new();
    let mut start = 0usize;
    let mut i = 0usize;
    let mut single = false;
    let mut double = false;
    while i < b.len() {
        let c = b[i];
        if single {
            if c == b'\'' {
                single = false;
            }
            i += 1;
            continue;
        }
        if double {
            if c == b'\\' {
                i = (i + 2).min(b.len());
                continue;
            }
            if c == b'"' {
                double = false;
            }
            i += 1;
            continue;
        }
        match c {
            b'\\' => {
                // A backslash escapes the next byte in POSIX shells; it is a
                // plain path separator in PowerShell. Only skip when it
                // escapes a separator or quote.
                if matches!(b.get(i + 1), Some(b'&' | b'|' | b';' | b'\'' | b'"')) {
                    i += 2;
                } else {
                    i += 1;
                }
                continue;
            }
            b'\'' => single = true,
            b'"' => double = true,
            b'&' if b.get(i + 1) == Some(&b'&') => {
                push(cmd, start, i, &mut parts);
                i += 2;
                start = i;
                continue;
            }
            b'|' => {
                push(cmd, start, i, &mut parts);
                i += if b.get(i + 1) == Some(&b'|') { 2 } else { 1 };
                start = i;
                continue;
            }
            b';' | b'\n' | b'\r' => {
                push(cmd, start, i, &mut parts);
                i += 1;
                start = i;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    push(cmd, start, b.len(), &mut parts);
    parts
}

/// Splits one subcommand into words, removing quotes.
pub fn tokenize(s: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_word = true;
                for q in chars.by_ref() {
                    if q == '\'' {
                        break;
                    }
                    cur.push(q);
                }
            }
            '"' => {
                in_word = true;
                while let Some(q) = chars.next() {
                    match q {
                        '"' => break,
                        '\\' if matches!(chars.peek(), Some('"' | '\\' | '$' | '`')) => {
                            cur.push(chars.next().unwrap_or('\\'));
                        }
                        _ => cur.push(q),
                    }
                }
            }
            c if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut cur));
                    in_word = false;
                }
            }
            _ => {
                in_word = true;
                cur.push(c);
            }
        }
    }
    if in_word {
        words.push(cur);
    }
    words
}

/// `NAME=value` shell assignment.
pub fn is_env_assignment(word: &str) -> bool {
    let Some(eq) = word.find('=') else {
        return false;
    };
    let name = &word[..eq];
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Lower-cased executable basename with `.exe/.cmd/.bat/.ps1` stripped.
pub fn exe_basename(word: &str) -> String {
    let base = word.rsplit(['/', '\\']).next().unwrap_or(word);
    let lower = base.to_ascii_lowercase();
    for ext in [".exe", ".cmd", ".bat", ".ps1"] {
        if let Some(stem) = lower.strip_suffix(ext) {
            return stem.to_string();
        }
    }
    lower
}

/// Words of a subcommand with leading env assignments and transparent
/// wrappers (`env`, `time`, `npx`, `bundle exec`, PowerShell `&`, …) removed.
pub fn command_words(sub: &str) -> Vec<String> {
    let mut words = tokenize(sub);
    while !words.is_empty() {
        let first = words[0].clone();
        let first = first.as_str();
        let second = words.get(1).cloned().unwrap_or_default();
        let second = second.as_str();
        if is_env_assignment(first)
            || matches!(
                first,
                "&" | "time" | "command" | "exec" | "nohup" | "sudo" | "env" | "npx" | "bunx"
            )
        {
            words.remove(0);
            while words
                .first()
                .is_some_and(|w| w.starts_with('-') && w.len() > 1)
            {
                words.remove(0);
            }
            continue;
        }
        let two_word_wrapper = matches!(
            (first, second),
            ("pnpm", "exec" | "dlx")
                | ("yarn", "exec" | "dlx")
                | ("uv" | "poetry" | "pipenv" | "pdm" | "hatch" | "rye", "run")
                | ("bundle", "exec")
        );
        if two_word_wrapper {
            words.drain(0..2);
            continue;
        }
        break;
    }
    words
}

/// A parsed `git` invocation: the subcommand and its arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitInvocation {
    pub sub: String,
    pub args: Vec<String>,
}

/// Parses `git [global options] <sub> <args…>`; `None` if not a git command.
pub fn parse_git(words: &[String]) -> Option<GitInvocation> {
    if exe_basename(words.first()?) != "git" {
        return None;
    }
    let mut i = 1;
    while i < words.len() {
        let w = words[i].as_str();
        if matches!(w, "-C" | "-c" | "--git-dir" | "--work-tree" | "--namespace") {
            i += 2;
            continue;
        }
        if w.starts_with('-') {
            i += 1;
            continue;
        }
        break;
    }
    let sub = words.get(i)?.clone();
    Some(GitInvocation {
        sub,
        args: words[i + 1..].to_vec(),
    })
}

/// Git restore-family subcommand per §13.3. `is_file` resolves a checkout
/// argument to an existing file (the "tracked file path" check).
pub fn is_restore_family(g: &GitInvocation, is_file: &dyn Fn(&str) -> bool) -> bool {
    match g.sub.as_str() {
        "restore" | "clean" => true,
        "reset" => g.args.iter().any(|a| a == "--hard"),
        "stash" => match g.args.first() {
            None => true,
            Some(a) if a.starts_with('-') => true,
            Some(a) => a == "push" || a == "save",
        },
        "checkout" => {
            g.args.iter().any(|a| a == "--" || a == ".")
                || g.args.iter().any(|a| !a.starts_with('-') && is_file(a))
        }
        _ => false,
    }
}

/// What a shell command means for git-aware file tracking.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GitEffects {
    /// First restore-family subcommand text, if any.
    pub restore: Option<String>,
    /// Whether any subcommand is `git commit`.
    pub commit: bool,
}

impl GitEffects {
    pub fn any(&self) -> bool {
        self.restore.is_some() || self.commit
    }
}

/// Scans every subcommand of `cmd` for git restore-family and commit calls.
pub fn git_effects(cmd: &str, is_file: &dyn Fn(&str) -> bool) -> GitEffects {
    let mut fx = GitEffects::default();
    // Fast reject: no "git" substring means no git subcommand.
    if !cmd.contains("git") {
        return fx;
    }
    for sub in split_subcommands(cmd) {
        let words = command_words(sub);
        let Some(g) = parse_git(&words) else { continue };
        if fx.restore.is_none() && is_restore_family(&g, is_file) {
            fx.restore = Some(sub.to_string());
        }
        if g.sub == "commit" {
            fx.commit = true;
        }
    }
    fx
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_files(_: &str) -> bool {
        false
    }

    #[test]
    fn splits_on_separators_but_not_inside_quotes() {
        assert_eq!(
            split_subcommands("cd a && npm test || echo 'x;y' ; git status | cat\nls"),
            vec!["cd a", "npm test", "echo 'x;y'", "git status", "cat", "ls"]
        );
        assert_eq!(
            split_subcommands("echo \"a && b\""),
            vec!["echo \"a && b\""]
        );
    }

    #[test]
    fn tokenizes_quotes() {
        assert_eq!(
            tokenize(r#"git commit -m "fix: a b" 'c d'"#),
            vec!["git", "commit", "-m", "fix: a b", "c d"]
        );
    }

    #[test]
    fn strips_wrappers() {
        assert_eq!(
            command_words("FOO=1 BAR=2 npx --yes jest -t x"),
            vec!["jest", "-t", "x"]
        );
        assert_eq!(command_words("bundle exec rspec"), vec!["rspec"]);
        assert_eq!(
            command_words("& git restore a.txt"),
            vec!["git", "restore", "a.txt"]
        );
    }

    #[test]
    fn restore_family() {
        let is = |c: &str| git_effects(c, &no_files).restore.is_some();
        assert!(is("git restore src/a.rs"));
        assert!(is("git checkout -- src/a.rs"));
        assert!(is("git checkout ."));
        assert!(is("git reset --hard HEAD~1"));
        assert!(is("git stash"));
        assert!(is("git stash -u"));
        assert!(is("git stash push -m wip"));
        assert!(is("git clean -fd"));
        assert!(is("npm test && git -C sub restore x"));
        assert!(!is("git stash pop"));
        assert!(!is("git reset --soft HEAD~1"));
        assert!(!is("git checkout main"));
        assert!(!is("git status"));
        assert!(git_effects("git checkout a.txt", &|p: &str| p == "a.txt")
            .restore
            .is_some());
        assert!(git_effects("git add . && git commit -m x", &no_files).commit);
    }
}

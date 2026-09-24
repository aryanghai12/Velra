//! Minimal shell command-line analysis: subcommand splitting, word
//! tokenization, and git restore-family detection (§13.3).
//!
//! This is intentionally not a full shell parser. It handles the quoting and
//! separators that occur in agent-issued commands for both Bash and PowerShell.

/// Splits a command line into subcommands on `&&`, `||`, `;`, `|` and
/// newlines, honoring single and double quotes. Empty parts are dropped.
pub fn split_subcommands(cmd: &str) -> Vec<&str> {
    subcommand_ranges(cmd)
        .into_iter()
        .map(|(a, b)| &cmd[a..b])
        .collect()
}

/// Byte ranges of [`split_subcommands`]'s parts, for callers that need to
/// slice the original line rather than rebuild it.
pub fn subcommand_ranges(cmd: &str) -> Vec<(usize, usize)> {
    fn push(cmd: &str, from: usize, to: usize, parts: &mut Vec<(usize, usize)>) {
        let piece = &cmd[from..to];
        let lead = piece.len() - piece.trim_start().len();
        let trimmed = piece.trim();
        if !trimmed.is_empty() {
            parts.push((from + lead, from + lead + trimmed.len()));
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
    strip_wrappers(tokenize(sub))
}

fn strip_wrappers(mut words: Vec<String>) -> Vec<String> {
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

/// Whether a subcommand only prepares the environment and says nothing about
/// what the command line did.
fn is_setup_only(sub: &str) -> bool {
    let words = command_words(sub);
    // `command_words` strips leading env assignments, so a subcommand that was
    // nothing but assignments comes back empty.
    let Some(first) = words.first() else {
        return true;
    };
    matches!(
        exe_basename(first).as_str(),
        "cd" | "chdir"
            | "pushd"
            | "popd"
            | "set-location"
            | "sl"
            | "export"
            | "set"
            | "setx"
            | "unset"
            | "source"
            | "."
            | "clear"
            | "cls"
    )
}

/// A command line with its leading setup subcommands removed, as a slice of
/// the original.
///
/// Agents habitually prefix a command with `cd "<absolute path>" &&`, and on
/// Windows that is sixty or more characters of path before the first byte that
/// says anything. The capsule quotes commands under a character cap and cuts
/// from the right, so the raw line spends its whole allowance on the prefix:
/// one v0.1 benchmark capsule reported its active failure as
/// ``Command: cd "C:\Users\…\s2-velra-r2" && git restore...``, which names
/// neither the test that failed nor the runner that ran it.
///
/// Only a *leading* run is dropped, and the remainder is returned verbatim, so
/// pipelines and separators keep their meaning. A line that is nothing but
/// setup is returned unchanged — there is nothing better to show.
pub fn display_command(cmd: &str) -> &str {
    let trimmed = cmd.trim();
    for (start, end) in subcommand_ranges(trimmed) {
        if is_setup_only(&trimmed[start..end]) {
            continue;
        }
        return trimmed[start..].trim_end();
    }
    trimmed
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
    git_effects_in(cmd, Dialect::Posix, is_file)
}

/// [`git_effects`] for a command line written for `dialect`.
///
/// `is_file` is asked about a `git checkout` argument joined to the directory
/// the subcommand runs in, as far as the line itself says (a literal `cd` or
/// `git -C` before it); relative paths stay relative to the command's cwd.
pub fn git_effects_in(cmd: &str, dialect: Dialect, is_file: &dyn Fn(&str) -> bool) -> GitEffects {
    let mut fx = GitEffects::default();
    // Fast reject: no "git" substring means no git subcommand.
    if !cmd.contains("git") {
        return fx;
    }
    for call in git_calls(cmd, dialect) {
        let base = call.base.clone();
        let in_base = |p: &str| match &base {
            Some(b) if !b.is_empty() && !is_absolute_word(p) => is_file(&format!("{b}/{p}")),
            _ => is_file(p),
        };
        if fx.restore.is_none() && is_restore_family(&call.git, &in_base) {
            fx.restore = Some(call.text.clone());
        }
        if call.git.sub == "commit" {
            fx.commit = true;
        }
    }
    fx
}

// ------------------------------------------------------------- dialects

/// The quoting rules a command line is written in.
///
/// Only what separates subcommands and words differs: POSIX shells escape
/// with `\`, PowerShell with a backtick (a `\` is a path separator, so
/// `"C:\proj\"` is a complete string there), and `cmd.exe` with `^`, where a
/// single `&` also separates commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    Posix,
    PowerShell,
    Cmd,
}

impl Dialect {
    /// The dialect of a shell tool's `command` (`Bash`, `PowerShell`).
    pub fn for_tool(tool: &str) -> Dialect {
        if tool == "PowerShell" {
            Dialect::PowerShell
        } else {
            Dialect::Posix
        }
    }
}

/// [`subcommand_ranges`] under `dialect`'s quoting.
pub fn subcommand_ranges_in(cmd: &str, dialect: Dialect) -> Vec<(usize, usize)> {
    if dialect == Dialect::Posix {
        return subcommand_ranges(cmd);
    }
    let escape = if dialect == Dialect::PowerShell {
        b'`'
    } else {
        b'^'
    };
    let b = cmd.as_bytes();
    let mut parts = Vec::new();
    let push = |from: usize, to: usize, parts: &mut Vec<(usize, usize)>| {
        let piece = &cmd[from..to];
        let lead = piece.len() - piece.trim_start().len();
        let trimmed = piece.trim();
        if !trimmed.is_empty() {
            parts.push((from + lead, from + lead + trimmed.len()));
        }
    };
    let (mut start, mut i) = (0usize, 0usize);
    let (mut single, mut double) = (false, false);
    while i < b.len() {
        let c = b[i];
        if single {
            single = c != b'\'';
            i += 1;
            continue;
        }
        if double {
            if c == escape && dialect == Dialect::PowerShell {
                i = (i + 2).min(b.len());
                continue;
            }
            double = c != b'"';
            i += 1;
            continue;
        }
        match c {
            _ if c == escape => {
                i = (i + 2).min(b.len());
                continue;
            }
            b'\'' if dialect == Dialect::PowerShell => single = true,
            b'"' => double = true,
            b'&' if b.get(i + 1) == Some(&b'&') => {
                push(start, i, &mut parts);
                i += 2;
                start = i;
                continue;
            }
            b'&' if dialect == Dialect::Cmd => {
                push(start, i, &mut parts);
                i += 1;
                start = i;
                continue;
            }
            b'|' => {
                push(start, i, &mut parts);
                i += if b.get(i + 1) == Some(&b'|') { 2 } else { 1 };
                start = i;
                continue;
            }
            b';' if dialect == Dialect::PowerShell => {
                push(start, i, &mut parts);
                i += 1;
                start = i;
                continue;
            }
            b'\n' | b'\r' => {
                push(start, i, &mut parts);
                i += 1;
                start = i;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    push(start, b.len(), &mut parts);
    parts
}

/// [`tokenize`] under `dialect`'s quoting.
pub fn tokenize_in(s: &str, dialect: Dialect) -> Vec<String> {
    if dialect == Dialect::Posix {
        return tokenize(s);
    }
    let escape = if dialect == Dialect::PowerShell {
        '`'
    } else {
        '^'
    };
    let mut words = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            '\'' if dialect == Dialect::PowerShell => {
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
                        _ if q == escape && dialect == Dialect::PowerShell => {
                            if let Some(n) = chars.next() {
                                cur.push(n);
                            }
                        }
                        _ => cur.push(q),
                    }
                }
            }
            _ if c == escape => {
                in_word = true;
                if let Some(n) = chars.next() {
                    cur.push(n);
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

/// [`command_words`] under `dialect`'s quoting.
pub fn command_words_in(sub: &str, dialect: Dialect) -> Vec<String> {
    if dialect == Dialect::Posix {
        return command_words(sub);
    }
    strip_wrappers(tokenize_in(sub, dialect))
}

// ------------------------------------------------------- git calls in a line

/// One git invocation found in a command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitCall {
    pub git: GitInvocation,
    /// The subcommand's text, as written.
    pub text: String,
    /// The directory it runs in, relative to the command's starting cwd
    /// (`""`) or absolute, as far as the line says: a literal `cd` before it
    /// and its own `-C`. `None` when the line moves somewhere it does not
    /// name literally (`cd $DIR`, `cd -`, `popd`) or git is pointed elsewhere
    /// (`--git-dir`, `--work-tree`).
    pub base: Option<String>,
    /// Quoting of the line it was found in.
    pub dialect: Dialect,
}

/// How deep `bash -c "…"` / `cmd /c …` / `powershell -Command …` are opened.
const MAX_NESTING: usize = 2;

/// Every git invocation in `cmd`, in order, including those inside a shell
/// the line starts (`bash -c`, `sh -c`, `cmd /c`, `powershell -Command`),
/// with the directory each runs in.
///
/// This follows `cd` only where the line spells the target out; it does not
/// expand variables, evaluate substitutions or track `pushd` stacks. Anything
/// it cannot follow makes the base unknown, which the callers treat as "the
/// line does not say".
pub fn git_calls(cmd: &str, dialect: Dialect) -> Vec<GitCall> {
    let mut out = Vec::new();
    collect_git_calls(cmd, dialect, Some(String::new()), 0, &mut out);
    out
}

fn collect_git_calls(
    cmd: &str,
    dialect: Dialect,
    mut base: Option<String>,
    depth: usize,
    out: &mut Vec<GitCall>,
) -> Option<String> {
    for (a, b) in subcommand_ranges_in(cmd, dialect) {
        let sub = &cmd[a..b];
        let words = command_words_in(sub, dialect);
        let Some(first) = words.first() else { continue };
        let exe = exe_basename(first);
        if let Some(target) = cd_target(&exe, &words) {
            base = match (base, target) {
                (Some(b), Some(t)) => Some(join_word(&b, &t)),
                _ => None,
            };
            continue;
        }
        if depth < MAX_NESTING {
            if let Some((script, inner)) = nested_script(&exe, &words) {
                // A child shell's `cd` does not outlive it.
                collect_git_calls(&script, inner, base.clone(), depth + 1, out);
                continue;
            }
        }
        let Some((git, dirs, elsewhere)) = parse_git_dirs(&words) else {
            continue;
        };
        let mut call_base = if elsewhere { None } else { base.clone() };
        for d in &dirs {
            call_base = match call_base {
                Some(b) if literal(d) => Some(join_word(&b, d)),
                _ => None,
            };
        }
        out.push(GitCall {
            git,
            text: sub.to_string(),
            base: call_base,
            dialect,
        });
    }
    base
}

/// The target of a directory change, if `words` is one: `Some(Some(dir))`
/// for a literal target, `Some(None)` for one the line does not spell out.
fn cd_target(exe: &str, words: &[String]) -> Option<Option<String>> {
    match exe {
        "cd" | "chdir" | "pushd" | "set-location" | "sl" | "push-location" => {}
        "popd" | "pop-location" => return Some(None),
        _ => return None,
    }
    // Flags (`-L`, `--`, PowerShell's `-Path`, cmd's `/d`) are not the target;
    // a lone `-` is, and means "the previous directory".
    let args: Vec<&String> = words[1..]
        .iter()
        .filter(|w| !(w.eq_ignore_ascii_case("/d") || (w.starts_with('-') && w.len() > 1)))
        .collect();
    Some(match args.as_slice() {
        [dir] if dir.as_str() != "-" && literal(dir) => Some(dir.to_string()),
        _ => None,
    })
}

/// A word with nothing a shell would expand in it.
fn literal(word: &str) -> bool {
    !word.is_empty() && !word.starts_with('~') && !word.contains(['$', '`', '%', '*', '?'])
}

fn is_absolute_word(p: &str) -> bool {
    crate::paths::is_absolute_str(p) || is_msys_drive(p)
}

/// `/c/Users/…`: Git Bash's spelling of `C:/Users/…`.
fn is_msys_drive(p: &str) -> bool {
    let b = p.as_bytes();
    b.len() >= 2 && b[0] == b'/' && b[1].is_ascii_alphabetic() && (b.len() == 2 || b[2] == b'/')
}

fn join_word(base: &str, p: &str) -> String {
    if is_absolute_word(p) || base.is_empty() {
        p.to_string()
    } else {
        format!("{}/{}", base.trim_end_matches(['/', '\\']), p)
    }
}

/// The script a shell wrapper runs, with its dialect: `bash -c "<script>"`
/// (also `sh`, `zsh`, `dash`, and combined flags like `-lc`), `cmd /c <…>`,
/// `powershell -Command <…>` / `pwsh -c <…>`.
fn nested_script(exe: &str, words: &[String]) -> Option<(String, Dialect)> {
    // The script as one word (`cmd /c "a && b"`) is the script; several
    // words (`cmd /c git reset --hard`) are put back together, re-quoting any
    // that held whitespace.
    let rejoin = |rest: &[String]| {
        if let [one] = rest {
            return one.clone();
        }
        rest.iter()
            .map(|w| {
                if w.contains(char::is_whitespace) {
                    format!("\"{w}\"")
                } else {
                    w.clone()
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    };
    match exe {
        "bash" | "sh" | "zsh" | "dash" | "ksh" => {
            let i = words.iter().position(|w| {
                w.len() > 1 && w.starts_with('-') && !w.starts_with("--") && w.contains('c')
            })?;
            Some((words.get(i + 1)?.clone(), Dialect::Posix))
        }
        "cmd" => {
            let i = words
                .iter()
                .position(|w| w.eq_ignore_ascii_case("/c") || w.eq_ignore_ascii_case("/k"))?;
            Some((rejoin(&words[i + 1..]), Dialect::Cmd))
        }
        "powershell" | "pwsh" => {
            let i = words.iter().position(|w| {
                let l = w.to_ascii_lowercase();
                l == "-c" || (l.len() >= 4 && "-command".starts_with(&l))
            })?;
            Some((rejoin(&words[i + 1..]), Dialect::PowerShell))
        }
        _ => None,
    }
}

/// [`parse_git`], also returning each `-C <dir>` in order and whether git is
/// pointed at a repository or work tree the line names some other way
/// (`--git-dir`, `--work-tree`).
fn parse_git_dirs(words: &[String]) -> Option<(GitInvocation, Vec<String>, bool)> {
    if exe_basename(words.first()?) != "git" {
        return None;
    }
    let mut dirs = Vec::new();
    let mut elsewhere = false;
    let mut i = 1;
    while i < words.len() {
        let w = words[i].as_str();
        match w {
            "-C" => {
                dirs.push(words.get(i + 1)?.clone());
                i += 2;
                continue;
            }
            "--git-dir" | "--work-tree" => {
                elsewhere = true;
                i += 2;
                continue;
            }
            "-c" | "--namespace" => {
                i += 2;
                continue;
            }
            _ if w.starts_with("--git-dir=") || w.starts_with("--work-tree=") => {
                elsewhere = true;
            }
            _ if w.starts_with('-') => {}
            _ => break,
        }
        i += 1;
    }
    let sub = words.get(i)?.clone();
    Some((
        GitInvocation {
            sub,
            args: words[i + 1..].to_vec(),
        },
        dirs,
        elsewhere,
    ))
}

// ------------------------------------------------------------ restore reach

/// Which work-tree files a restore-family call can have changed, read from
/// its arguments alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reach {
    /// Every file of the repository it runs in (`reset --hard`, a bare
    /// `stash`, `clean` without paths).
    Tree,
    /// The files these pathspecs match, relative to the call's directory.
    Paths(Vec<String>),
    /// No work-tree file (`restore --staged`, `clean -n`, a `restore` with no
    /// pathspec, which git refuses).
    Nothing,
    /// The arguments do not say (`-p`, `--pathspec-from-file`, pathspec
    /// magic).
    Unknown,
}

/// A restore-family call and what it can reach.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreTarget {
    /// The subcommand's text, as written.
    pub command: String,
    /// Its directory (see [`GitCall::base`]).
    pub base: Option<String>,
    pub reach: Reach,
}

/// Every restore-family call in `cmd` with its reach. Unlike
/// [`git_effects`], no argument is checked against the disk: a
/// `git checkout <x>` whose `x` was a file when the hook ran is recognised
/// then, and here every non-option argument is a candidate pathspec.
pub fn restore_targets(cmd: &str, dialect: Dialect) -> Vec<RestoreTarget> {
    if !cmd.contains("git") {
        return Vec::new();
    }
    git_calls(cmd, dialect)
        .into_iter()
        .filter(|c| is_restore_family(&c.git, &|_| true))
        .map(|c| RestoreTarget {
            reach: reach_of(&c.git),
            command: c.text,
            base: c.base,
        })
        .collect()
}

fn has_short(a: &str, flag: char) -> bool {
    a.len() > 1 && a.starts_with('-') && !a.starts_with("--") && a[1..].contains(flag)
}

fn reach_of(g: &GitInvocation) -> Reach {
    let args = &g.args;
    let dd = args.iter().position(|a| a == "--");
    let (opts, after) = match dd {
        Some(i) => (&args[..i], &args[i + 1..]),
        None => (&args[..], &args[..0]),
    };
    if args
        .iter()
        .any(|a| a == "-p" || a == "--patch" || a.starts_with("--pathspec-from-file") || a == "-i")
    {
        return Reach::Unknown;
    }
    // Non-option words before `--`, skipping the values of options that take
    // one.
    let words = |valued: &[&str]| -> Vec<String> {
        let mut out = Vec::new();
        let mut skip = false;
        for a in opts {
            if skip {
                skip = false;
                continue;
            }
            if valued.contains(&a.as_str()) {
                skip = true;
            } else if !a.starts_with('-') {
                out.push(a.clone());
            }
        }
        out
    };
    let specs = |mut v: Vec<String>| -> Reach {
        v.extend(after.iter().cloned());
        // A word the shell would expand (`$(git diff --name-only)`, `$F`,
        // `%F%`, `~/x`) names nothing the line spells out.
        let expanded = |s: &String| s.contains(['$', '`', '%']) || s.starts_with('~');
        // Pathspec magic other than "the top of the tree".
        let magic = |s: &String| s.starts_with(':') && s != ":/" && s != ":";
        if v.is_empty() {
            Reach::Nothing
        } else if v.iter().any(|s| expanded(s) || magic(s)) {
            Reach::Unknown
        } else if v.iter().any(|s| s == ":/" || s == ":") {
            Reach::Tree
        } else {
            Reach::Paths(v)
        }
    };
    match g.sub.as_str() {
        "restore" => {
            let staged = args.iter().any(|a| a == "--staged" || has_short(a, 'S'));
            let worktree = args.iter().any(|a| a == "--worktree" || has_short(a, 'W'));
            if staged && !worktree {
                return Reach::Nothing;
            }
            specs(words(&["-s", "--source"]))
        }
        "checkout" => {
            if opts
                .iter()
                .any(|a| matches!(a.as_str(), "-b" | "-B" | "--orphan"))
            {
                return Reach::Unknown;
            }
            match dd {
                // `git checkout <tree-ish> -- <paths>`: what precedes `--` is
                // the tree-ish.
                Some(_) => specs(Vec::new()),
                None => specs(words(&["--conflict"])),
            }
        }
        "reset" => Reach::Tree,
        "stash" => {
            let first = args.first().map(String::as_str);
            match first {
                None => Reach::Tree,
                Some("save") => Reach::Tree,
                Some(a) if a == "push" || a.starts_with('-') => {
                    let v: Vec<String> = words(&["-m", "--message"])
                        .into_iter()
                        .filter(|w| w != "push")
                        .collect();
                    match specs(v) {
                        Reach::Nothing => Reach::Tree,
                        r => r,
                    }
                }
                Some(_) => Reach::Nothing,
            }
        }
        "clean" => {
            if args.iter().any(|a| a == "--dry-run" || has_short(a, 'n')) {
                return Reach::Nothing;
            }
            match specs(words(&["-e", "--exclude"])) {
                Reach::Nothing => Reach::Tree,
                r => r,
            }
        }
        _ => Reach::Unknown,
    }
}

/// Whether `target` can have changed `file`, as far as the command line
/// says: `Some(true)` / `Some(false)`, or `None` when it does not say.
///
/// `file` is a stored path (project-relative, or absolute outside the
/// project), `cwd` the directory the command started in and `root` the
/// project root, both absolute. Matching is lexical: a pathspec names the
/// file, a directory above it, or matches it as a glob (`*`, `?`); `.` and
/// `..` are resolved without touching the disk.
pub fn reaches(target: &RestoreTarget, file: &str, cwd: Option<&str>, root: &str) -> Option<bool> {
    use crate::paths::{is_absolute_str, paths_equal};
    let root = clean_abs(root, root)?;
    let file_abs = if is_absolute_str(file) {
        clean_abs(file, &root)?
    } else {
        clean_abs(&format!("{root}/{file}"), &root)?
    };
    let base = target.base.as_deref()?;
    let dir = match (is_absolute_word(base), cwd) {
        (true, _) => clean_abs(base, &root)?,
        (false, Some(c)) => clean_abs(&join_word(c, base), &root)?,
        (false, None) => return None,
    };
    let within = |path: &str, dir: &str| {
        paths_equal(path, dir)
            || (path.len() > dir.len()
                && path.get(..dir.len()).is_some_and(|h| paths_equal(h, dir))
                && (path.as_bytes()[dir.len()] == b'/' || dir.ends_with('/')))
    };
    match &target.reach {
        Reach::Nothing => Some(false),
        Reach::Unknown => None,
        Reach::Tree => {
            // The repository `dir` is in holds the project when `dir` is the
            // root, inside it, or above it. A tree-wide call made from
            // anywhere else is about some other checkout.
            if within(&dir, &root) || within(&root, &dir) {
                Some(within(&file_abs, &root))
            } else {
                None
            }
        }
        Reach::Paths(specs) => {
            let mut unsure = false;
            for spec in specs {
                let glob = spec.contains(['*', '?', '[']);
                if glob && (spec.contains('[') || spec.contains("..")) {
                    unsure = true;
                    continue;
                }
                let Some(full) = clean_abs(&join_word(&dir, spec), &root) else {
                    unsure = true;
                    continue;
                };
                let hit = if glob {
                    glob_match(&full, &file_abs)
                } else {
                    within(&file_abs, &full)
                };
                if hit {
                    return Some(true);
                }
            }
            (!unsure).then_some(false)
        }
    }
}

/// `p` as an absolute `/`-separated path with `.` and `..` resolved
/// lexically. `/c/…` (Git Bash) becomes `C:/…` when `like` has a drive
/// letter. `None` for a relative path or one that climbs above its root.
fn clean_abs(p: &str, like: &str) -> Option<String> {
    let mut s = crate::paths::normalize_abs(p);
    let drive = like.as_bytes().get(1) == Some(&b':');
    if drive && is_msys_drive(&s) {
        let letter = s.as_bytes()[1].to_ascii_uppercase() as char;
        s = format!("{letter}:{}", &s[2..]);
        if s.len() == 2 {
            s.push('/');
        }
    }
    if !crate::paths::is_absolute_str(&s) {
        return None;
    }
    let (prefix, rest) = if let Some(unc) = s.strip_prefix("//") {
        ("//", unc)
    } else if s.as_bytes().get(1) == Some(&b':') {
        (&s[..3], &s[3..])
    } else {
        ("/", &s[1..])
    };
    let mut parts: Vec<&str> = Vec::new();
    for c in rest.split('/') {
        match c {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            c => parts.push(c),
        }
    }
    Some(format!("{prefix}{}", parts.join("/")))
}

/// Git's default pathspec glob: `*` and `?` also match `/`. Case-insensitive
/// on Windows, like the paths it is compared with.
fn glob_match(pattern: &str, text: &str) -> bool {
    let fold = |s: &str| {
        if cfg!(windows) {
            s.to_lowercase()
        } else {
            s.to_string()
        }
    };
    let (p, t): (Vec<char>, Vec<char>) = (
        fold(pattern).chars().collect(),
        fold(text).chars().collect(),
    );
    let (mut pi, mut ti) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi, ti));
            pi += 1;
        } else if let Some((sp, st)) = star {
            pi = sp + 1;
            ti = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
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
    fn display_drops_leading_setup_only() {
        assert_eq!(
            display_command(r#"cd "C:\Users\me\proj" && python -m pytest -q"#),
            "python -m pytest -q"
        );
        assert_eq!(
            display_command("cd /tmp/x && git restore a.py && pytest -q"),
            "git restore a.py && pytest -q"
        );
        // Pipelines keep their separators because the tail is returned verbatim.
        assert_eq!(
            display_command("cd /tmp && pytest -q | tail -5"),
            "pytest -q | tail -5"
        );
        // Nothing to drop.
        assert_eq!(display_command("  pytest -q  "), "pytest -q");
        // Setup only: there is nothing better to show than the line itself.
        assert_eq!(display_command("cd /tmp"), "cd /tmp");
        // Only a leading run is dropped; a later `cd` stays.
        assert_eq!(
            display_command("make build && cd out && ./run"),
            "make build && cd out && ./run"
        );
        assert_eq!(display_command(r"Set-Location 'C:\p' ; pytest"), "pytest");
        assert_eq!(display_command("FOO=1 BAR=2 ; pytest -q"), "pytest -q");
    }

    #[test]
    fn subcommand_ranges_slice_the_original() {
        let cmd = "cd a && npm test || echo x";
        for ((start, end), part) in subcommand_ranges(cmd)
            .into_iter()
            .zip(split_subcommands(cmd))
        {
            assert_eq!(&cmd[start..end], part);
        }
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

    // ------------------------------------------------ reach and attribution

    const ROOT: &str = "/w/proj";

    /// Whether the line's restore-family calls reach `file` (stored path)
    /// from `cwd`: the first answer that is not `Some(false)`, as the reducer
    /// reads it.
    fn reach(cmd: &str, dialect: Dialect, cwd: Option<&str>, file: &str) -> Option<bool> {
        let targets = restore_targets(cmd, dialect);
        let mut any_unknown = false;
        for t in &targets {
            match reaches(t, file, cwd, ROOT) {
                Some(true) => return Some(true),
                None => any_unknown = true,
                Some(false) => {}
            }
        }
        (!any_unknown).then_some(false)
    }

    fn posix(cmd: &str, file: &str) -> Option<bool> {
        reach(cmd, Dialect::Posix, Some(ROOT), file)
    }

    #[test]
    fn a_restore_reaches_only_what_its_pathspec_names() {
        assert_eq!(posix("git restore a.py", "a.py"), Some(true));
        assert_eq!(posix("git restore a.py", "b.py"), Some(false));
        assert_eq!(posix("git restore src/", "src/x/a.py"), Some(true));
        assert_eq!(posix("git restore src", "srcx/a.py"), Some(false));
        assert_eq!(posix("git restore .", "src/a.py"), Some(true));
        assert_eq!(posix("git restore -- a.py b.py", "b.py"), Some(true));
        assert_eq!(
            posix("git restore --source HEAD~1 a.py", "a.py"),
            Some(true)
        );
        assert_eq!(
            posix("git restore --source HEAD~1 a.py", "HEAD~1"),
            Some(false)
        );
        // Compound lines: every separator, and the effect is the restore's
        // whatever the line's final status.
        for line in [
            "git restore a.py && pytest",
            "pytest ; git restore a.py",
            "pytest || git restore a.py",
            "pytest | git restore a.py",
            "echo x\ngit restore a.py",
        ] {
            assert_eq!(posix(line, "a.py"), Some(true), "{line}");
            assert_eq!(posix(line, "b.py"), Some(false), "{line}");
        }
    }

    #[test]
    fn the_directory_a_restore_runs_in_is_followed_where_the_line_spells_it() {
        assert_eq!(posix("cd sub && git restore a.py", "sub/a.py"), Some(true));
        assert_eq!(posix("cd sub && git restore a.py", "a.py"), Some(false));
        assert_eq!(
            posix("cd \"/w/proj\" && git restore a.py", "a.py"),
            Some(true)
        );
        assert_eq!(
            posix("cd /w/other && git restore a.py", "a.py"),
            Some(false)
        );
        assert_eq!(posix("git -C sub restore a.py", "sub/a.py"), Some(true));
        assert_eq!(
            posix("git -C sub -C deeper restore a.py", "sub/deeper/a.py"),
            Some(true)
        );
        assert_eq!(
            posix("cd sub && git -C .. restore a.py", "a.py"),
            Some(true)
        );
        // A directory the line does not spell out: unknown, not a guess.
        assert_eq!(posix("cd $DIR && git restore a.py", "a.py"), None);
        assert_eq!(posix("cd ~/proj && git restore a.py", "a.py"), None);
        assert_eq!(posix("cd - && git restore a.py", "a.py"), None);
        assert_eq!(posix("cd && git restore a.py", "a.py"), None);
        assert_eq!(posix("git --work-tree=/x restore a.py", "a.py"), None);
        // The command's own cwd unknown: a relative pathspec says nothing.
        assert_eq!(
            reach("git restore a.py", Dialect::Posix, None, "a.py"),
            None
        );
        // ...but an absolute `cd` still does.
        assert_eq!(
            reach(
                "cd /w/proj && git restore a.py",
                Dialect::Posix,
                None,
                "a.py"
            ),
            Some(true)
        );
    }

    #[test]
    fn what_the_arguments_cannot_say_is_unknown() {
        assert_eq!(posix("git restore -p a.py", "a.py"), None);
        assert_eq!(posix("git checkout -p -- a.py", "a.py"), None);
        assert_eq!(
            posix("git restore --pathspec-from-file=l.txt", "a.py"),
            None
        );
        assert_eq!(posix("git restore ':(icase)A.PY'", "a.py"), None);
        assert_eq!(posix("git restore '[ab].py'", "a.py"), None);
        // A pathspec the shell expands names nothing the line spells out.
        assert_eq!(posix("git restore $(git diff --name-only)", "a.py"), None);
        assert_eq!(posix("git restore \"$F\"", "a.py"), None);
        assert_eq!(posix("git restore ~/proj/a.py", "a.py"), None);
    }

    #[test]
    fn tree_wide_and_index_only_calls() {
        assert_eq!(posix("git reset --hard HEAD", "src/a.py"), Some(true));
        assert_eq!(posix("git stash", "a.py"), Some(true));
        assert_eq!(posix("git stash -u", "a.py"), Some(true));
        assert_eq!(posix("git stash push -m wip", "a.py"), Some(true));
        assert_eq!(posix("git stash push -- a.py", "b.py"), Some(false));
        assert_eq!(posix("git stash push a.py", "a.py"), Some(true));
        assert_eq!(posix("git clean -fd", "new.py"), Some(true));
        assert_eq!(posix("git clean -fdn", "new.py"), Some(false));
        assert_eq!(posix("git clean -fd tmp/", "new.py"), Some(false));
        assert_eq!(posix("git restore :/", "deep/a.py"), Some(true));
        // The index only.
        assert_eq!(posix("git restore --staged a.py", "a.py"), Some(false));
        assert_eq!(posix("git restore -S -W a.py", "a.py"), Some(true));
        // `git restore` without a pathspec is refused by git.
        assert_eq!(posix("git restore", "a.py"), Some(false));
        // A tree-wide call made in some other checkout.
        assert_eq!(posix("cd /w/other && git reset --hard", "a.py"), None);
        // One made below the project root is the project's repository.
        assert_eq!(posix("cd src && git reset --hard", "a.py"), Some(true));
        // A file outside the project is not the project's work tree.
        assert_eq!(posix("git reset --hard", "/elsewhere/a.py"), Some(false));
    }

    #[test]
    fn checkout_pathspecs() {
        assert_eq!(posix("git checkout -- a.py", "a.py"), Some(true));
        assert_eq!(posix("git checkout HEAD~2 -- a.py", "a.py"), Some(true));
        assert_eq!(posix("git checkout HEAD~2 -- a.py", "b.py"), Some(false));
        assert_eq!(posix("git checkout .", "a.py"), Some(true));
        assert_eq!(posix("git checkout main a.py", "a.py"), Some(true));
        assert_eq!(posix("git checkout -b topic a.py", "a.py"), None);
    }

    #[test]
    fn globs_follow_gits_default_pathspec_matching() {
        assert_eq!(posix("git checkout -- '*.py'", "src/deep/a.py"), Some(true));
        assert_eq!(posix("git checkout -- 'src/*.py'", "src/a.py"), Some(true));
        assert_eq!(posix("git checkout -- 'src/*.py'", "lib/a.py"), Some(false));
        assert_eq!(posix("git restore 'a?.py'", "ab.py"), Some(true));
        assert_eq!(posix("git restore 'a?.py'", "abc.py"), Some(false));
    }

    #[test]
    fn shell_wrappers_are_opened_one_level_deep() {
        assert_eq!(posix("bash -c \"git restore a.py\"", "a.py"), Some(true));
        assert_eq!(
            posix("bash -lc 'cd sub && git restore a.py'", "sub/a.py"),
            Some(true)
        );
        assert_eq!(
            posix("sh -c 'git restore a.py' && pytest", "b.py"),
            Some(false)
        );
        // A child shell's `cd` does not move the parent.
        assert_eq!(
            posix("bash -c 'cd sub' && git restore a.py", "a.py"),
            Some(true)
        );
        assert_eq!(posix("cmd /c \"git restore a.py\"", "a.py"), Some(true));
        assert_eq!(
            posix(
                "powershell -NoProfile -Command \"git restore a.py\"",
                "a.py"
            ),
            Some(true)
        );
        assert_eq!(posix("pwsh -c git restore a.py", "a.py"), Some(true));
        // Detection sees through them too.
        assert!(git_effects("bash -c 'git restore a.py'", &no_files)
            .restore
            .is_some());
        assert!(git_effects("cmd /c git reset --hard", &no_files)
            .restore
            .is_some());
    }

    #[test]
    fn windows_paths_under_each_dialect() {
        const WROOT: &str = "C:/w/proj";
        let win = |cmd: &str, d: Dialect, file: &str| {
            let targets = restore_targets(cmd, d);
            targets
                .iter()
                .find_map(|t| reaches(t, file, Some("C:\\w\\proj"), WROOT).filter(|r| *r))
                .or_else(|| {
                    targets
                        .iter()
                        .all(|t| reaches(t, file, Some("C:\\w\\proj"), WROOT) == Some(false))
                        .then_some(false)
                })
        };
        // Quoted, with spaces, backslashes, and a trailing backslash: a
        // complete string in PowerShell and cmd.
        let ps = "cd \"C:\\w\\proj\\\" ; git restore src\\a.py";
        assert_eq!(win(ps, Dialect::PowerShell, "src/a.py"), Some(true));
        let cmd = "cd /d \"C:\\w\\proj\\\" & git restore src\\a.py";
        assert_eq!(win(cmd, Dialect::Cmd, "src/a.py"), Some(true));
        // In a POSIX shell `\"` escapes the quote: the line never ends its
        // string, and bash would not run it as written. Nothing is credited.
        assert_eq!(win(ps, Dialect::Posix, "src/a.py"), Some(false));
        // Drive letters compare without regard to case; Git Bash spells
        // `C:` as `/c`.
        assert_eq!(
            win(
                "cd /c/w/proj && git restore src/a.py",
                Dialect::Posix,
                "src/a.py"
            ),
            Some(true)
        );
        assert_eq!(
            win(
                "git -C 'c:\\w\\proj' restore src/a.py",
                Dialect::Posix,
                "src/a.py"
            ),
            Some(true)
        );
        assert_eq!(
            win(
                "cd \"C:\\w\\my proj\" && git restore a.py",
                Dialect::Posix,
                "a.py"
            ),
            Some(false)
        );
        // Detection under PowerShell quoting: the trailing backslash does not
        // swallow the rest of the line.
        assert!(git_effects_in(ps, Dialect::PowerShell, &no_files)
            .restore
            .is_some());
        // The PowerShell escape is a backtick.
        assert_eq!(
            tokenize_in("git restore `\"a b`\".py 'c d'", Dialect::PowerShell),
            ["git", "restore", "\"a", "b\".py", "c d"]
        );
        assert_eq!(
            split_subcommands_in("cmd1 & cmd2 ^& not && cmd3", Dialect::Cmd),
            ["cmd1", "cmd2 ^& not", "cmd3"]
        );
    }

    fn split_subcommands_in(cmd: &str, d: Dialect) -> Vec<&str> {
        subcommand_ranges_in(cmd, d)
            .into_iter()
            .map(|(a, b)| &cmd[a..b])
            .collect()
    }

    #[test]
    fn lexical_paths() {
        assert_eq!(clean_abs("/a/./b/../c", "/"), Some("/a/c".into()));
        assert_eq!(clean_abs("/a/../..", "/"), None);
        assert_eq!(clean_abs("c:\\a\\..\\b", "C:/"), Some("C:/b".into()));
        assert_eq!(clean_abs("/c/a", "C:/w"), Some("C:/a".into()));
        assert_eq!(clean_abs("/c/a", "/w"), Some("/c/a".into()));
        assert_eq!(clean_abs("rel/a", "/w"), None);
        assert!(glob_match("/w/*.py", "/w/a/b.py"));
        assert!(!glob_match("/w/*.py", "/w/a/b.pyc"));
        assert!(glob_match("/w/**", "/w/x"));
    }

    /// No command line, however malformed, panics the analysis.
    #[test]
    fn malformed_lines_never_panic() {
        let samples = [
            "",
            "\"",
            "'",
            "`",
            "^",
            "&&",
            "||",
            ";;",
            "cd",
            "git",
            "git -C",
            "git -C \"",
            "cd \"\u{e9}\u{301}\" && git restore \u{1f600}.py",
            "git restore ':(",
            "bash -c",
            "cmd /c",
            "pwsh -Command",
            "git restore -- ",
            "cd /d",
            "git stash push -m",
            "git -c",
            "git --git-dir",
            "\u{0}git restore a",
        ];
        for s in samples {
            for d in [Dialect::Posix, Dialect::PowerShell, Dialect::Cmd] {
                let _ = git_effects_in(s, d, &|_| true);
                for t in restore_targets(s, d) {
                    let _ = reaches(&t, "\u{e9}.py", Some("C:\\\u{e9}"), "C:/\u{e9}");
                    let _ = reaches(&t, "a.py", None, "/");
                }
            }
        }
    }
}

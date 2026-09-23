//! velra — local-first session continuity for Claude Code.
//!
//! The hook path (`velra hook <event>`, `velra reduce`) dispatches straight
//! from `argv` and never initializes clap, a logger, an async runtime or a
//! global regex (§3.2).

mod atomic;
mod cli;
mod compat;
mod home;
mod hook;
mod inspect;
mod log;
mod normalize;
mod restore;
mod settings;

fn main() {
    let ts_ms = velra_core::time::now_ms();
    let mut args = std::env::args_os().skip(1);
    let first = args.next();
    let first = first.as_ref().map(|a| a.to_string_lossy().into_owned());
    match first.as_deref() {
        Some("hook") => {
            hook::run(
                "hook",
                args.next().map(|a| a.to_string_lossy().into_owned()),
                ts_ms,
            );
            std::process::exit(0);
        }
        Some("reduce") => {
            hook::run("reduce", None, ts_ms);
            std::process::exit(0);
        }
        _ => std::process::exit(cli::run()),
    }
}

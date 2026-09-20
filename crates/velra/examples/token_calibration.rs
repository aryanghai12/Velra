//! Compares `estimate_tokens` against real Anthropic tokenizer counts.
//!
//!     cargo run -p velra --example token_calibration -- tests/fixtures/tokenizer
//!
//! The directory must hold a `measured.json` mapping each capsule file to the
//! token count it really cost, as `bench/harness/measure_tokens.py` records it.
//! A positive `under%` is the estimator reading *below* the real cost, which is
//! the direction that breaks the budget: it is what let a capsule rendered to a
//! 745-token target ship at 804 real tokens and fail hypothesis E4 of the
//! v0.1.1 efficacy benchmark. It must stay at or below zero on every fixture.
//!
//! Development tool only; `crates/velra/tests/capsule.rs` asserts the same
//! property in CI.

use std::path::PathBuf;

fn main() {
    let dir = PathBuf::from(
        std::env::args()
            .nth(1)
            .unwrap_or_else(|| "tests/fixtures/tokenizer".to_string()),
    );
    let measured: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(dir.join("measured.json")).expect("measured"),
    )
    .expect("json");

    let mut names: Vec<&String> = measured.as_object().expect("object").keys().collect();
    names.sort();

    println!("{:<44} {:>6} {:>6} {:>8}", "file", "est", "real", "under%");
    let mut worst_under = f64::MIN;
    let mut worst_over = f64::MIN;
    for name in names {
        let text = std::fs::read_to_string(dir.join(name)).expect("fixture");
        let real = measured[name]["measured_tokens"].as_u64().expect("count") as f64;
        let est = f64::from(velra_core::text::estimate_tokens(&text));
        let under = (real - est) / real * 100.0;
        worst_under = worst_under.max(under);
        worst_over = worst_over.max(-under);
        println!("{name:<44} {est:>6} {real:>6} {under:>7.2}%");
    }
    println!("\nworst under-read: {worst_under:.2}%  (must be <= 0)");
    println!("worst over-read:  {worst_over:.2}%  (budget the capsule declines to use)");
    if worst_under > 0.0 {
        eprintln!("\nFAIL: the estimator reads below the real tokenizer on at least one capsule.");
        std::process::exit(1);
    }
}

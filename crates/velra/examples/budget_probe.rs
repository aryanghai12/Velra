//! Sweeps the render budget against a real trial database.
//!
//!     cargo run --release -p velra --example budget_probe -- <velra_home> [session_id]
//!
//! Answers one question the benchmark kept raising indirectly: at which target
//! does `[FILE_ACTIVITY]` survive, and what does the capsule estimate there?
//! Development tool only; not part of the shipped binary.

use std::path::PathBuf;
use velra_core::db::{Db, Role};
use velra_core::model::Trigger;
use velra_core::render::{self, RenderConfig};
use velra_core::snapshot::{self, SnapshotMeta};

fn main() {
    let mut args = std::env::args().skip(1);
    let home = PathBuf::from(
        args.next()
            .expect("usage: budget_probe <velra_home> [session]"),
    );
    let db = Db::open(&home.join("velra.db"), Role::Cli).expect("open database");

    let session: String = match args.next() {
        Some(s) => s,
        None => db
            .conn
            .query_row(
                "SELECT session_id FROM events ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .expect("a session in the database"),
    };

    let meta = SnapshotMeta {
        checkpoint_id: "ckpt_probe".into(),
        created_ms: velra_core::time::now_ms(),
        trigger: Trigger::Manual,
        partial: false,
        preview: true,
        tz_offset_secs: 0,
    };
    let snap = snapshot::build(&db.conn, &session, &meta).expect("snapshot");

    println!("session {session}");
    println!(" target    est   chars  steps  sections");
    for target in [640, 680, 700, 720, 745, 754, 760, 775, 790, 800, 850] {
        let r = render::render(
            &snap,
            &RenderConfig {
                budget_tokens: target,
            },
        );
        let sections: Vec<&str> = r
            .text
            .lines()
            .filter_map(|l| l.strip_prefix('['))
            .filter_map(|l| l.split(']').next())
            .collect();
        let has_wf = sections.contains(&"FILE_ACTIVITY");
        println!(
            "{target:>7}  {:>5}  {:>6}  {:>5}  FILE_ACTIVITY={}  [{}]",
            r.tokens,
            r.text.chars().count(),
            r.steps,
            if has_wf { "YES" } else { "no " },
            sections.join(" ")
        );
    }
}

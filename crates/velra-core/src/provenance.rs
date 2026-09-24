//! Where a piece of state stopped: `velra inspect --trace <MARKER>`.
//!
//! Given any string -- a test name, a symbol, a path, a phrase the user typed
//! -- this walks it through every layer a capsule is made from and reports
//! PRESENT or ABSENT at each, in pipeline order:
//!
//! ```text
//! source_event      the Claude Code transcript Velra recorded for the session
//! normalized_state  the hook payloads Velra stored (`events`)
//! ledger            the reducer's projections (intents, commands, files, ...)
//! snapshot          `snapshot::build`, the selection a capsule is made from
//! renderer          `render::render` under the configured budget
//! restore           what `velra restore` stages: rendered with restore's own
//!                   framing (its checkpoint id, so its budget), then redacted
//! capsule           the capsule actually staged for this workspace, if any
//! ```
//!
//! The first PRESENT -> ABSENT transition is the first loss, and the reason is
//! read off the rows and the ladder, never guessed: a superseded intent says
//! which prompt replaced it, a budgeted-out line names the ladder rung that
//! removed it. When no specific reason applies the report says so.
//!
//! It exists because the v0.1.2 Token-Burn qualification diagnosed its own
//! misses from the final capsule alone and called them capture failures. The
//! ledger held every marker; two were lost by the snapshot and one by the
//! renderer. This makes that distinction a query instead of an investigation.

use crate::reducer::session_epoch;
use crate::render::{self, RenderConfig, Snapshot};
use crate::snapshot::{self, SnapshotMeta};
use rusqlite::{params, Connection, OptionalExtension};

/// Presence of a marker at one layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presence {
    Present,
    Absent,
    /// The layer could not be read (no transcript on disk, nothing staged).
    Unavailable,
}

impl Presence {
    pub fn as_str(self) -> &'static str {
        match self {
            Presence::Present => "PRESENT",
            Presence::Absent => "ABSENT",
            Presence::Unavailable => "N/A",
        }
    }
}

/// One layer's verdict with the rows or fields that justify it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayerResult {
    pub layer: &'static str,
    pub presence: Presence,
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkerTrace {
    pub marker: String,
    pub layers: Vec<LayerResult>,
    /// The first layer at which a PRESENT marker became ABSENT.
    pub first_loss: Option<&'static str>,
    pub reason: Option<String>,
}

/// Inputs that are not in the database.
pub struct TraceInputs<'a> {
    pub meta: &'a SnapshotMeta,
    pub cfg: &'a RenderConfig,
    /// The capsule staged for this workspace from this session, when there is
    /// one. `None` reports the layer as N/A.
    pub staged: Option<&'a str>,
}

fn norm(s: &str) -> String {
    s.replace('\\', "/").to_lowercase()
}

fn contains(hay: &str, needle: &str) -> bool {
    norm(hay).contains(needle)
}

/// Every text field of a snapshot, labelled with where it lives.
pub fn snapshot_texts(s: &Snapshot) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut push = |label: String, text: &str| out.push((label, text.to_string()));
    if let Some(r) = &s.root {
        push(format!("root (intents:{})", r.id), &r.text);
    }
    for c in &s.constraints {
        push(format!("constraints (constraints:{})", c.id), &c.text);
    }
    for c in &s.rejections {
        push(format!("rejections (constraints:{})", c.id), &c.text);
    }
    if let Some(v) = &s.subtask {
        push(format!("subtask (intents:{})", v.id), &v.text);
    }
    if let Some(v) = &s.earlier {
        push(format!("earlier (intents:{})", v.id), &v.text);
    }
    if let Some(v) = &s.latest {
        push(format!("latest (intents:{})", v.id), &v.text);
    }
    if let Some(f) = &s.failure {
        push(format!("failure.command (commands:{})", f.id), &f.command);
        for (i, line) in f.excerpt.iter().enumerate() {
            push(format!("failure.excerpt[{i}] (commands:{})", f.id), line);
        }
    }
    for t in &s.tests {
        push(format!("tests (commands:{})", t.last_fail.id), &t.id);
        if let Some(d) = &t.detail {
            push(format!("tests.detail (commands:{})", t.last_fail.id), d);
        }
    }
    for d in &s.dead_ends {
        push(format!("dead_ends (dead_ends:{})", d.id), &d.path);
        for part in [&d.command, &d.minus, &d.plus].into_iter().flatten() {
            push(format!("dead_ends.detail (dead_ends:{})", d.id), part);
        }
    }
    for a in &s.attempts {
        push(format!("attempts (edits:{})", a.edit_id), &a.path);
    }
    for w in &s.working_files {
        push("working_files".to_string(), &w.path);
    }
    if let Some(t) = &s.next_target {
        push(format!("next_target ({})", t.source), &t.target);
    }
    out
}

/// A ledger row holding the marker, with the reason the snapshot would not
/// select it (when the snapshot indeed lacks it).
struct LedgerHit {
    row: String,
    omission: Option<String>,
}

fn ledger_hits(
    conn: &Connection,
    session_id: &str,
    needle: &str,
    snap: &Snapshot,
) -> rusqlite::Result<Vec<LedgerHit>> {
    let epoch = session_epoch(conn, session_id)?;
    let like = format!("%{needle}%");
    let mut hits = Vec::new();
    let other_epoch = |e: i64| {
        (e != epoch).then(|| {
            format!("earlier epoch: recorded in epoch {e}; the capsule covers epoch {epoch}")
        })
    };

    // Intents. Matching is done in SQL on a lower-cased, slash-normalised copy.
    let mut stmt = conn.prepare(
        "SELECT i.id, i.level, i.epoch, i.superseded_ms, \
           (SELECT MIN(n.id) FROM intents n WHERE n.session_id = i.session_id AND n.epoch = i.epoch \
             AND n.level = i.level AND n.id > i.id), i.text \
         FROM intents i WHERE i.session_id = ?1 \
         AND lower(replace(i.text, '\\', '/')) LIKE ?2 ORDER BY i.id",
    )?;
    let rows = stmt.query_map(params![session_id, like], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, Option<i64>>(3)?,
            r.get::<_, Option<i64>>(4)?,
            r.get::<_, String>(5)?,
        ))
    })?;
    for row in rows {
        let (id, level, e, superseded, next, text) = row?;
        // A row written before v0.1.2 can hold injected context; the snapshot
        // reads through it (`snapshot::user_text`), and that is the reason to
        // report, ahead of any supersession.
        let injected = (!contains(&snapshot::user_text(&text), needle))
            .then(|| injected_reason(&text, needle))
            .flatten();
        let omission = other_epoch(e).or(injected).or_else(|| {
            superseded.map(|_| {
                let by = next.map_or_else(String::new, |n| format!(" by intents:{n}"));
                format!(
                    "superseded: intents:{id} ({level}) was replaced{by}, and the snapshot \
                     carries only the live {level} message plus, when the latest names no \
                     code, the most recent earlier message that does"
                )
            })
        });
        hits.push(LedgerHit {
            row: format!(
                "intents:{id} ({level}, epoch {e}{})",
                if superseded.is_some() {
                    ", superseded"
                } else {
                    ""
                }
            ),
            omission,
        });
    }

    // Constraints.
    let active: Vec<i64> = snap.constraints.iter().map(|c| c.id).collect();
    let mut stmt = conn.prepare(
        "SELECT id, epoch, superseded_ms FROM constraints WHERE session_id = ?1 \
         AND lower(replace(text, '\\', '/')) LIKE ?2 ORDER BY id",
    )?;
    let rows = stmt.query_map(params![session_id, like], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, Option<i64>>(2)?,
        ))
    })?;
    for row in rows {
        let (id, e, superseded) = row?;
        let omission = other_epoch(e)
            .or_else(|| superseded.map(|_| format!("superseded: constraints:{id} was withdrawn")))
            .or_else(|| {
                (!active.contains(&id)).then(|| {
                    "lower priority: beyond the constraint cap (oldest 3 kept)".to_string()
                })
            });
        hits.push(LedgerHit {
            row: format!("constraints:{id}"),
            omission,
        });
    }

    // Commands: excerpt, command line or mentioned paths.
    let active_failure = snap.failure.as_ref().map(|f| f.id);
    let mut stmt = conn.prepare(
        "SELECT c.id, c.epoch, c.kind, c.outcome, \
           lower(replace(coalesce(c.excerpt, ''), '\\', '/')) LIKE ?2, \
           (SELECT MIN(d.id) FROM commands d WHERE d.session_id = c.session_id AND d.epoch = c.epoch \
             AND d.signature = c.signature AND d.outcome = 'PASS' AND d.ts_ms >= c.ts_ms AND d.id > c.id) \
         FROM commands c WHERE c.session_id = ?1 AND ( \
           lower(replace(coalesce(c.excerpt, ''), '\\', '/')) LIKE ?2 \
           OR lower(replace(c.command_text, '\\', '/')) LIKE ?2 \
           OR lower(replace(coalesce(c.mentioned_paths, ''), '\\\\', '/')) LIKE ?2) ORDER BY c.id",
    )?;
    let rows = stmt.query_map(params![session_id, like], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, bool>(4)?,
            r.get::<_, Option<i64>>(5)?,
        ))
    })?;
    for row in rows {
        let (id, e, kind, outcome, in_excerpt, later_pass) = row?;
        let field = if in_excerpt {
            "output excerpt"
        } else {
            "command line"
        };
        let omission = other_epoch(e).or_else(|| {
            Some(if !matches!(kind.as_str(), "test" | "build" | "lint") {
                format!("category reduction: the snapshot carries no {kind} command")
            } else if outcome != "FAIL" {
                format!("not carried: a {outcome} run is referenced, never quoted")
            } else if let Some(p) = later_pass {
                format!("resolved: commands:{p}, a later run of the same command, passed")
            } else if Some(id) != active_failure {
                match active_failure {
                    Some(a) => format!(
                        "superseded: commands:{a} is the most recent open failure and the only \
                         one quoted"
                    ),
                    None => "superseded: no open failure is quoted".to_string(),
                }
            } else {
                "other: the active failure's quoted fields do not include it".to_string()
            })
        });
        hits.push(LedgerHit {
            row: format!("commands:{id} ({kind} {outcome}, {field})"),
            omission,
        });
    }

    // Files.
    let project: Option<String> = conn
        .query_row(
            "SELECT project_id FROM sessions WHERE session_id = ?1",
            [session_id],
            |r| r.get(0),
        )
        .optional()?;
    let root = match project {
        Some(p) => crate::reducer::project_root(conn, &p)?,
        None => None,
    };
    let listed: Vec<&str> = snap.working_files.iter().map(|w| w.path.as_str()).collect();
    let mut stmt = conn.prepare(
        "SELECT path, epoch FROM file_stats WHERE session_id = ?1 \
         AND lower(replace(path, '\\', '/')) LIKE ?2",
    )?;
    let rows = stmt.query_map(params![session_id, like], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
    })?;
    for row in rows {
        let (path, e) = row?;
        let on_disk = root
            .as_deref()
            .is_none_or(|r| crate::paths::resolve(&path, r).exists());
        let omission = other_epoch(e).or_else(|| {
            (!listed.contains(&path.as_str())).then(|| {
                if on_disk {
                    "lower priority: ranked below the working-file cap (8)".to_string()
                } else {
                    "unavailable: the file no longer exists on disk".to_string()
                }
            })
        });
        hits.push(LedgerHit {
            row: format!("file_stats:{path}"),
            omission,
        });
    }

    // Edits and dead ends.
    let mut stmt = conn.prepare(
        "SELECT e.id, e.epoch, e.status, e.path, \
           (SELECT MAX(x.id) FROM edits x WHERE x.session_id = e.session_id AND x.epoch = e.epoch \
             AND x.path = e.path AND x.status = 'ACTIVE' AND x.id > e.id) \
         FROM edits e WHERE e.session_id = ?1 AND ( \
           lower(replace(e.path, '\\', '/')) LIKE ?2 OR lower(replace(coalesce(e.excerpt, ''), '\\', '/')) LIKE ?2) \
         ORDER BY e.id",
    )?;
    let rows = stmt.query_map(params![session_id, like], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, Option<i64>>(4)?,
        ))
    })?;
    for row in rows {
        let (id, e, status, path, later) = row?;
        let omission = other_epoch(e).or_else(|| {
            Some(if status == "ACTIVE" {
                if snap.dead_ends.iter().any(|d| d.path == path) {
                    "duplicate: its file is already listed as a reverted edit".to_string()
                } else if let Some(l) = later {
                    format!("superseded: edits:{l} is a later edit of the same file")
                } else {
                    "lower priority: beyond the attempts cap (4)".to_string()
                }
            } else {
                format!(
                    "invalidated: edits:{id} is {status}; it is carried only through a dead end"
                )
            })
        });
        hits.push(LedgerHit {
            row: format!("edits:{id} ({status})"),
            omission,
        });
    }
    let mut stmt = conn.prepare(
        "SELECT id, epoch, reapplied FROM dead_ends WHERE session_id = ?1 AND ( \
           lower(replace(path, '\\', '/')) LIKE ?2 OR lower(replace(coalesce(command_text, ''), '\\', '/')) LIKE ?2)",
    )?;
    let rows = stmt.query_map(params![session_id, like], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, i64>(2)?,
        ))
    })?;
    for row in rows {
        let (id, e, reapplied) = row?;
        let omission = other_epoch(e).or_else(|| {
            Some(if reapplied != 0 {
                format!("invalidated: dead_ends:{id} was reapplied")
            } else {
                "lower priority: beyond the dead-end cap (4 kept: the most recent revert of \
                 each file first, then further reverts by recency)"
                    .to_string()
            })
        });
        hits.push(LedgerHit {
            row: format!("dead_ends:{id}"),
            omission,
        });
    }
    Ok(hits)
}

/// When `needle` occurs in `prompt` only inside injected context
/// (`crate::prompt`), the reason it is not carried as the user's words.
fn injected_reason(prompt: &str, needle: &str) -> Option<String> {
    let parts = crate::prompt::split(prompt);
    if contains(parts.authored, needle) {
        return None;
    }
    let block = parts.injected.iter().find(|b| contains(b.text, needle))?;
    Some(format!(
        "injected context: it occurs only inside a `<{}>` block ({}) that the client added \
         around the user's prompt, which is not carried as the user's words",
        block.tag,
        block.origin.as_str()
    ))
}

/// [`injected_reason`] over every stored prompt of the session that holds it.
fn injected_in_prompts(
    conn: &Connection,
    events: &[i64],
    needle: &str,
) -> rusqlite::Result<Option<String>> {
    let mut stmt = conn
        .prepare("SELECT payload FROM events WHERE id = ?1 AND hook_event = 'UserPromptSubmit'")?;
    for id in events {
        let payload: Option<String> = stmt.query_row([id], |r| r.get(0)).optional()?;
        let prompt = payload
            .and_then(|p| serde_json::from_str::<serde_json::Value>(&p).ok())
            .and_then(|v| v["prompt"].as_str().map(str::to_string));
        if let Some(reason) = prompt.and_then(|p| injected_reason(&p, needle)) {
            return Ok(Some(format!("{reason} (events:{id})")));
        }
    }
    Ok(None)
}

fn transcript_presence(conn: &Connection, session_id: &str, needle: &str) -> LayerResult {
    let path: Option<String> = conn
        .query_row(
            "SELECT transcript_path FROM sessions WHERE session_id = ?1",
            [session_id],
            |r| r.get(0),
        )
        .optional()
        .ok()
        .flatten()
        .flatten();
    let text = path
        .as_deref()
        .and_then(|p| std::fs::read_to_string(p).ok());
    match (path, text) {
        (Some(p), Some(t)) => {
            let hits = t.lines().filter(|l| contains(l, needle)).count();
            LayerResult {
                layer: "source_event",
                presence: if hits > 0 {
                    Presence::Present
                } else {
                    Presence::Absent
                },
                evidence: vec![format!("{hits} transcript line(s) in {p}")],
            }
        }
        (p, _) => LayerResult {
            layer: "source_event",
            presence: Presence::Unavailable,
            evidence: vec![match p {
                Some(p) => format!("transcript not readable: {p}"),
                None => "no transcript path recorded".to_string(),
            }],
        },
    }
}

/// Traces `marker` through every layer for `session_id`. Read-only.
pub fn trace_marker(
    conn: &Connection,
    session_id: &str,
    inputs: &TraceInputs<'_>,
    marker: &str,
) -> rusqlite::Result<MarkerTrace> {
    let needle = norm(marker);
    let mut layers = Vec::new();

    layers.push(transcript_presence(conn, session_id, &needle));

    let like = format!("%{needle}%");
    // (id, occurrence time, spooled): when each event happened is the hook's
    // clock; when it reached the ledger is its row, and a spooled event
    // reached it after rows that happened later (`crate::order`).
    let matched: Vec<(i64, i64, bool)> = conn
        .prepare(
            "SELECT id, ts_ms, COALESCE(json_extract(payload, '$.spooled'), 0) != 0 FROM events \
             WHERE session_id = ?1 \
             AND lower(replace(replace(payload, '\\\\', '/'), '\\', '/')) LIKE ?2 ORDER BY id",
        )?
        .query_map(params![session_id, like], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let events: Vec<i64> = matched.iter().map(|m| m.0).collect();
    let cursor = crate::db::cursor(conn).unwrap_or(0);
    let unreduced: Vec<i64> = events.iter().copied().filter(|&id| id > cursor).collect();
    layers.push(LayerResult {
        layer: "normalized_state",
        presence: if events.is_empty() {
            Presence::Absent
        } else {
            Presence::Present
        },
        evidence: vec![format!(
            "{} event(s){}",
            events.len(),
            if events.is_empty() {
                String::new()
            } else {
                format!(
                    ": {}",
                    matched
                        .iter()
                        .take(8)
                        .map(|&(id, ts, spooled)| format!(
                            "events:{id} (occurred {}{}{})",
                            crate::time::rfc3339_utc(ts),
                            if spooled {
                                ", spooled: ingested late"
                            } else {
                                ""
                            },
                            if id > cursor { ", not yet reduced" } else { "" }
                        ))
                        .collect::<Vec<_>>()
                        .join(",")
                )
            }
        )],
    });

    let snap = snapshot::build(conn, session_id, inputs.meta)?;
    let hits = ledger_hits(conn, session_id, &needle, &snap)?;
    layers.push(LayerResult {
        layer: "ledger",
        presence: if hits.is_empty() {
            Presence::Absent
        } else {
            Presence::Present
        },
        evidence: hits.iter().map(|h| h.row.clone()).collect(),
    });

    let fields: Vec<String> = snapshot_texts(&snap)
        .into_iter()
        .filter(|(_, text)| contains(text, &needle))
        .map(|(label, _)| label)
        .collect();
    let presence = if fields.is_empty() {
        Presence::Absent
    } else {
        Presence::Present
    };
    let mut evidence = fields;
    // When the selection was made, and from how much of the log.
    evidence.push(format!(
        "built {} from events reduced through events:{cursor}",
        crate::time::rfc3339_utc(inputs.meta.created_ms)
    ));
    layers.push(LayerResult {
        layer: "snapshot",
        presence,
        evidence,
    });

    let ladder = render::render_ladder(&snap, inputs.cfg);
    let rendered = ladder.last().map(|s| s.text.clone()).unwrap_or_default();
    let in_render = contains(&rendered, &needle);
    let in_full = ladder.first().is_some_and(|s| contains(&s.text, &needle));
    layers.push(LayerResult {
        layer: "renderer",
        presence: if in_render {
            Presence::Present
        } else {
            Presence::Absent
        },
        evidence: vec![format!(
            "{} ladder rung(s) applied; at full detail: {}",
            ladder.len().saturating_sub(1),
            if in_full { "PRESENT" } else { "ABSENT" }
        )],
    });

    // What `velra restore` would stage now: its own framing (the source
    // checkpoint's id in the opening tag, where the preview prints `preview`),
    // so its own budget, then redaction. Rendering the preview here instead
    // could report PRESENT for a line the real restore had to cut.
    let restore_meta = crate::restore::snapshot_meta(
        crate::restore::source_checkpoint(conn, session_id)?.as_deref(),
        inputs.meta.created_ms,
        inputs.meta.tz_offset_secs,
    );
    let restore_snap = snapshot::build(conn, session_id, &restore_meta)?;
    let restore_ladder = render::render_ladder(&restore_snap, inputs.cfg);
    let restore_rendered = restore_ladder
        .last()
        .map(|s| s.text.clone())
        .unwrap_or_default();
    let restored = crate::redact::redact(&restore_rendered).into_owned();
    layers.push(LayerResult {
        layer: "restore",
        presence: if contains(&restored, &needle) {
            Presence::Present
        } else {
            Presence::Absent
        },
        evidence: vec![format!(
            "rendered as `velra restore` stages it (checkpoint=\"{}\"), after redaction",
            restore_meta.checkpoint_id
        )],
    });

    layers.push(match inputs.staged {
        Some(text) => LayerResult {
            layer: "capsule",
            presence: if contains(text, &needle) {
                Presence::Present
            } else {
                Presence::Absent
            },
            evidence: vec!["the capsule currently staged for this workspace".into()],
        },
        None => LayerResult {
            layer: "capsule",
            presence: Presence::Unavailable,
            evidence: vec!["nothing staged for this workspace from this session".into()],
        },
    });

    // First PRESENT -> ABSENT transition, skipping layers that could not be read.
    let mut last_known: Option<Presence> = None;
    let mut first_loss = None;
    for l in &layers {
        if l.presence == Presence::Unavailable {
            continue;
        }
        if last_known == Some(Presence::Present) && l.presence == Presence::Absent {
            first_loss = Some(l.layer);
            break;
        }
        last_known = Some(l.presence);
    }

    let injected = match first_loss {
        Some("ledger") => injected_in_prompts(conn, &events, &needle)?,
        _ => None,
    };
    let budget_rung = |ladder: &[render::LadderStep]| {
        ladder
            .windows(2)
            .find(|w| contains(&w[0].text, &needle) && !contains(&w[1].text, &needle))
            .map(|w| w[1].rung)
    };
    let reason = first_loss.map(|layer| match layer {
        "normalized_state" => "not captured: no hook payload Velra stored contains it (assistant \
                               prose and reasoning are not hooked)"
            .to_string(),
        // Not a loss: the ledger has not caught up with the event log.
        "ledger" if !unreduced.is_empty() && unreduced.len() == events.len() => format!(
            "not yet reduced: {} past the reducer cursor (events:{cursor}); the ledger has not \
             caught up with the event log, and nothing was dropped",
            unreduced
                .iter()
                .take(8)
                .map(|i| format!("events:{i}"))
                .collect::<Vec<_>>()
                .join(",")
        ),
        "ledger" => injected.clone().unwrap_or_else(|| {
            format!(
                "not extracted: it occurs only in raw hook payloads ({} event(s)); no reducer \
                 field keeps that part of the payload",
                events.len()
            )
        }),
        "snapshot" => {
            let mut reasons: Vec<String> = Vec::new();
            for h in &hits {
                if let Some(o) = &h.omission {
                    let line = format!("{} -> {o}", h.row);
                    if !reasons.contains(&line) {
                        reasons.push(line);
                    }
                }
            }
            if reasons.is_empty() {
                "other: every ledger row holding it is selected, but no selected field quotes it"
                    .to_string()
            } else {
                reasons.join("; ")
            }
        }
        "renderer" => {
            if !in_full {
                "not rendered: the snapshot field holding it has no capsule line".to_string()
            } else {
                match budget_rung(&ladder) {
                    Some(rung) => format!(
                        "budgeted out: removed by ladder rung `{rung}` (target {} estimated tokens)",
                        inputs.cfg.budget_tokens
                    ),
                    None => "budgeted out".to_string(),
                }
            }
        }
        "restore" => {
            if contains(&restore_rendered, &needle) {
                "redacted: the redaction pass replaced it".to_string()
            } else {
                match budget_rung(&restore_ladder) {
                    Some(rung) => format!(
                        "budgeted out in the restore render, whose framing differs from the \
                         preview: removed by ladder rung `{rung}` (target {} estimated tokens)",
                        inputs.cfg.budget_tokens
                    ),
                    None => "other: the restore render does not carry it".to_string(),
                }
            }
        }
        "capsule" => "stale: the staged capsule predates the current ledger; re-run `velra restore`"
            .to_string(),
        _ => "other".to_string(),
    });

    Ok(MarkerTrace {
        marker: marker.to_string(),
        layers,
        first_loss,
        reason,
    })
}

impl MarkerTrace {
    /// The plain-text report `velra inspect --trace` prints.
    pub fn report(&self) -> String {
        let mut out = format!("MARKER: {}\n", self.marker);
        for l in &self.layers {
            out.push_str(&format!(
                "{:<17} {}",
                format!("{}:", l.layer),
                l.presence.as_str()
            ));
            if !l.evidence.is_empty() {
                out.push_str(&format!("  [{}]", l.evidence.join("; ")));
            }
            out.push('\n');
        }
        out.push_str(&format!(
            "first_loss:       {}\n",
            self.first_loss.unwrap_or("none")
        ));
        if let Some(r) = &self.reason {
            out.push_str(&format!("reason:           {r}\n"));
        }
        out
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "marker": self.marker,
            "layers": self.layers.iter().map(|l| serde_json::json!({
                "layer": l.layer,
                "presence": l.presence.as_str(),
                "evidence": l.evidence,
            })).collect::<Vec<_>>(),
            "first_loss": self.first_loss,
            "reason": self.reason,
        })
    }
}

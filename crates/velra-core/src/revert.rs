//! Revert, discard and reapplication decisions (§13.2–§13.4) as pure
//! functions over version histories. The reducer supplies the rows.

use crate::model::VersionSource;

/// One observed version of a file (a `file_versions` row).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    pub id: i64,
    pub hash: String,
    pub source: VersionSource,
    pub event_id: i64,
    /// Hook timestamp of the event that produced the observation. This is the
    /// *logical* order of the observation; `id` is only the order it happened
    /// to be ingested in, which a spooled event can invert.
    pub ts_ms: i64,
}

/// An ACTIVE edit on the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveEdit {
    pub id: i64,
    pub event_id: i64,
    pub post_hash: String,
}

/// Hash-return revert detection (§13.2).
///
/// `history` is the version sequence for `(session, path)` *before* the new
/// observation, oldest first. Consecutive identical hashes are collapsed;
/// when the new hash differs from the latest version and equals an earlier
/// one, every ACTIVE edit whose post-edit version lies strictly between that
/// earlier version and now is reverted. Returns the reverted edit ids (sorted).
pub fn detect_revert(history: &[Version], new_hash: &str, active: &[ActiveEdit]) -> Vec<i64> {
    if new_hash == crate::hash::UNREADABLE || active.is_empty() {
        return Vec::new();
    }
    // Collapse runs of equal hashes, remembering post_edit event ids per run.
    let mut runs: Vec<(&str, Vec<i64>)> = Vec::new();
    for v in history {
        let post = (v.source == VersionSource::PostEdit).then_some(v.event_id);
        match runs.last_mut() {
            Some((h, ids)) if *h == v.hash => ids.extend(post),
            _ => runs.push((v.hash.as_str(), post.into_iter().collect())),
        }
    }
    let Some((last, _)) = runs.last() else {
        return Vec::new();
    };
    if *last == new_hash {
        return Vec::new();
    }
    let Some(j) = runs.iter().rposition(|(h, _)| *h == new_hash) else {
        return Vec::new();
    };
    let between: Vec<i64> = runs[j + 1..]
        .iter()
        .flat_map(|(_, ids)| ids.iter().copied())
        .collect();
    let mut out: Vec<i64> = active
        .iter()
        .filter(|e| between.contains(&e.event_id))
        .map(|e| e.id)
        .collect();
    out.sort_unstable();
    out
}

/// Whether a git restore-family command discarded the file's ACTIVE edits
/// (§13.3). The file counts as touched when its hash changed across the
/// command (`git_pre` → `git_post`), or — without a pre observation — when it
/// no longer matches the last post-edit hash.
pub fn is_discarded(
    last_post_edit: Option<&str>,
    git_pre: Option<&str>,
    git_post: &str,
    has_active: bool,
) -> bool {
    if !has_active || git_post == crate::hash::UNREADABLE {
        return false;
    }
    let touched = match git_pre {
        Some(pre) => pre != git_post,
        None => last_post_edit != Some(git_post),
    };
    touched && last_post_edit != Some(git_post)
}

/// A dead end on the file that has not been reapplied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenDeadEnd {
    pub id: i64,
    /// `post_hash` of each edit grouped in the dead end.
    pub post_hashes: Vec<String>,
    /// When the dead end was recorded. An observation older than this
    /// describes a state that predates the revert and cannot witness the
    /// change coming back.
    pub resolved_ms: i64,
}

/// A new observation of a file's content, with the ordering and provenance
/// needed to judge what it proves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Observation<'a> {
    pub hash: &'a str,
    pub source: VersionSource,
    /// Hook timestamp of the producing event (logical order, not ingestion
    /// order).
    pub ts_ms: i64,
}

/// Whether an observation from `source` describes the file *after* its
/// event's effect, and so can witness a change coming back.
///
/// `git_pre`, `pre_edit` and `original` are all snapshots taken *before* a
/// command or edit runs. The `git_pre` row written for the very command that
/// discards a change carries, by construction, the discarded content: if a
/// spooled `PreToolUse` event is ingested after the turn-end scan that opened
/// the dead end, treating that row as evidence marks the dead end reapplied
/// and deletes `[REVERTED_EDITS]` from the capsule. Observed in the v0.1 benchmark;
/// see `f5c_a_late_git_pre_row_does_not_resurrect_a_dead_end`.
pub fn is_settled_source(source: VersionSource) -> bool {
    matches!(
        source,
        VersionSource::PostEdit | VersionSource::GitPost | VersionSource::TurnScan
    )
}

/// Dead ends reapplied by a new observation (§13.4): the file returned to a
/// hash produced by one of the dead end's edits.
///
/// Two guards keep a stale observation from resurrecting a change that is
/// really gone: the observation must describe a settled state
/// ([`is_settled_source`]), and it must not predate the dead end itself.
pub fn reapplied(obs: &Observation<'_>, open: &[OpenDeadEnd]) -> Vec<i64> {
    if !is_settled_source(obs.source) || !crate::hash::is_content(obs.hash) {
        return Vec::new();
    }
    open.iter()
        .filter(|d| obs.ts_ms >= d.resolved_ms)
        .filter(|d| d.post_hashes.iter().any(|h| h == obs.hash))
        .map(|d| d.id)
        .collect()
}

/// Mechanism for a revert, from the observation that produced the new hash.
pub fn mechanism_for(source: VersionSource, tool_name: Option<&str>) -> crate::model::Mechanism {
    use crate::model::Mechanism;
    match source {
        VersionSource::GitPost => Mechanism::GitCommand,
        VersionSource::PostEdit if tool_name == Some("Write") => Mechanism::Rewrite,
        VersionSource::PostEdit => Mechanism::InverseEdit,
        _ => Mechanism::External,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use VersionSource::*;

    fn v(id: i64, hash: &str, source: VersionSource, event_id: i64) -> Version {
        Version {
            id,
            hash: hash.into(),
            source,
            event_id,
            // Tests that do not care about ordering use the event id as the
            // clock, which keeps logical and ingestion order the same.
            ts_ms: event_id,
        }
    }

    #[test]
    fn inverse_edit_reverts_previous() {
        // Edit A→B (event 10), then Edit B→A (event 20).
        let hist = vec![
            v(1, "A", PreEdit, 10),
            v(2, "B", PostEdit, 10),
            v(3, "B", PreEdit, 20),
        ];
        let active = vec![ActiveEdit {
            id: 1,
            event_id: 10,
            post_hash: "B".into(),
        }];
        assert_eq!(detect_revert(&hist, "A", &active), vec![1]);
        // Same hash as the latest version: nothing.
        assert!(detect_revert(&hist, "B", &active).is_empty());
        // Unknown hash: nothing.
        assert!(detect_revert(&hist, "C", &active).is_empty());
    }

    #[test]
    fn repeated_observation_does_not_revert_the_restoring_edit() {
        // A→B (e10), B→A (e20, restores). Later pre_edit of A again.
        let hist = vec![
            v(1, "A", PreEdit, 10),
            v(2, "B", PostEdit, 10),
            v(3, "B", PreEdit, 20),
            v(4, "A", PostEdit, 20),
        ];
        let active = vec![ActiveEdit {
            id: 2,
            event_id: 20,
            post_hash: "A".into(),
        }];
        assert!(detect_revert(&hist, "A", &active).is_empty());
    }

    #[test]
    fn only_edits_after_return_point() {
        // A→B (e10) … back to A externally … A→C (e30) … back to A.
        let hist = vec![
            v(1, "A", PreEdit, 10),
            v(2, "B", PostEdit, 10),
            v(3, "A", TurnScan, 15),
            v(4, "A", PreEdit, 30),
            v(5, "C", PostEdit, 30),
        ];
        let active = vec![
            ActiveEdit {
                id: 1,
                event_id: 10,
                post_hash: "B".into(),
            },
            ActiveEdit {
                id: 3,
                event_id: 30,
                post_hash: "C".into(),
            },
        ];
        assert_eq!(detect_revert(&hist, "A", &active), vec![3]);
    }

    #[test]
    fn discard_rules() {
        assert!(is_discarded(Some("B"), Some("B"), "A", true));
        assert!(!is_discarded(Some("B"), Some("B"), "B", true));
        // Changed outside the agent before git ran, but git did not touch it.
        assert!(!is_discarded(Some("B"), Some("C"), "C", true));
        assert!(is_discarded(Some("B"), None, "A", true));
        assert!(!is_discarded(Some("B"), Some("B"), "A", false));
    }

    fn obs(hash: &str, source: VersionSource, ts_ms: i64) -> Observation<'_> {
        Observation {
            hash,
            source,
            ts_ms,
        }
    }

    #[test]
    fn reapplication() {
        let open = vec![OpenDeadEnd {
            id: 7,
            post_hashes: vec!["B".into()],
            resolved_ms: 100,
        }];
        assert_eq!(reapplied(&obs("B", PostEdit, 200), &open), vec![7]);
        assert!(reapplied(&obs("C", PostEdit, 200), &open).is_empty());
    }

    #[test]
    fn a_pre_state_observation_never_reapplies() {
        let open = vec![OpenDeadEnd {
            id: 7,
            post_hashes: vec!["B".into()],
            resolved_ms: 100,
        }];
        // The `git_pre` snapshot of the command that discarded the change
        // carries the discarded content by construction.
        assert!(reapplied(&obs("B", GitPre, 200), &open).is_empty());
        assert!(reapplied(&obs("B", PreEdit, 200), &open).is_empty());
        assert!(reapplied(&obs("B", Original, 200), &open).is_empty());
        // Settled sources still count.
        assert_eq!(reapplied(&obs("B", GitPost, 200), &open), vec![7]);
        assert_eq!(reapplied(&obs("B", TurnScan, 200), &open), vec![7]);
    }

    #[test]
    fn an_observation_older_than_the_dead_end_never_reapplies() {
        let open = vec![OpenDeadEnd {
            id: 7,
            post_hashes: vec!["B".into()],
            resolved_ms: 100,
        }];
        assert!(reapplied(&obs("B", TurnScan, 99), &open).is_empty());
        assert_eq!(reapplied(&obs("B", TurnScan, 100), &open), vec![7]);
    }

    /// Only real content can show a change coming back: `absent` and
    /// `unreadable` are sentinels, and two of either compare equal without
    /// meaning the same bytes are on disk.
    #[test]
    fn a_sentinel_hash_proves_nothing() {
        let open = vec![OpenDeadEnd {
            id: 7,
            post_hashes: vec![crate::hash::UNREADABLE.to_string()],
            resolved_ms: 100,
        }];
        assert!(reapplied(&obs(crate::hash::UNREADABLE, TurnScan, 200), &open).is_empty());
        // Nor does a file that is simply gone.
        let open = vec![OpenDeadEnd {
            id: 8,
            post_hashes: vec![crate::hash::ABSENT.to_string()],
            resolved_ms: 100,
        }];
        assert!(reapplied(&obs(crate::hash::ABSENT, TurnScan, 200), &open).is_empty());
    }
}

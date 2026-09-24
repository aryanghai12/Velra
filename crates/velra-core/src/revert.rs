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
///
/// Only a state that equality can identify is returned to: a digest of the
/// bytes, or `absent` -- the path not existing is one state, and a file the
/// session created and that is gone again has returned to it. `unreadable`
/// and a large file's size-and-mtime fingerprint (`hash::is_digest`) are
/// equal without the content being the same, and prove no return.
pub fn detect_revert(history: &[Version], new_hash: &str, active: &[ActiveEdit]) -> Vec<i64> {
    let identifies = crate::hash::is_digest(new_hash) || new_hash == crate::hash::ABSENT;
    if !identifies || active.is_empty() {
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
/// no longer matches the last post-edit hash. (An `unreadable` pre
/// observation therefore decides nothing: the result is the post hash against
/// the last post-edit hash, as without one.)
///
/// The caller decides first whether the command could have reached the file
/// at all (`shell::reaches`); this only reads the hashes.
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
/// ([`is_settled_source`]), and it must not predate the dead end itself. And
/// only a digest of the bytes can show the content is back
/// ([`crate::hash::is_digest`]).
///
/// What this establishes is a fact about the file -- the discarded content is
/// in it again -- not about who put it there: equal bytes are the same state
/// whether an edit, a `git checkout`, a copy or a generator wrote them. The
/// dead end stops being an open route because the route is live again; no
/// conclusion about the cause is recorded.
pub fn reapplied(obs: &Observation<'_>, open: &[OpenDeadEnd]) -> Vec<i64> {
    if !is_settled_source(obs.source) || !crate::hash::is_digest(obs.hash) {
        return Vec::new();
    }
    open.iter()
        .filter(|d| obs.ts_ms >= d.resolved_ms)
        .filter(|d| d.post_hashes.iter().any(|h| h == obs.hash))
        .map(|d| d.id)
        .collect()
}

/// Mechanism for a revert, from the observation that produced the new hash.
///
/// A `git_post` observation is taken for every file the session edited, not
/// only those the command names; `git_reached` says whether one of the line's
/// restore-family calls could have changed this file (`shell::reaches`). A
/// file it could not reach, or might not have, changed by some other means
/// the line does not identify -- another subcommand, a formatter, the user --
/// and is recorded with the mechanism that claims no cause.
pub fn mechanism_for(
    source: VersionSource,
    tool_name: Option<&str>,
    git_reached: bool,
) -> crate::model::Mechanism {
    use crate::model::Mechanism;
    match source {
        VersionSource::GitPost if git_reached => Mechanism::GitCommand,
        VersionSource::PostEdit if tool_name == Some("Write") => Mechanism::Rewrite,
        VersionSource::PostEdit => Mechanism::InverseEdit,
        _ => Mechanism::External,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use VersionSource::*;

    /// A digest standing for the content named `name`: revert and
    /// reapplication only ever compare digests of bytes.
    fn d(name: &str) -> String {
        crate::hash::content_hash(name.as_bytes())
    }

    fn v(id: i64, hash: &str, source: VersionSource, event_id: i64) -> Version {
        Version {
            id,
            hash: d(hash),
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
            post_hash: d("B"),
        }];
        assert_eq!(detect_revert(&hist, &d("A"), &active), vec![1]);
        // Same hash as the latest version: nothing.
        assert!(detect_revert(&hist, &d("B"), &active).is_empty());
        // Unknown hash: nothing.
        assert!(detect_revert(&hist, &d("C"), &active).is_empty());
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
            post_hash: d("A"),
        }];
        assert!(detect_revert(&hist, &d("A"), &active).is_empty());
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
                post_hash: d("B"),
            },
            ActiveEdit {
                id: 3,
                event_id: 30,
                post_hash: d("C"),
            },
        ];
        assert_eq!(detect_revert(&hist, &d("A"), &active), vec![3]);
    }

    #[test]
    fn discard_rules() {
        assert!(is_discarded(Some("B"), Some("B"), "A", true));
        // An unreadable pre observation decides nothing: the post hash
        // against the last post-edit hash does.
        assert!(!is_discarded(
            Some("B"),
            Some(crate::hash::UNREADABLE),
            "B",
            true
        ));
        assert!(is_discarded(
            Some("B"),
            Some(crate::hash::UNREADABLE),
            "A",
            true
        ));
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
            post_hashes: vec![d("B")],
            resolved_ms: 100,
        }];
        assert_eq!(reapplied(&obs(&d("B"), PostEdit, 200), &open), vec![7]);
        assert!(reapplied(&obs(&d("C"), PostEdit, 200), &open).is_empty());
    }

    #[test]
    fn a_pre_state_observation_never_reapplies() {
        let open = vec![OpenDeadEnd {
            id: 7,
            post_hashes: vec![d("B")],
            resolved_ms: 100,
        }];
        // The `git_pre` snapshot of the command that discarded the change
        // carries the discarded content by construction.
        assert!(reapplied(&obs(&d("B"), GitPre, 200), &open).is_empty());
        assert!(reapplied(&obs(&d("B"), PreEdit, 200), &open).is_empty());
        assert!(reapplied(&obs(&d("B"), Original, 200), &open).is_empty());
        // Settled sources still count.
        assert_eq!(reapplied(&obs(&d("B"), GitPost, 200), &open), vec![7]);
        assert_eq!(reapplied(&obs(&d("B"), TurnScan, 200), &open), vec![7]);
    }

    #[test]
    fn an_observation_older_than_the_dead_end_never_reapplies() {
        let open = vec![OpenDeadEnd {
            id: 7,
            post_hashes: vec![d("B")],
            resolved_ms: 100,
        }];
        assert!(reapplied(&obs(&d("B"), TurnScan, 99), &open).is_empty());
        assert_eq!(reapplied(&obs(&d("B"), TurnScan, 100), &open), vec![7]);
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

    /// A large file's fingerprint is size and mtime, and `large:{n}:0` is the
    /// same for every file of that size whose mtime was not read. Equal
    /// fingerprints make neither a revert nor a reapplication.
    #[test]
    fn a_large_file_fingerprint_proves_no_return() {
        let fp = "large:5000000:0";
        let hist = vec![
            Version {
                id: 1,
                hash: fp.into(),
                source: PreEdit,
                event_id: 10,
                ts_ms: 10,
            },
            Version {
                id: 2,
                hash: "large:5000100:0".into(),
                source: PostEdit,
                event_id: 10,
                ts_ms: 10,
            },
        ];
        let active = vec![ActiveEdit {
            id: 1,
            event_id: 10,
            post_hash: "large:5000100:0".into(),
        }];
        assert!(detect_revert(&hist, fp, &active).is_empty());
        let open = vec![OpenDeadEnd {
            id: 7,
            post_hashes: vec![fp.to_string()],
            resolved_ms: 100,
        }];
        assert!(reapplied(&obs(fp, PostEdit, 200), &open).is_empty());
        assert!(reapplied(&obs(fp, TurnScan, 200), &open).is_empty());
    }

    /// A file the session created and that is gone again has returned to the
    /// state before the creation: `absent` is one state. It never shows the
    /// content coming back.
    #[test]
    fn deleting_a_created_file_reverts_the_creation() {
        let hist = vec![
            Version {
                id: 1,
                hash: crate::hash::ABSENT.into(),
                source: PreEdit,
                event_id: 10,
                ts_ms: 10,
            },
            v(2, "B", PostEdit, 10),
        ];
        let active = vec![ActiveEdit {
            id: 1,
            event_id: 10,
            post_hash: d("B"),
        }];
        assert_eq!(detect_revert(&hist, crate::hash::ABSENT, &active), vec![1]);
        // Unreadable is not a state it can return to.
        assert!(detect_revert(&hist, crate::hash::UNREADABLE, &active).is_empty());
    }

    #[test]
    fn a_git_observation_names_git_only_for_files_the_call_reached() {
        use crate::model::Mechanism;
        assert_eq!(
            mechanism_for(GitPost, Some("Bash"), true),
            Mechanism::GitCommand
        );
        assert_eq!(
            mechanism_for(GitPost, Some("Bash"), false),
            Mechanism::External
        );
        assert_eq!(
            mechanism_for(PostEdit, Some("Edit"), false),
            Mechanism::InverseEdit
        );
        assert_eq!(
            mechanism_for(PostEdit, Some("Write"), false),
            Mechanism::Rewrite
        );
        assert_eq!(mechanism_for(TurnScan, None, true), Mechanism::External);
        assert_eq!(
            mechanism_for(GitPre, Some("Bash"), true),
            Mechanism::External
        );
    }
}

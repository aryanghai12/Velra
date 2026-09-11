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
}

/// Dead ends reapplied by a new observation (§13.4): the file returned to a
/// hash produced by one of the dead end's edits.
pub fn reapplied(new_hash: &str, open: &[OpenDeadEnd]) -> Vec<i64> {
    open.iter()
        .filter(|d| d.post_hashes.iter().any(|h| h == new_hash))
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

    #[test]
    fn reapplication() {
        let open = vec![OpenDeadEnd {
            id: 7,
            post_hashes: vec!["B".into()],
        }];
        assert_eq!(reapplied("B", &open), vec![7]);
        assert!(reapplied("C", &open).is_empty());
    }
}

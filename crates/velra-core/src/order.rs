//! Logical event order within a session.
//!
//! # What the ledger knows about time
//!
//! An event row carries two orderings, and neither is chronology on its own:
//!
//! * `id` is **ingestion** order. A hook that reaches the database appends its
//!   event while it runs, so among those events `id` order *is* the order they
//!   happened in -- Claude Code runs a session's hooks as the session
//!   proceeds, and a later hook starts after an earlier one appended. A hook
//!   that cannot reach the database writes its event to the spool instead,
//!   and the event gets its `id` whenever a reducer ingests the spool: after
//!   rows that happened later.
//! * `ts_ms` is the hook's wall clock when it started. It survives the spool,
//!   but it is only a clock: it can repeat within a millisecond, run backwards
//!   after a correction, or jump.
//!
//! The hook marks an event it spools (`Payload::spooled`, set by
//! `crate::spool::write`). That is the missing fact that makes the two usable
//! together.
//!
//! # The order
//!
//! * Events appended directly keep their `id` order. Their clock is never
//!   used to reorder them, so a clock that runs backwards cannot reverse two
//!   things the ledger saw happen in sequence.
//! * A spooled event is placed after the last directly appended event that
//!   was ingested before it and whose clock is not later than its own; the
//!   spooled events placed at the same point are ordered by `(ts_ms, id)`.
//!   A direct event ingested after a spooled one always comes after it: the
//!   spooled event had happened before it was ingested.
//!
//! Equal timestamps are ordered by `id`. Everything is deterministic in the
//! rows themselves, so replaying the same ledger gives the same order.
//!
//! What this cannot recover: where a spooled event goes when the clock was
//! wrong *while it was spooled*. There is no occurrence sequence independent
//! of the wall clock -- Claude Code sends none, and the hook that spools is
//! precisely the one that could not reach the shared counter the database
//! would be. Events without the mark (spooled by builds before it existed)
//! are indistinguishable from direct ones and keep their `id` order.

/// What the order is computed from: one event of a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Key {
    pub id: i64,
    pub ts_ms: i64,
    /// Written to the spool rather than appended directly.
    pub spooled: bool,
}

/// Indices into `events` in logical order (see the module docs). `events`
/// may be given in any order; ids are expected to be distinct.
pub fn logical_order(events: &[Key]) -> Vec<usize> {
    let mut by_id: Vec<usize> = (0..events.len()).collect();
    by_id.sort_by_key(|&i| events[i].id);
    let direct: Vec<usize> = by_id
        .iter()
        .copied()
        .filter(|&i| !events[i].spooled)
        .collect();
    // groups[k]: spooled events placed after direct[k - 1] (k = 0: before any).
    let mut groups: Vec<Vec<usize>> = vec![Vec::new(); direct.len() + 1];
    for &s in by_id.iter().filter(|&&i| events[i].spooled) {
        let ev = events[s];
        // Direct events are in id order: those ingested before `s` are a prefix.
        let ingested_before = direct.partition_point(|&d| events[d].id < ev.id);
        let anchor = direct[..ingested_before]
            .iter()
            .rposition(|&d| events[d].ts_ms <= ev.ts_ms)
            .map_or(0, |k| k + 1);
        groups[anchor].push(s);
    }
    let mut out = Vec::with_capacity(events.len());
    for (k, group) in groups.iter_mut().enumerate() {
        if k > 0 {
            out.push(direct[k - 1]);
        }
        group.sort_by_key(|&i| (events[i].ts_ms, events[i].id));
        out.extend(group.iter().copied());
    }
    out
}

/// What [`logical_order`] needs to know about a session's events, taken in
/// ingestion (id) order, to say whether the next one is logically last: the
/// clock of the last directly appended event, and the latest clock among
/// the spooled events ingested after it.
///
/// A spooled event is placed after the last direct event whose clock is not
/// later than its own, and among the spooled events placed there by
/// `(ts_ms, id)`. So it is last exactly when that last direct event is not
/// later than it and no spooled event since is later than it -- a check of
/// two numbers, where computing the order is a scan and a sort of the whole
/// session. Reducing a spooled tail with the full computation was
/// quadratic: 28 ms an event on a 20,000-event session (DECISIONS D119).
/// `the_tail_agrees_with_the_full_order` holds the two to the same answer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tail {
    pub last_direct_ts: Option<i64>,
    pub max_spooled_ts_since: Option<i64>,
}

impl Tail {
    /// Whether `next`, ingested after every event observed so far, is last
    /// in the logical order of those events and itself.
    pub fn is_last(&self, next: Key) -> bool {
        !next.spooled
            || (self.last_direct_ts.is_none_or(|t| t <= next.ts_ms)
                && self.max_spooled_ts_since.is_none_or(|t| t <= next.ts_ms))
    }

    /// Takes `ev`, the next event in ingestion order, into account.
    pub fn observe(&mut self, ev: Key) {
        if ev.spooled {
            self.max_spooled_ts_since = Some(
                self.max_spooled_ts_since
                    .map_or(ev.ts_ms, |t| t.max(ev.ts_ms)),
            );
        } else {
            self.last_direct_ts = Some(ev.ts_ms);
            self.max_spooled_ts_since = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Property: over random sessions -- clocks that repeat, run backwards
    /// and jump, any mix of direct and spooled -- the incremental [`Tail`]
    /// answers "is the next event last?" exactly as [`logical_order`] does,
    /// at every prefix.
    #[test]
    fn the_tail_agrees_with_the_full_order() {
        let mut seed: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _case in 0..3_000 {
            let len = 1 + (next() % 24) as usize;
            let spread = 1 + (next() % 12) as i64;
            let mut keys: Vec<Key> = Vec::with_capacity(len);
            let mut tail = Tail::default();
            for i in 0..len {
                let k = Key {
                    id: i as i64 + 1,
                    ts_ms: (next() % spread as u64) as i64,
                    spooled: next() % 3 != 0,
                };
                keys.push(k);
                let full = logical_order(&keys).last().map(|&j| keys[j].id) == Some(k.id);
                assert_eq!(tail.is_last(k), full, "{keys:?}");
                tail.observe(k);
            }
        }
    }

    fn d(id: i64, ts: i64) -> Key {
        Key {
            id,
            ts_ms: ts,
            spooled: false,
        }
    }
    fn s(id: i64, ts: i64) -> Key {
        Key {
            id,
            ts_ms: ts,
            spooled: true,
        }
    }
    fn ids(events: &[Key]) -> Vec<i64> {
        logical_order(events)
            .iter()
            .map(|&i| events[i].id)
            .collect()
    }

    #[test]
    fn a_spooled_event_goes_back_to_when_it_happened() {
        // A (ts 10) was spooled and ingested after B (20) and C (30).
        assert_eq!(ids(&[d(1, 20), d(2, 30), s(3, 10)]), vec![3, 1, 2]);
        // Between B and C.
        assert_eq!(ids(&[d(1, 20), d(2, 30), s(3, 25)]), vec![1, 3, 2]);
        // Two spooled events, ingested in the wrong order themselves.
        assert_eq!(ids(&[d(1, 30), s(2, 20), s(3, 10)]), vec![3, 2, 1]);
    }

    #[test]
    fn direct_events_keep_ingestion_order_whatever_the_clock_says() {
        // The clock ran backwards between 1 and 2, and jumped far ahead at 3.
        assert_eq!(
            ids(&[d(1, 100), d(2, 50), d(3, 9_999_999), d(4, 60)]),
            vec![1, 2, 3, 4]
        );
        // The anchor is the last direct event whose clock is not later than
        // the spooled one's: a future-dated event in between does not drag
        // everything after it along.
        assert_eq!(
            ids(&[d(1, 100), d(2, 9_999_999), d(3, 110), s(4, 120)]),
            vec![1, 2, 3, 4]
        );
        assert_eq!(
            ids(&[d(1, 100), d(2, 9_999_999), d(3, 130), s(4, 120)]),
            vec![1, 4, 2, 3]
        );
    }

    #[test]
    fn a_direct_event_ingested_after_a_spooled_one_comes_after_it() {
        // Spooled at ts 500 (clock wrong), ingested before direct event 3.
        assert_eq!(ids(&[d(1, 100), s(2, 500), d(3, 200)]), vec![1, 2, 3]);
    }

    #[test]
    fn equal_timestamps_are_ordered_by_id() {
        assert_eq!(
            ids(&[d(1, 10), d(2, 10), s(3, 10), s(4, 10)]),
            vec![1, 2, 3, 4]
        );
        assert_eq!(ids(&[d(1, 10), s(4, 5), s(3, 5)]), vec![3, 4, 1]);
    }

    #[test]
    fn input_order_does_not_matter() {
        let evs = [d(1, 20), s(5, 12), d(2, 30), s(4, 25), d(3, 40), s(6, 1)];
        let want = ids(&evs);
        let mut rev = evs;
        rev.reverse();
        assert_eq!(ids(&rev), want);
        assert_eq!(want, vec![6, 5, 1, 4, 2, 3]);
    }

    proptest::proptest! {
        /// Direct events never change relative order, every event appears
        /// once, and the order is the same however the input is permuted.
        #[test]
        fn the_order_is_a_deterministic_permutation(
            raw in proptest::collection::vec((0i64..50, proptest::bool::ANY), 0..30),
            rot in 0usize..30,
        ) {
            let evs: Vec<Key> = raw.iter().enumerate()
                .map(|(i, &(ts, sp))| Key { id: i as i64 + 1, ts_ms: ts, spooled: sp })
                .collect();
            let got = ids(&evs);
            let mut sorted = got.clone();
            sorted.sort_unstable();
            proptest::prop_assert_eq!(sorted, (1..=evs.len() as i64).collect::<Vec<_>>());
            let direct: Vec<i64> = got.iter().copied().filter(|id| !evs[*id as usize - 1].spooled).collect();
            let mut direct_sorted = direct.clone();
            direct_sorted.sort_unstable();
            proptest::prop_assert_eq!(direct, direct_sorted);
            let mut rotated = evs.clone();
            if !rotated.is_empty() {
                let r = rot % rotated.len();
                rotated.rotate_left(r);
            }
            proptest::prop_assert_eq!(ids(&rotated), got);
        }
    }
}

//! Time formatting without heavyweight dependencies on the hot path.

use std::time::{SystemTime, UNIX_EPOCH};

/// Current Unix epoch milliseconds.
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// Days since 1970-01-01 → (year, month, day). Howard Hinnant's algorithm.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn parts(ms: i64, offset_secs: i64) -> (i64, u32, u32, i64, i64, i64) {
    let secs = ms.div_euclid(1000) + offset_secs;
    let (y, mo, d) = civil_from_days(secs.div_euclid(86_400));
    let sod = secs.rem_euclid(86_400);
    (y, mo, d, sod / 3600, (sod % 3600) / 60, sod % 60)
}

/// RFC 3339 UTC with second precision, e.g. `2026-09-12T10:04:05Z`.
pub fn rfc3339_utc(ms: i64) -> String {
    let (y, mo, d, h, mi, s) = parts(ms, 0);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

/// Compact UTC stamp for backup names, e.g. `20260912T100405Z`.
pub fn compact_utc(ms: i64) -> String {
    let (y, mo, d, h, mi, s) = parts(ms, 0);
    format!("{y:04}{mo:02}{d:02}T{h:02}{mi:02}{s:02}Z")
}

/// `HH:MM` at the given UTC offset.
pub fn hh_mm(ms: i64, offset_secs: i32) -> String {
    let (_, _, _, h, mi, _) = parts(ms, i64::from(offset_secs));
    format!("{h:02}:{mi:02}")
}

/// Local UTC offset in seconds at instant `ms`.
///
/// `TZ=UTC` (and equivalents) forces 0 on every platform so golden tests are
/// byte-identical across operating systems.
pub fn local_offset_secs(ms: i64) -> i32 {
    if let Ok(tz) = std::env::var("TZ") {
        if matches!(
            tz.as_str(),
            "UTC" | "UTC0" | "Etc/UTC" | "GMT" | "Z" | ":UTC" | "Etc/GMT"
        ) {
            return 0;
        }
    }
    use chrono::{Offset, TimeZone};
    chrono::Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|dt| dt.offset().fix().local_minus_utc())
        .unwrap_or(0)
}

/// Human age such as `3s`, `5m`, `2h`, `4d`.
pub fn human_age(delta_ms: i64) -> String {
    let s = delta_ms.max(0) / 1000;
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m", s / 60),
        3600..=86_399 => format!("{}h", s / 3600),
        _ => format!("{}d", s / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats() {
        // 2026-09-12T10:04:05Z
        let ms = 1_789_207_445_000;
        assert_eq!(rfc3339_utc(ms), "2026-09-12T10:04:05Z");
        assert_eq!(compact_utc(ms), "20260912T100405Z");
        assert_eq!(hh_mm(ms, 0), "10:04");
        assert_eq!(hh_mm(ms, 5 * 3600 + 1800), "15:34");
        assert_eq!(hh_mm(ms, -11 * 3600), "23:04");
        assert_eq!(rfc3339_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339_utc(951_782_400_000), "2000-02-29T00:00:00Z");
    }

    #[test]
    fn ages() {
        assert_eq!(human_age(5_000), "5s");
        assert_eq!(human_age(120_000), "2m");
        assert_eq!(human_age(7_200_000), "2h");
        assert_eq!(human_age(172_800_000), "2d");
    }
}

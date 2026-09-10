//! How a moment is written into the database.
//!
//! One canonical form, used for every timestamp column.
//!
//! The requirement that shapes it: the listing sorts newest-first, and it must
//! do that in SQL rather than by loading every row. SQLite compares text
//! lexicographically, so the encoding has to make lexicographic order and
//! chronological order the same thing. Two properties are needed for that, and
//! ordinary RFC 3339 has neither.
//!
//! **Always UTC.** `2026-01-01T00:00:00+05:00` and `2025-12-31T19:00:00Z` are
//! the same instant and sort a year apart as text.
//!
//! **Fixed width.** RFC 3339 lets subseconds be absent or any length, so
//! `12:00:00Z` sorts *after* `12:00:00.5Z` — `Z` is above `.` in ASCII — even
//! though it is the earlier instant. Padding subseconds to a constant nine
//! digits removes the case entirely, and nine digits is the full precision of
//! the timestamps being stored, so nothing is rounded away on the way in.

use time::format_description::BorrowedFormatItem;
use time::macros::format_description;
use time::{OffsetDateTime, UtcOffset};

/// Fixed-width UTC, to the nanosecond: `2026-09-08T12:00:00.000000000Z`.
const CANONICAL: &[BorrowedFormatItem<'_>] =
    format_description!("[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:9]Z");

/// Encode a moment for storage.
///
/// Converts to UTC first, so the same instant always produces the same text no
/// matter what offset it arrived in.
pub fn encode(at: OffsetDateTime) -> Result<String, time::error::Format> {
    at.to_offset(UtcOffset::UTC).format(&CANONICAL)
}

/// Read a moment back.
pub fn decode(text: &str) -> Result<OffsetDateTime, time::error::Parse> {
    time::PrimitiveDateTime::parse(text, &CANONICAL).map(|t| t.assume_utc())
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    #[test]
    fn a_moment_survives_the_round_trip_exactly() {
        for moment in [
            datetime!(2026-09-08 12:00:00 UTC),
            datetime!(2026-09-08 12:00:00.123456789 UTC),
            datetime!(1970-01-01 00:00:00 UTC),
        ] {
            let text = encode(moment).expect("encode");
            assert_eq!(decode(&text).expect("decode"), moment, "via {text:?}");
        }
    }

    #[test]
    fn an_offset_is_normalized_to_utc() {
        // The same instant, written two ways. As text they must be identical,
        // or they sort a year apart.
        let east = datetime!(2026-01-01 00:00:00 +05:00);
        let utc = datetime!(2025-12-31 19:00:00 UTC);
        assert_eq!(east, utc, "the fixture is wrong, these are one instant");
        assert_eq!(encode(east).unwrap(), encode(utc).unwrap());
    }

    #[test]
    fn text_order_is_time_order() {
        // The property the listing's ORDER BY depends on.
        let mut moments = [
            datetime!(2026-09-08 12:00:01 UTC),
            datetime!(2026-09-08 12:00:00.5 UTC),
            datetime!(2026-09-08 12:00:00 UTC),
            datetime!(2025-01-01 00:00:00 UTC),
            datetime!(2026-09-08 12:00:00.000000001 UTC),
        ];
        moments.sort();

        let encoded: Vec<String> = moments.iter().map(|m| encode(*m).unwrap()).collect();
        let mut sorted_as_text = encoded.clone();
        sorted_as_text.sort();

        assert_eq!(encoded, sorted_as_text);
    }

    #[test]
    fn subsecond_zero_sorts_before_subsecond_half() {
        // Written out because this is the exact case plain RFC 3339 gets wrong:
        // "12:00:00Z" > "12:00:00.5Z" as text, and it is the earlier instant.
        let whole = encode(datetime!(2026-09-08 12:00:00 UTC)).unwrap();
        let half = encode(datetime!(2026-09-08 12:00:00.5 UTC)).unwrap();
        assert!(whole < half, "{whole:?} should sort before {half:?}");
    }

    #[test]
    fn every_timestamp_is_the_same_width() {
        // Fixed width is the whole mechanism; a variable-length encoding would
        // reintroduce the bug above.
        let widths: std::collections::BTreeSet<usize> = [
            datetime!(2026-09-08 12:00:00 UTC),
            datetime!(2026-09-08 12:00:00.1 UTC),
            datetime!(2026-09-08 12:00:00.123456789 UTC),
            datetime!(0001-01-01 00:00:00 UTC),
        ]
        .into_iter()
        .map(|m| encode(m).unwrap().len())
        .collect();

        assert_eq!(widths.len(), 1, "widths differ: {widths:?}");
    }

    #[test]
    fn nothing_that_is_not_the_canonical_form_is_accepted() {
        // A value written by some other tool, or a corrupted cell, is reported
        // rather than half-understood.
        for bad in [
            "2026-09-08T12:00:00Z",           // no subseconds
            "2026-09-08T12:00:00.123Z",       // wrong precision
            "2026-09-08 12:00:00.000000000Z", // space instead of T
            "not a time",
            "",
        ] {
            assert!(decode(bad).is_err(), "{bad:?} should not have parsed");
        }
    }
}

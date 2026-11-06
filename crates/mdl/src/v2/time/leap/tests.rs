use super::*;
use crate::v2::time::Timestamp;

fn unix(text: &str) -> i128 {
    text.parse::<Timestamp>().unwrap().unix_nanos_wide()
}

#[test]
fn the_table_starts_each_offset_at_the_utc_day_it_names() {
    let days = [
        "1972-01-01",
        "1972-07-01",
        "1973-01-01",
        "1974-01-01",
        "1975-01-01",
        "1976-01-01",
        "1977-01-01",
        "1978-01-01",
        "1979-01-01",
        "1980-01-01",
        "1981-07-01",
        "1982-07-01",
        "1983-07-01",
        "1985-07-01",
        "1988-01-01",
        "1990-01-01",
        "1991-01-01",
        "1992-07-01",
        "1993-07-01",
        "1994-07-01",
        "1996-01-01",
        "1997-07-01",
        "1999-01-01",
        "2006-01-01",
        "2009-01-01",
        "2012-07-01",
        "2015-07-01",
        "2017-01-01",
    ];
    assert_eq!(LEAP_SECONDS.len(), days.len());
    for ((start, offset), (day, expected)) in LEAP_SECONDS.iter().zip(days.iter().zip(10..)) {
        assert_eq!(
            i128::from(*start) * NANOS,
            unix(&format!("{day}T00:00:00Z")),
            "{day}"
        );
        assert_eq!(*offset, expected, "{day}");
    }
}

#[test]
fn tai_runs_ahead_of_utc_by_the_leap_seconds() {
    // Since 2017, TAI − UTC is 37 s.
    let utc = unix("2026-06-01T12:00:00Z");
    assert_eq!(to_utc(TimeScale::Tai, utc + 37 * NANOS), utc);
    assert_eq!(from_utc(TimeScale::Tai, utc), utc + 37 * NANOS);
    // And GPS 18 s.
    assert_eq!(to_utc(TimeScale::Gps, utc + 18 * NANOS), utc);
    assert_eq!(from_utc(TimeScale::Gps, utc), utc + 18 * NANOS);
    assert_eq!(to_utc(TimeScale::Utc, utc), utc);
}

#[test]
fn a_label_inside_a_leap_second_is_held_to_the_second_before_it_ends() {
    let leap = unix("2017-01-01T00:00:00Z");
    // 23:59:59.999999999 UTC, just before the leap second, is TAI
    // 00:00:35.999999999 (36 s ahead).
    assert_eq!(to_utc(TimeScale::Tai, leap + 36 * NANOS - 1), leap - 1);
    // The leap second, 23:59:60 UTC, is TAI 00:00:36 up to 00:00:37: held.
    assert_eq!(to_utc(TimeScale::Tai, leap + 36 * NANOS), leap - 1);
    assert_eq!(
        to_utc(TimeScale::Tai, leap + 36 * NANOS + NANOS / 2),
        leap - 1
    );
    // From then on TAI is 37 s ahead.
    assert_eq!(to_utc(TimeScale::Tai, leap + 37 * NANOS), leap);
}

#[test]
fn before_the_table_its_first_offset_applies() {
    // The CCSDS epoch, 1958-01-01 TAI, is 10 s ahead of UTC by this rule.
    let epoch = unix("1958-01-01T00:00:00Z");
    assert_eq!(to_utc(TimeScale::Tai, epoch), epoch - 10 * NANOS);
}

#[test]
fn to_and_from_utc_undo_each_other_outside_leap_seconds() {
    for day in [
        "1975-03-01",
        "1999-12-31",
        "2008-02-29",
        "2017-01-02",
        "2030-01-01",
    ] {
        let utc = unix(&format!("{day}T06:00:00.123456789Z"));
        for scale in [TimeScale::Utc, TimeScale::Tai, TimeScale::Gps] {
            assert_eq!(to_utc(scale, from_utc(scale, utc)), utc, "{day} {scale:?}");
        }
    }
}

#[test]
fn terrestrial_time_is_tai_plus_32_184_seconds() {
    let utc = unix("2026-06-01T12:00:00Z");
    let tt = utc + 37 * NANOS + 32_184_000_000;
    assert_eq!(to_utc(TimeScale::Tt, tt), utc);
    assert_eq!(from_utc(TimeScale::Tt, utc), tt);
}

#[test]
fn the_table_is_known_good_until_the_next_possible_leap_second() {
    assert_eq!(
        i128::from(VALID_UNTIL) * NANOS,
        unix("2027-07-01T00:00:00Z")
    );
    assert!(LEAP_SECONDS.iter().all(|&(start, _)| start < VALID_UNTIL));
    assert_eq!(
        Timestamp::LEAP_SECONDS_VALID_UNTIL,
        "2027-07-01T00:00:00Z".parse().unwrap()
    );
}

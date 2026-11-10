use proptest::prelude::*;

use super::*;

fn instant(text: &str) -> Timestamp {
    text.parse().unwrap()
}

#[test]
fn instants_have_one_spelling() {
    for good in [
        "2000-01-01T00:00:00Z",
        "2026-11-05T23:59:59.5Z",
        "2024-02-29T12:00:00.123456789Z",
        "2024-02-29T12:00:00.000000001Z",
        "0001-01-01T00:00:00Z",
        "9999-12-31T23:59:59.999999999Z",
    ] {
        assert_eq!(instant(good).to_string(), good);
    }
    for bad in [
        "2000-01-01 00:00:00Z",
        "2000-01-01t00:00:00z",
        "2000-01-01T23:59:60Z",
        "2000-01-01T24:00:00Z",
        "2000-01-01T00:00:00.1234567891Z",
        "2000-01-01T00:00:00.50Z",
        "2000-01-01T00:00:00.0Z",
        "2000-01-01T00:00:00.Z",
        "0000-01-01T00:00:00Z",
        "2000-01-01T00:00:00+00:00",
        "2023-02-29T00:00:00Z",
        "2000-13-01T00:00:00Z",
        "2000-01-01",
        "+2000-01-01T00:00:00Z",
        "2000-01-01T00:00:00Ž",
    ] {
        assert!(bad.parse::<Timestamp>().is_err(), "{bad}");
    }
}

#[test]
fn known_instants_have_known_seconds() {
    assert_eq!(instant("1970-01-01T00:00:00Z"), Timestamp::UNIX_EPOCH);
    assert_eq!(instant("0001-01-01T00:00:00Z"), Timestamp::MIN);
    assert_eq!(instant("9999-12-31T23:59:59.999999999Z"), Timestamp::MAX);
    assert_eq!(instant("2000-01-01T00:00:00Z").seconds(), 946_684_800);
    let before_epoch = instant("1969-12-31T23:59:59.999999999Z");
    assert_eq!(
        (before_epoch.seconds(), before_epoch.nanos()),
        (-1, 999_999_999)
    );
    assert_eq!(Timestamp::from_unix_nanos(-1), before_epoch);
}

#[test]
fn construction_stays_in_range() {
    assert_eq!(Timestamp::new(0, 1_000_000_000), None);
    assert_eq!(
        Timestamp::new(Timestamp::MIN.seconds() - 1, 999_999_999),
        None
    );
    assert_eq!(Timestamp::new(Timestamp::MAX.seconds() + 1, 0), None);
    assert_eq!(
        Timestamp::new(Timestamp::MAX.seconds(), 999_999_999),
        Some(Timestamp::MAX)
    );
    for nanos in [i64::MIN, -1, 0, 1, i64::MAX] {
        let t = Timestamp::from_unix_nanos(nanos);
        assert_eq!(Timestamp::new(t.seconds(), t.nanos()), Some(t));
    }
}

#[test]
fn ordering_is_chronological() {
    let mut instants = [
        "2026-06-01T00:00:00.000000001Z",
        "1969-12-31T23:59:59.5Z",
        "2026-06-01T00:00:00Z",
        "1969-12-31T23:59:59Z",
    ]
    .map(instant);
    instants.sort();
    let texts: Vec<String> = instants.iter().map(Timestamp::to_string).collect();
    assert_eq!(
        texts,
        [
            "1969-12-31T23:59:59Z",
            "1969-12-31T23:59:59.5Z",
            "2026-06-01T00:00:00Z",
            "2026-06-01T00:00:00.000000001Z",
        ]
    );
}

fn any_instant() -> impl Strategy<Value = Timestamp> {
    (
        Timestamp::MIN.seconds()..=Timestamp::MAX.seconds(),
        prop_oneof![
            Just(0_u32),
            0_u32..NANOS_PER_SECOND,
            (1_u32..1000).prop_map(|ms| ms * 1_000_000),
        ],
    )
        .prop_map(|(seconds, nanos)| Timestamp::new(seconds, nanos).unwrap())
}

proptest! {
    #[test]
    fn text_round_trips(t in any_instant()) {
        let text = t.to_string();
        prop_assert_eq!(text.parse::<Timestamp>(), Ok(t));
    }

    #[test]
    fn text_matches_chrono(t in any_instant()) {
        let reference = chrono::DateTime::from_timestamp(t.seconds(), t.nanos())
            .unwrap()
            .to_rfc3339_opts(chrono::SecondsFormat::Nanos, true);
        let ours = t.to_string();
        let trimmed = match reference.split_once('.') {
            Some((whole, fraction)) => {
                let digits = fraction.trim_end_matches('Z').trim_end_matches('0');
                if digits.is_empty() {
                    format!("{whole}Z")
                } else {
                    format!("{whole}.{digits}Z")
                }
            }
            None => reference,
        };
        prop_assert_eq!(ours, trimmed);
    }

    #[test]
    fn order_matches_nanoseconds(a in any::<i64>(), b in any::<i64>()) {
        prop_assert_eq!(
            Timestamp::from_unix_nanos(a).cmp(&Timestamp::from_unix_nanos(b)),
            a.cmp(&b)
        );
    }
}

#[test]
fn payload_instants_are_any_rfc3339() {
    let utc = |text: &str| Timestamp::from_rfc3339(text).map(|t| t.to_string());
    for (text, expected) in [
        ("2026-06-01T12:00:00Z", "2026-06-01T12:00:00Z"),
        ("2026-06-01t12:00:00z", "2026-06-01T12:00:00Z"),
        ("2026-06-01 12:00:00Z", "2026-06-01T12:00:00Z"),
        ("2026-06-01T14:00:00.250+02:00", "2026-06-01T12:00:00.25Z"),
        ("2026-06-01T12:00:00-00:00", "2026-06-01T12:00:00Z"),
        (
            "2026-06-01T12:00:00.1234567891234Z",
            "2026-06-01T12:00:00.123456789Z",
        ),
        ("2016-12-31T23:59:60Z", "2016-12-31T23:59:59.999999999Z"),
        ("2016-12-31T23:59:60.5Z", "2016-12-31T23:59:59.999999999Z"),
    ] {
        assert_eq!(utc(text).as_deref(), Ok(expected), "{text}");
    }
    for bad in [
        "2026-06-01T12:00:00",
        "2026-06-01T12:00Z",
        "0000-01-01T00:00:00Z",
        "2026-06-01T12:00:00+2400",
        "yesterday",
    ] {
        assert!(Timestamp::from_rfc3339(bad).is_err(), "{bad}");
    }
}

#[test]
fn wide_nanoseconds_round_trip_within_range() {
    for t in [Timestamp::MIN, Timestamp::UNIX_EPOCH, Timestamp::MAX] {
        assert_eq!(
            Timestamp::from_unix_nanos_wide(t.unix_nanos_wide()),
            Some(t)
        );
    }
    assert_eq!(
        Timestamp::from_unix_nanos_wide(Timestamp::MAX.unix_nanos_wide() + 1),
        None
    );
}

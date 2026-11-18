use mailroom_logic::time::{epoch, iso};

#[test]
fn iso_round_trips() {
    assert_eq!(epoch(&iso(1795000000)), Some(1795000000));
    assert_eq!(iso(1795000000).len(), 20);
}

#[test]
fn formats_known_instant() {
    assert_eq!(
        epoch("2026-11-18T10:05:00Z").map(iso).as_deref(),
        Some("2026-11-18T10:05:00Z")
    );
}

#[test]
fn offset_and_fractions() {
    assert_eq!(
        epoch("2026-11-18T10:04:40.123000+00:00"),
        epoch("2026-11-18T10:04:40Z")
    );
    assert!(epoch("2026-11-18T10:04:40Z").is_some());
}

#[test]
fn garbage_is_none() {
    assert_eq!(epoch("nope"), None);
}

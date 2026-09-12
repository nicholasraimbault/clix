use std::time::{Duration, SystemTime};

use chrono::{Local, NaiveDateTime, NaiveTime, TimeZone, Weekday};
use clix::{add, check, check_at, hands, remove, resolve_tool, BodyId, ClixError, Schedule, Store};

fn empty_store() -> Store {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::open(dir.path()).unwrap();
    s.peers = vec![
        clix::Peer {
            name: BodyId("server".into()),
            owner_pk: vec![1; 32],
            addr: None,
        },
        clix::Peer {
            name: BodyId("phone".into()),
            owner_pk: vec![2; 32],
            addr: None,
        },
    ];
    s
}

fn t(h: u32, m: u32) -> NaiveTime {
    NaiveTime::from_hms_opt(h, m, 0).unwrap()
}

fn datetime(s: &str) -> chrono::DateTime<Local> {
    let naive = NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S").unwrap();
    match Local.from_local_datetime(&naive) {
        chrono::LocalResult::Single(t) | chrono::LocalResult::Ambiguous(t, _) => t,
        chrono::LocalResult::None => panic!("no local time for {s}"),
    }
}

#[test]
fn add_all_bodies_until_removed() {
    let mut s = empty_store();
    add(&mut s, "true", &[], false, None, None).unwrap();
    assert!(check(&s, "true", &BodyId("server".into())).is_ok());
    assert!(check(&s, "true", &BodyId("phone".into())).is_ok());
}

#[test]
fn allow_server_denies_other() {
    let mut s = empty_store();
    add(&mut s, "true", &["server".into()], false, None, None).unwrap();
    assert!(check(&s, "true", &BodyId("server".into())).is_ok());
    let e = check(&s, "true", &BodyId("phone".into())).unwrap_err();
    assert!(e.to_string().contains("not allowed"));
}

#[test]
fn expired_until_is_denied() {
    let mut s = empty_store();
    add(
        &mut s,
        "true",
        &[],
        false,
        Some(SystemTime::UNIX_EPOCH),
        None,
    )
    .unwrap();
    let e = check(&s, "true", &BodyId("server".into())).unwrap_err();
    assert!(e.to_string().contains("expired"));
}

#[test]
fn missing_is_plain_english() {
    let mut s = empty_store();
    s.body_name = "laptop".into();
    let e = check(&s, "adb", &BodyId("server".into())).unwrap_err();
    assert_eq!(e.to_string(), "adb is not added on laptop");
}

#[test]
fn days_and_hours_denies_saturday() {
    let mut s = empty_store();
    s.body_name = "laptop".into();
    add(
        &mut s,
        "true",
        &[],
        false,
        None,
        Some(Schedule {
            days: vec![
                Weekday::Mon,
                Weekday::Tue,
                Weekday::Wed,
                Weekday::Thu,
                Weekday::Fri,
            ],
            dates: vec![],
            from: Some(t(9, 0)),
            to: Some(t(17, 0)),
        }),
    )
    .unwrap();
    let saturday_noon = datetime("2026-09-12T12:00:00");
    let e = check_at(&s, "true", &BodyId("server".into()), saturday_noon).unwrap_err();
    assert!(e.to_string().contains("not added") && e.to_string().contains("at this time"));
}

#[test]
fn dates_or_days_union() {
    let mut s = empty_store();
    add(
        &mut s,
        "true",
        &[],
        false,
        None,
        Some(Schedule {
            days: vec![Weekday::Fri],
            dates: vec![1],
            from: None,
            to: None,
        }),
    )
    .unwrap();
    assert!(check_at(
        &s,
        "true",
        &BodyId("server".into()),
        datetime("2026-09-04T12:00:00")
    )
    .is_ok()); // Friday
    assert!(check_at(
        &s,
        "true",
        &BodyId("server".into()),
        datetime("2026-09-01T12:00:00")
    )
    .is_ok()); // 1st, Tuesday
    assert!(check_at(
        &s,
        "true",
        &BodyId("server".into()),
        datetime("2026-09-02T12:00:00")
    )
    .is_err()); // Tue 2nd
}

#[test]
fn add_resolves_true_via_which() {
    let path = resolve_tool("true").unwrap();
    assert!(path.ends_with("true"));
    assert!(path.is_absolute());
    let mut s = empty_store();
    let g = add(&mut s, "true", &[], false, None, None).unwrap();
    assert_eq!(g.tool, "true");
    assert_eq!(g.binary, path);
}

#[test]
fn remove_then_check_is_not_added() {
    let mut s = empty_store();
    s.body_name = "laptop".into();
    add(&mut s, "true", &[], false, None, None).unwrap();
    remove(&mut s, "true").unwrap();
    let e = check(&s, "true", &BodyId("server".into())).unwrap_err();
    assert_eq!(e.to_string(), "true is not added on laptop");
}

#[test]
fn once_and_schedule_conflict() {
    let mut s = empty_store();
    let e = add(
        &mut s,
        "true",
        &[],
        true,
        None,
        Some(Schedule {
            days: vec![Weekday::Mon],
            dates: vec![],
            from: None,
            to: None,
        }),
    )
    .unwrap_err();
    assert!(e.to_string().contains("use --once or a schedule, not both"));
}

#[test]
fn days_and_hours_allows_friday_noon_denies_before_from() {
    let mut s = empty_store();
    s.body_name = "laptop".into();
    add(
        &mut s,
        "true",
        &[],
        false,
        None,
        Some(Schedule {
            days: vec![
                Weekday::Mon,
                Weekday::Tue,
                Weekday::Wed,
                Weekday::Thu,
                Weekday::Fri,
            ],
            dates: vec![],
            from: Some(t(9, 0)),
            to: Some(t(17, 0)),
        }),
    )
    .unwrap();
    assert!(check_at(
        &s,
        "true",
        &BodyId("server".into()),
        datetime("2026-09-11T12:00:00")
    )
    .is_ok());
    let e = check_at(
        &s,
        "true",
        &BodyId("server".into()),
        datetime("2026-09-11T08:00:00"),
    )
    .unwrap_err();
    assert!(e.to_string().contains("not added") && e.to_string().contains("at this time"));
}

#[test]
fn not_allowed_is_plain_english() {
    let mut s = empty_store();
    s.body_name = "laptop".into();
    add(&mut s, "true", &["server".into()], false, None, None).unwrap();
    let e = check(&s, "true", &BodyId("phone".into())).unwrap_err();
    assert_eq!(e.to_string(), "phone is not allowed to use true on laptop");
}

#[test]
fn expired_until_is_plain_english() {
    let mut s = empty_store();
    s.body_name = "laptop".into();
    add(
        &mut s,
        "true",
        &[],
        false,
        Some(SystemTime::UNIX_EPOCH),
        None,
    )
    .unwrap();
    let e = check(&s, "true", &BodyId("server".into())).unwrap_err();
    assert_eq!(e.to_string(), "true grant on laptop has expired");
}

#[test]
fn future_until_is_allowed() {
    let mut s = empty_store();
    let until = SystemTime::now() + Duration::from_secs(3600);
    add(&mut s, "true", &[], false, Some(until), None).unwrap();
    assert!(check(&s, "true", &BodyId("server".into())).is_ok());
}

#[test]
fn hands_lists_added_tools() {
    let mut s = empty_store();
    add(&mut s, "true", &[], false, None, None).unwrap();
    let listed = hands(&s);
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].tool, "true");
}

#[test]
fn add_missing_binary_is_no_such_tool() {
    let mut s = empty_store();
    let e = add(&mut s, "clix-no-such-tool-xyz", &[], false, None, None).unwrap_err();
    match e {
        ClixError::NoSuchTool { tool } => assert_eq!(tool, "clix-no-such-tool-xyz"),
        other => panic!("expected NoSuchTool, got {other}"),
    }
}

#[test]
fn readd_replaces_who() {
    let mut s = empty_store();
    add(&mut s, "true", &["server".into()], false, None, None).unwrap();
    add(&mut s, "true", &[], false, None, None).unwrap();
    assert!(check(&s, "true", &BodyId("phone".into())).is_ok());
    assert_eq!(s.grants.len(), 1);
}

#[test]
fn allow_is_the_same_allow_from_field() {
    let mut s = empty_store();
    let g = add(&mut s, "true", &["server".into()], false, None, None).unwrap();
    assert_eq!(
        g.allow_from.as_ref().unwrap(),
        &vec![BodyId("server".into())]
    );
    assert!(g.schedule.is_none());
}

#[test]
fn unknown_allow_body_is_rejected_without_changing_grants() {
    let mut s = empty_store();
    assert!(add(&mut s, "true", &["unpaired".into()], false, None, None).is_err());
    assert!(s.grants.is_empty());
}

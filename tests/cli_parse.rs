use std::time::{Duration, SystemTime};

use clix::parse_argv;
use clix::ClixError;
use clix::Cmd;

#[test]
fn explicit_destination_preserves_body_identity_and_all_tool_arguments() {
    for (args, no_wait) in [
        (vec!["clix", "--", "job", "tool", "--no-wait", "--"], false),
        (
            vec!["clix", "--no-wait", "--", "pin", "tool", "--no-wait", "--"],
            true,
        ),
    ] {
        let body = if no_wait { "pin" } else { "job" };
        assert_eq!(
            parse_argv(&args.into_iter().map(String::from).collect::<Vec<_>>()).unwrap(),
            Cmd::Exec {
                body: body.into(),
                argv: vec!["tool".into(), "--no-wait".into(), "--".into()],
                no_wait
            }
        );
    }
    for args in [
        vec!["clix", "--"],
        vec!["clix", "--", "job"],
        vec!["clix", "--", "-bad", "tool"],
    ] {
        assert!(parse_argv(&args.into_iter().map(String::from).collect::<Vec<_>>()).is_err());
    }
}

#[test]
fn exec_is_body_then_argv() {
    let cmd = parse_argv(&[
        "clix".into(),
        "laptop".into(),
        "adb".into(),
        "devices".into(),
    ])
    .unwrap();
    match cmd {
        Cmd::Exec {
            body,
            argv,
            no_wait,
        } => {
            assert_eq!(body, "laptop");
            assert_eq!(argv, vec!["adb", "devices"]);
            assert!(!no_wait);
        }
        _ => panic!("expected exec"),
    }
}

#[test]
fn add_with_explicit_scope_is_not_once_by_default() {
    let cmd = parse_argv(&["clix".into(), "add".into(), "adb".into(), "--all".into()]).unwrap();
    match cmd {
        Cmd::Add { tool, once, .. } => {
            assert_eq!(tool, "adb");
            assert!(!once);
        }
        _ => panic!("expected add"),
    }
}

#[test]
fn add_once_for_allow() {
    let cmd = parse_argv(&[
        "clix".into(),
        "add".into(),
        "adb".into(),
        "--once".into(),
        "--for".into(),
        "2h".into(),
        "--allow".into(),
        "server".into(),
    ])
    .unwrap();
    match cmd {
        Cmd::Add {
            tool,
            once,
            allow,
            for_dur,
            ..
        } => {
            assert_eq!(tool, "adb");
            assert!(once);
            assert_eq!(allow, vec!["server"]);
            assert_eq!(for_dur.unwrap().as_secs(), 7200);
        }
        _ => panic!("expected add"),
    }
}

#[test]
fn server_fills_the_same_allow_field() {
    let cmd = parse_argv(&["clix".into(), "add".into(), "adb".into(), "--server".into()]).unwrap();
    match cmd {
        Cmd::Add { allow, .. } => assert_eq!(allow, vec!["server"]),
        _ => panic!("expected add"),
    }
}

#[test]
fn allow_is_repeatable() {
    let cmd = parse_argv(&[
        "clix".into(),
        "add".into(),
        "adb".into(),
        "--allow".into(),
        "server".into(),
        "--allow".into(),
        "phone".into(),
    ])
    .unwrap();
    match cmd {
        Cmd::Add { allow, .. } => assert_eq!(allow, vec!["server", "phone"]),
        _ => panic!("expected add"),
    }
}

#[test]
fn weekdays_is_a_bool_on_add() {
    let cmd = parse_argv(&[
        "clix".into(),
        "add".into(),
        "adb".into(),
        "--all".into(),
        "--weekdays".into(),
        "--from".into(),
        "9am".into(),
        "--to".into(),
        "5pm".into(),
    ])
    .unwrap();
    match cmd {
        Cmd::Add {
            weekdays,
            days,
            from,
            to,
            ..
        } => {
            assert!(weekdays);
            assert!(days.is_empty());
            assert_eq!(from.as_deref(), Some("9am"));
            assert_eq!(to.as_deref(), Some("5pm"));
        }
        _ => panic!("expected add"),
    }
}

#[test]
fn days_dates_from_to() {
    let cmd = parse_argv(&[
        "clix".into(),
        "add".into(),
        "adb".into(),
        "--all".into(),
        "--days".into(),
        "mon,wed,fri".into(),
        "--dates".into(),
        "1,15".into(),
        "--from".into(),
        "9am".into(),
        "--to".into(),
        "5pm".into(),
    ])
    .unwrap();
    match cmd {
        Cmd::Add {
            days,
            dates,
            from,
            to,
            weekdays,
            ..
        } => {
            assert_eq!(days, vec!["mon", "wed", "fri"]);
            assert_eq!(dates, vec![1, 15]);
            assert_eq!(from.as_deref(), Some("9am"));
            assert_eq!(to.as_deref(), Some("5pm"));
            assert!(!weekdays);
        }
        _ => panic!("expected add"),
    }
}

#[test]
fn until_is_local_today_or_tomorrow() {
    let cmd = parse_argv(&[
        "clix".into(),
        "add".into(),
        "adb".into(),
        "--all".into(),
        "--until".into(),
        "5pm".into(),
    ])
    .unwrap();
    match cmd {
        Cmd::Add { until, .. } => {
            let t = until.expect("until");
            let now = SystemTime::now();
            assert!(t > now - Duration::from_secs(1));
            assert!(t <= now + Duration::from_secs(26 * 3600));
        }
        _ => panic!("expected add"),
    }
}

#[test]
fn days_and_weekdays_conflict() {
    let err = parse_argv(&[
        "clix".into(),
        "add".into(),
        "adb".into(),
        "--all".into(),
        "--days".into(),
        "mon".into(),
        "--weekdays".into(),
    ])
    .unwrap_err();
    match err {
        ClixError::Usage(s) => assert!(s.contains("--days") && s.contains("--weekdays")),
        other => panic!("expected usage, got {other}"),
    }
}

#[test]
fn once_rejects_repeating_schedule() {
    for extra in [
        vec!["--days".into(), "mon".into()],
        vec!["--dates".into(), "1".into()],
        vec!["--weekdays".into()],
    ] {
        let mut argv = vec![
            "clix".into(),
            "add".into(),
            "adb".into(),
            "--all".into(),
            "--once".into(),
        ];
        argv.extend(extra);
        let err = parse_argv(&argv).unwrap_err();
        match err {
            ClixError::Usage(s) => assert!(s.contains("--once"), "{s}"),
            other => panic!("expected usage, got {other}"),
        }
    }
}

#[test]
fn add_without_tool_is_usage() {
    let err = parse_argv(&["clix".into(), "add".into()]).unwrap_err();
    match err {
        ClixError::Usage(s) => {
            assert!(!s.is_empty());
            assert!(s.len() < 80, "usage string should be short: {s}");
        }
        other => panic!("expected usage, got {other}"),
    }
}

#[test]
fn reserved_commands() {
    assert!(matches!(
        parse_argv(&["clix".into(), "hands".into()]).unwrap(),
        Cmd::Hands
    ));
    assert!(matches!(
        parse_argv(&["clix".into(), "log".into()]).unwrap(),
        Cmd::Log
    ));
    assert!(matches!(
        parse_argv(&["clix".into(), "pending".into()]).unwrap(),
        Cmd::Pending
    ));
    match parse_argv(&["clix".into(), "allow".into(), "req-1".into()]).unwrap() {
        Cmd::Allow { once, allow, .. } => {
            assert!(once, "default allow is --once");
            assert!(allow.is_empty());
        }
        other => panic!("expected allow, got {other:?}"),
    }
    assert!(matches!(
        parse_argv(&["clix".into(), "deny".into(), "req-1".into()]).unwrap(),
        Cmd::Deny { request_id } if request_id == "req-1"
    ));
    assert!(matches!(
        parse_argv(&["clix".into(), "daemon".into()]).unwrap(),
        Cmd::Daemon
    ));
    assert!(matches!(
        parse_argv(&["clix".into(), "install".into()]).unwrap(),
        Cmd::Install
    ));
    assert!(matches!(
        parse_argv(&["clix".into(), "status".into()]).unwrap(),
        Cmd::Status
    ));
    match parse_argv(&["clix".into(), "pair".into()]).unwrap() {
        Cmd::Pair { phrase, name } => {
            assert_eq!(phrase, None);
            assert_eq!(name, None);
        }
        _ => panic!("expected pair"),
    }
    match parse_argv(&["clix".into(), "pair".into(), "oak-42".into()]).unwrap() {
        Cmd::Pair { phrase, name } => {
            assert_eq!(phrase.as_deref(), Some("oak-42"));
            assert_eq!(name, None);
        }
        _ => panic!("expected pair"),
    }
    match parse_argv(&[
        "clix".into(),
        "pair".into(),
        "--name".into(),
        "laptop".into(),
    ])
    .unwrap()
    {
        Cmd::Pair { phrase, name } => {
            assert_eq!(phrase, None);
            assert_eq!(name.as_deref(), Some("laptop"));
        }
        _ => panic!("expected pair --name"),
    }
    match parse_argv(&["clix".into(), "remove".into(), "adb".into()]).unwrap() {
        Cmd::Remove { tool } => assert_eq!(tool, "adb"),
        _ => panic!("expected remove"),
    }
}

#[test]
fn remove_without_tool_is_usage() {
    let err = parse_argv(&["clix".into(), "remove".into()]).unwrap_err();
    match err {
        ClixError::Usage(s) => assert!(!s.is_empty()),
        other => panic!("expected usage, got {other}"),
    }
}

#[test]
fn exec_no_wait_flag() {
    for argv in [
        vec![
            "clix".into(),
            "--no-wait".into(),
            "laptop".into(),
            "adb".into(),
        ],
        vec![
            "clix".into(),
            "laptop".into(),
            "--no-wait".into(),
            "adb".into(),
        ],
    ] {
        match parse_argv(&argv).unwrap() {
            Cmd::Exec {
                body,
                argv,
                no_wait,
            } => {
                assert_eq!(body, "laptop");
                assert_eq!(argv, vec!["adb"]);
                assert!(no_wait);
            }
            other => panic!("expected exec, got {other:?}"),
        }
    }
    match parse_argv(&[
        "clix".into(),
        "laptop".into(),
        "adb".into(),
        "--no-wait".into(),
    ])
    .unwrap()
    {
        Cmd::Exec { argv, no_wait, .. } => {
            assert_eq!(argv, vec!["adb", "--no-wait"]);
            assert!(!no_wait);
        }
        other => panic!("expected exec, got {other:?}"),
    }
}

#[test]
fn exec_without_cmd_is_usage() {
    let err = parse_argv(&["clix".into(), "laptop".into()]).unwrap_err();
    match err {
        ClixError::Usage(s) => assert!(!s.is_empty()),
        other => panic!("expected usage, got {other}"),
    }
}

#[test]
fn request_is_body_and_tool() {
    match parse_argv(&[
        "clix".into(),
        "request".into(),
        "laptop".into(),
        "adb".into(),
    ])
    .unwrap()
    {
        Cmd::Request { body, tool } => {
            assert_eq!(body, "laptop");
            assert_eq!(tool, "adb");
        }
        other => panic!("expected request, got {other:?}"),
    }
}

#[test]
fn request_without_tool_is_usage() {
    let err = parse_argv(&["clix".into(), "request".into(), "laptop".into()]).unwrap_err();
    match err {
        ClixError::Usage(s) => {
            assert!(!s.is_empty());
            assert!(s.len() < 80, "usage string should be short: {s}");
        }
        other => panic!("expected usage, got {other}"),
    }
}

#[test]
fn allow_for_is_not_once() {
    match parse_argv(&[
        "clix".into(),
        "allow".into(),
        "req-1".into(),
        "--for".into(),
        "2h".into(),
    ])
    .unwrap()
    {
        Cmd::Allow { once, for_dur, .. } => {
            assert!(!once);
            assert_eq!(for_dur, Some(Duration::from_secs(2 * 3600)));
        }
        other => panic!("expected allow, got {other:?}"),
    }
}

#[test]
fn bad_weekday_is_usage() {
    let err = parse_argv(&[
        "clix".into(),
        "add".into(),
        "adb".into(),
        "--all".into(),
        "--days".into(),
        "monday".into(),
    ])
    .unwrap_err();
    match err {
        ClixError::Usage(s) => assert!(s.contains("weekday")),
        other => panic!("expected usage, got {other}"),
    }
}

#[test]
fn bad_date_is_usage() {
    let err = parse_argv(&[
        "clix".into(),
        "add".into(),
        "adb".into(),
        "--all".into(),
        "--dates".into(),
        "32".into(),
    ])
    .unwrap_err();
    match err {
        ClixError::Usage(s) => assert!(s.contains("date")),
        other => panic!("expected usage, got {other}"),
    }
}

#[test]
fn owner_decisions_need_an_explicit_request_id() {
    for op in ["allow", "deny"] {
        assert!(parse_argv(&["clix".into(), op.into()]).is_err());
    }
}

#[test]
fn add_requires_explicit_scope() {
    // clix add <tool> with no --allow/--all/--server no longer grants every
    // paired machine by default; it must be an error naming the choice.
    let err = parse_argv(&["clix".into(), "add".into(), "adb".into()]).unwrap_err();
    match err {
        ClixError::Usage(s) => {
            assert!(s.contains("--allow") && s.contains("--all"), "{s}")
        }
        _ => panic!("expected usage error, got {err:?}"),
    }
}

#[test]
fn add_all_grants_every_machine() {
    let cmd = parse_argv(&["clix".into(), "add".into(), "adb".into(), "--all".into()]).unwrap();
    match cmd {
        Cmd::Add { tool, allow, .. } => {
            assert_eq!(tool, "adb");
            assert!(allow.is_empty(), "--all means no allow-list: {allow:?}");
        }
        _ => panic!("expected add"),
    }
}

#[test]
fn add_all_conflicts_with_an_allow_list() {
    let err = parse_argv(&[
        "clix".into(),
        "add".into(),
        "adb".into(),
        "--all".into(),
        "--allow".into(),
        "server".into(),
    ])
    .unwrap_err();
    match err {
        ClixError::Usage(s) => assert!(s.contains("--all"), "{s}"),
        _ => panic!("expected usage error, got {err:?}"),
    }
}

#[test]
fn add_only_builds_exact_argument_vectors() {
    let cmd = parse_argv(&[
        "clix".into(),
        "add".into(),
        "adb".into(),
        "--allow".into(),
        "server".into(),
        "--only".into(),
        "devices".into(),
        "--only".into(),
        "devices -l".into(),
        "--only".into(),
        "".into(),
    ])
    .unwrap();
    match cmd {
        Cmd::Add { args, .. } => assert_eq!(
            args,
            Some(vec![
                vec!["devices".to_string()],
                vec!["devices".to_string(), "-l".to_string()],
                vec![],
            ])
        ),
        _ => panic!("expected add"),
    }
}

#[test]
fn add_without_only_leaves_arguments_unconstrained() {
    let cmd = parse_argv(&["clix".into(), "add".into(), "adb".into(), "--all".into()]).unwrap();
    match cmd {
        Cmd::Add { args, .. } => assert_eq!(args, None),
        _ => panic!("expected add"),
    }
}

#[test]
fn allow_rejects_argument_limits() {
    let err = parse_argv(&[
        "clix".into(),
        "allow".into(),
        "some-request".into(),
        "--only".into(),
        "devices".into(),
    ])
    .unwrap_err();
    match err {
        ClixError::Usage(s) => assert!(s.contains("clix add"), "{s}"),
        other => panic!("expected usage, got {other}"),
    }
}

#[test]
fn at_body_addresses_a_machine_even_when_its_name_is_a_command() {
    for (args, body, argv, no_wait) in [
        (
            vec!["clix", "@laptop", "adb", "devices"],
            "laptop",
            vec!["adb", "devices"],
            false,
        ),
        (vec!["clix", "@status", "adb"], "status", vec!["adb"], false),
        (
            vec!["clix", "--no-wait", "@laptop", "adb"],
            "laptop",
            vec!["adb"],
            true,
        ),
        // Tool arguments are preserved, including ones that look like flags.
        (
            vec!["clix", "@laptop", "adb", "--no-wait", "-s"],
            "laptop",
            vec!["adb", "--no-wait", "-s"],
            false,
        ),
    ] {
        let argv_in: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        match parse_argv(&argv_in).unwrap() {
            Cmd::Exec {
                body: b,
                argv: a,
                no_wait: w,
            } => {
                assert_eq!(b, body, "{args:?}");
                assert_eq!(a, argv, "{args:?}");
                assert_eq!(w, no_wait, "{args:?}");
            }
            other => panic!("{args:?} parsed as {other:?}"),
        }
    }
}

#[test]
fn at_body_requires_a_valid_name_and_a_tool() {
    for args in [
        vec!["clix", "@"],
        vec!["clix", "@laptop"],
        vec!["clix", "@bad!", "x"],
    ] {
        let argv_in: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        assert!(parse_argv(&argv_in).is_err(), "{args:?} should be rejected");
    }
}

#[test]
fn grants_is_an_alias_for_hands() {
    for word in ["grants", "hands"] {
        let cmd = parse_argv(&["clix".into(), word.into()]).unwrap();
        assert_eq!(cmd, Cmd::Hands, "{word}");
    }
}

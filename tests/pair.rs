mod common;

use serde_json::json;

use common::TestDaemon;

#[test]
fn phrase_is_three_wordlist_words() {
    let words: Vec<&str> = include_str!("../src/pair_words.txt")
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    assert_eq!(words.len(), 256);
    let p = clix::phrase();
    let parts: Vec<&str> = p.split('-').collect();
    assert_eq!(parts.len(), 3, "{p}");
    for w in &parts {
        assert!(words.contains(w), "{w} not in word list");
        assert!(w.chars().all(|c| c.is_ascii_lowercase()), "{w}");
    }
}

#[tokio::test]
async fn pair_two_daemons() {
    let a = TestDaemon::spawn_named("laptop").await;
    let b = TestDaemon::spawn_named("server").await;
    let phrase = a.rpc(json!({"op":"pair_start"})).await.unwrap()["phrase"]
        .as_str()
        .unwrap()
        .to_string();
    b.rpc(json!({"op":"pair_join","phrase": phrase}))
        .await
        .unwrap();
    // wait until both see a peer
    let hands = a.rpc(json!({"op":"status"})).await.unwrap();
    assert_eq!(hands["peers"][0]["name"], "server");
}

#[tokio::test]
async fn pair_stores_owner_keys_on_both_sides() {
    let a = TestDaemon::spawn_named("laptop").await;
    let b = TestDaemon::spawn_named("server").await;
    let phrase = a.rpc(json!({"op": "pair_start"})).await.unwrap()["phrase"]
        .as_str()
        .unwrap()
        .to_string();
    b.rpc(json!({"op": "pair_join", "phrase": phrase}))
        .await
        .unwrap();
    let sa = a.rpc(json!({"op": "status"})).await.unwrap();
    let sb = b.rpc(json!({"op": "status"})).await.unwrap();
    assert_eq!(sb["peers"][0]["name"], "laptop");
    let pk_a = sa["peers"][0]["owner_pk"].as_array().expect("owner_pk");
    let pk_b = sb["peers"][0]["owner_pk"].as_array().expect("owner_pk");
    assert_eq!(pk_a.len(), 32);
    assert_eq!(pk_b.len(), 32);
    assert_ne!(pk_a, pk_b);
}

#[tokio::test]
async fn wrong_phrase_does_not_match() {
    let a = TestDaemon::spawn_named("laptop").await;
    let b = TestDaemon::spawn_named("server").await;
    let _ = a.rpc(json!({"op": "pair_start"})).await.unwrap();
    let e = b
        .rpc(json!({
            "op": "pair_join",
            "phrase": "nope-nope-nope",
            "addr": a.mesh_addr()
        }))
        .await
        .unwrap_err();
    // The listener may reject its MAC and close before sending an identity.
    // The joiner cannot distinguish that EOF from a lost connection.
    assert!(
        matches!(
            e.to_string().as_str(),
            "phrase did not match" | "pairing did not finish; check the phrase and connection"
        ),
        "{e}"
    );
    let sa = a.rpc(json!({"op": "status"})).await.unwrap();
    let sb = b.rpc(json!({"op": "status"})).await.unwrap();
    assert!(sa["peers"].as_array().unwrap().is_empty());
    assert!(sb["peers"].as_array().unwrap().is_empty());
}

#[test]
fn hostname_default_is_sanitized_into_a_valid_body_name() {
    // An FQDN in /etc/hostname must not yield an invalid body name that
    // crashes the daemon at startup. Only the first label is kept.
    assert_eq!(clix::sanitize_body_name("devbox.example.com"), "devbox");
    // Leading non-alphanumerics are stripped; disallowed bytes dropped.
    assert_eq!(clix::sanitize_body_name("-weird name!"), "weirdname");
    // An all-invalid or empty source falls back to the generic default.
    assert_eq!(clix::sanitize_body_name(""), "clix");
    assert_eq!(clix::sanitize_body_name("...."), "clix");
    // An already-valid name is preserved unchanged.
    assert_eq!(clix::sanitize_body_name("laptop-1"), "laptop-1");
    // Whatever it returns must satisfy validate_name (no panic = valid).
    for raw in ["devbox.example.com", "", "....", "-weird name!", "laptop-1"] {
        let name = clix::sanitize_body_name(raw);
        assert!(
            name.bytes()
                .next()
                .is_some_and(|b| b.is_ascii_alphanumeric()),
            "sanitized {raw:?} -> {name:?} is not a valid body name"
        );
    }
}

#[tokio::test]
async fn new_pairing_refuses_a_machine_named_like_an_owner_command() {
    // Choosing a command name for this machine is refused up front.
    let a = TestDaemon::spawn_named("laptop").await;
    let err = a
        .rpc(json!({"op":"pair_start","name":"status"}))
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("status") && err.contains("command"), "{err}");

    // A peer that announces a command name is refused by this machine, and
    // neither side saves the pairing.
    let listener = TestDaemon::spawn_named("laptop").await;
    let joiner = TestDaemon::spawn_named("pin").await;
    let phrase = listener.rpc(json!({"op":"pair_start"})).await.unwrap()["phrase"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(joiner
        .rpc(json!({"op":"pair_join","phrase": phrase}))
        .await
        .is_err());
    for d in [&listener, &joiner] {
        let status = d.rpc(json!({"op":"status"})).await.unwrap();
        assert!(
            status["peers"].as_array().unwrap().is_empty(),
            "no pairing may be saved: {status}"
        );
    }
}

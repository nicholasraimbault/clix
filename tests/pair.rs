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
    assert_eq!(e.to_string(), "phrase did not match");
    let sa = a.rpc(json!({"op": "status"})).await.unwrap();
    let sb = b.rpc(json!({"op": "status"})).await.unwrap();
    assert!(sa["peers"].as_array().unwrap().is_empty());
    assert!(sb["peers"].as_array().unwrap().is_empty());
}

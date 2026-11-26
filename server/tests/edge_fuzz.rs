//! Edge cases of choosing a hosted game (docs/MULTIPLAYER.md, "Choosing a game") and a
//! deterministic randomized run of the relay that checks, after every step, what each client was
//! told: the games offered, the "Players in Match" lists, the hosts' seats, the match starts.

#![allow(dead_code)]

include!("harness/mod.rs");

use rg_server::casino::HEROES;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

const BASE: u32 = 900_000_000;
const TEN_MIN: u64 = 600_000;
const LOCK_MS: u64 = 3_000;
const LOBBY: &str = "CasinoSystem";

// ============================================================================ small helpers

fn qm_as(t: &mut T, c: &C, hero: &str) {
    let req = frame(CH_MATCHMAKER, &Writer::new().u8(REQUEST).u8(9).str(hero).str("FALSE").done());
    t.bin(c, &req);
}

/// The ids of the games in a `games` message.
fn ids_of(g: &Value) -> Vec<u64> {
    g["games"].as_array().unwrap().iter().map(|e| e[0].as_u64().unwrap()).collect()
}

/// The `games` entry of this game, if offered.
fn entry_of(g: &Value, id: u64) -> Option<Value> {
    g["games"].as_array().unwrap().iter().find(|e| e[0] == id).cloned()
}

/// The names EVENT CASINO_ENTER introduced.
fn intros(msgs: &[Msg]) -> Vec<String> {
    let mut v: Vec<String> = msgs
        .iter()
        .filter(|m| m.0 != CH_TABLE && m.1 == EVENT && m.2 == 8)
        .map(|m| {
            let mut r = Reader::new(&m.3);
            r.str().unwrap();
            r.kv().unwrap().into_iter().find(|(k, _)| k == "name").unwrap().1
        })
        .collect();
    v.sort();
    v
}

fn ping_all(t: &mut T, cs: &[&C]) {
    for c in cs {
        t.send(c, json!({"t": "ping"}));
    }
}

/// Lets time pass in 20 s steps while these connections keep pinging (the others go idle).
fn wait_with(t: &mut T, ms: u64, cs: &[&C]) {
    let mut left = ms;
    while left > 0 {
        let d = left.min(20_000);
        ping_all(t, cs);
        t.tick(d);
        left -= d;
    }
    ping_all(t, cs);
}

// ============================================================================ deterministic edge cases

/// A game whose seats are all taken waits for its host; meanwhile it is offered to nobody else and
/// an older game's player is not squeezed in. Start takes exactly the seated player.
#[test]
fn a_full_game_waiting_for_its_host_is_offered_to_no_one_else() {
    let mut t = T::new(Config::default());
    let ann = t.host_approving("Ann", 1, "82.0.0.1");
    let pia = t.chooser_at("Pia", "82.0.0.2");
    t.quick_match(&pia);
    t.send(&pia, json!({"t": "pick", "id": 900000001}));
    pia.take();
    assert_eq!(seats(&ann.texts()), Some(vec!["Pia"]));
    let rex = t.chooser_at("Rex", "82.0.0.3");
    t.quick_match(&rex);
    let g = last_games(&rex.texts()).cloned().unwrap();
    assert!(ids_of(&g).is_empty(), "the full game is not offered: {g}");
    let quinn = t.player_at("Quinn", "82.0.0.4");
    t.quick_match(&quinn);
    let mut ql = Vec::new();
    waiting_list(&quinn.msgs(), &mut ql);
    assert_eq!(names(&ql), ["Rex"], "Quinn waits with Rex, not in Ann's full game");
    assert!(ann.texts().iter().all(|m| m["t"] != "seats"), "Ann's seats did not change");
    t.send(&ann, json!({"t": "start"}));
    assert!(got_lock(&pia.msgs()));
    assert!(!got_lock(&rex.msgs()) && !got_lock(&quinn.msgs()));
    assert_eq!(t.relay.status()["hosted"], 0);
    assert_eq!(t.relay.status()["queued"], 2);
}

/// Two players see the last seat; the first pick takes it, the second is refused and gets the
/// list again (without that game).
#[test]
fn the_last_seat_goes_to_the_first_who_picks_it() {
    let mut t = T::new(Config::default());
    let ann = t.host_approving("Ann", 1, "82.0.0.1");
    let pia = t.chooser_at("Pia", "82.0.0.2");
    let rex = t.chooser_at("Rex", "82.0.0.3");
    t.quick_match(&pia);
    t.quick_match(&rex);
    assert_eq!(ids_of(last_games(&pia.texts()).unwrap()), [900000001]);
    assert_eq!(ids_of(last_games(&rex.texts()).unwrap()), [900000001]);
    t.send(&pia, json!({"t": "pick", "id": 900000001}));
    pia.take();
    let g = last_games(&rex.texts()).cloned().unwrap();
    assert!(ids_of(&g).is_empty(), "Rex no longer sees the full game: {g}");
    t.send(&rex, json!({"t": "pick", "id": 900000001})); // pressed OK on the old list
    let g = last_games(&rex.texts()).cloned().expect("the list is sent again");
    assert_eq!(g["seat"], 0);
    assert!(ids_of(&g).is_empty());
    assert_eq!(last_games(&pia.texts()), None, "Pia's list did not change");
    assert_eq!(seats(&ann.texts()), Some(vec!["Pia"]));
}

/// START with nobody seated does nothing (the host hears the seats again); once someone sits
/// down it works.
#[test]
fn a_start_with_nobody_seated_does_nothing() {
    let mut t = T::new(Config::default());
    let ann = t.host_approving("Ann", 1, "82.0.0.1");
    assert_eq!(seats(&ann.texts()), None, "host_approving took the first seats message");
    t.send(&ann, json!({"t": "start"}));
    assert_eq!(seats(&ann.texts()), Some(vec![]));
    assert_eq!(t.relay.status()["hosted"], 1, "still waiting");
    let pia = t.chooser_at("Pia", "82.0.0.2");
    t.quick_match(&pia);
    t.send(&pia, json!({"t": "pick", "id": 900000001}));
    pia.take();
    t.send(&ann, json!({"t": "start"}));
    assert!(got_lock(&pia.msgs()));
}

/// The panel said START, but a player left before the host pressed it: nothing starts, and the
/// host hears who is still there.
#[test]
fn a_start_after_a_player_left_waits_and_tells_the_host() {
    let mut t = T::new(Config::default());
    let ann = t.host_approving("Ann", 2, "82.0.0.1");
    let pia = t.chooser_at("Pia", "82.0.0.2");
    let rex = t.chooser_at("Rex", "82.0.0.3");
    for c in [&pia, &rex] {
        t.quick_match(c);
        t.send(c, json!({"t": "pick", "id": 900000001}));
    }
    assert_eq!(seats(&ann.texts()), Some(vec!["Pia", "Rex"]));
    t.cancel(&rex); // Back, just before Ann presses OK
    assert_eq!(seats(&ann.texts()), Some(vec!["Pia"]));
    pia.take();
    t.send(&ann, json!({"t": "start"}));
    assert!(!got_lock(&pia.msgs()), "one seat is free");
    assert_eq!(seats(&ann.texts()), Some(vec!["Pia"]), "the host's panel is told again");
}

/// The seats list follows every change in the order players sat down, and says how many seats
/// the game has.
#[test]
fn the_host_hears_every_seating_change_in_order() {
    let mut t = T::new(Config::default());
    let ann = t.host_approving("Ann", 3, "82.0.0.1");
    let ps: Vec<C> = ["Pia", "Rex", "Sue"].iter().enumerate().map(|(i, n)| t.chooser_at(n, &format!("82.0.0.{}", i + 2))).collect();
    let mut seen = Vec::new();
    for p in &ps {
        t.quick_match(p);
        t.send(p, json!({"t": "pick", "id": 900000001}));
        let all = ann.texts();
        let s = all.iter().rev().find(|m| m["t"] == "seats").unwrap().clone();
        assert_eq!(s["of"], 3);
        assert_eq!(s["room"], "HOST_Ann_1");
        seen.push(s["names"].clone());
    }
    assert_eq!(seen, vec![json!(["Pia"]), json!(["Pia", "Rex"]), json!(["Pia", "Rex", "Sue"])]);
    t.disconnect(&ps[1]);
    assert_eq!(seats(&ann.texts()), Some(vec!["Pia", "Sue"]));
}

/// A kicked player is not offered that game again, also not after starting the game again under
/// another capitalisation of the name, and a pick of its id (an old list) is refused.
#[test]
fn a_kicked_player_is_not_offered_that_game_even_after_reconnecting() {
    let mut t = T::new(Config::default());
    let ann = t.host_approving("Ann", 2, "82.0.0.1");
    let pia = t.chooser_at("Pia", "82.0.0.2");
    t.quick_match(&pia);
    t.send(&pia, json!({"t": "pick", "id": 900000001}));
    pia.take();
    t.send(&ann, json!({"t": "kick", "name": "PIA"}));
    assert_eq!(seats(&ann.texts()), Some(vec![]));
    let g = last_games(&pia.texts()).cloned().unwrap();
    assert_eq!(g["seat"], 0);
    assert!(ids_of(&g).is_empty(), "{g}");
    t.disconnect(&pia);
    let pia2 = t.chooser_at("pIA", "82.0.0.2");
    t.quick_match(&pia2);
    let g = last_games(&pia2.texts()).cloned().unwrap();
    assert!(ids_of(&g).is_empty(), "still not offered: {g}");
    t.send(&pia2, json!({"t": "pick", "id": 900000001}));
    let g = last_games(&pia2.texts()).cloned().unwrap();
    assert_eq!(g["seat"], 0, "refused");
    assert_eq!(seats(&ann.texts()), None, "nobody sat down");
}

/// A kick bars only that host's game: the player can still sit in another.
#[test]
fn a_kick_bars_only_that_hosts_game() {
    let mut t = T::new(Config::default());
    let ann = t.host_approving("Ann", 2, "82.0.0.1");
    t.tick(1_000);
    let ben = t.host_approving("Ben", 2, "82.0.0.5");
    let pia = t.chooser_at("Pia", "82.0.0.2");
    t.quick_match(&pia);
    t.send(&pia, json!({"t": "pick", "id": 900000001}));
    t.send(&ann, json!({"t": "kick", "name": "Pia"}));
    let g = last_games(&pia.texts()).cloned().unwrap();
    assert_eq!(ids_of(&g), [900000002], "Ben's game is still offered");
    t.send(&pia, json!({"t": "pick", "id": 900000002}));
    assert_eq!(last_games(&pia.texts()).unwrap()["seat"], 900000002);
    assert_eq!(seats(&ben.texts()), Some(vec!["Pia"]));
}

/// The host's game at home seats a player by itself, but not one the host sent away.
#[test]
fn a_kicked_player_from_the_hosts_home_is_not_seated_there_again() {
    let mut t = T::new(Config::default());
    let ann = t.host_approving("Ann", 2, "82.0.0.1");
    let pia = t.chooser_at("Pia", "82.0.0.1"); // same home: seated by itself
    t.quick_match(&pia);
    assert_eq!(last_games(&pia.texts()).unwrap()["seat"], 900000001);
    let ola = t.player_at("Ola", "82.0.0.1"); // an older game at the same home
    t.quick_match(&ola);
    assert_eq!(seats(&ann.texts()), Some(vec!["Pia", "Ola"]));
    t.send(&ann, json!({"t": "kick", "name": "Pia"}));
    t.send(&ann, json!({"t": "kick", "name": "Ola"}));
    assert_eq!(seats(&ann.texts()), Some(vec![]));
    let g = last_games(&pia.texts()).cloned().unwrap();
    assert_eq!(g["seat"], 0);
    assert!(ids_of(&g).is_empty(), "{g}");
    // Back and Quick Match again: still not at home
    for c in [&pia, &ola] {
        t.cancel(c);
        t.quick_match(c);
    }
    assert_eq!(last_games(&pia.texts()).unwrap()["seat"], 0);
    let mut ol = Vec::new();
    waiting_list(&ola.msgs(), &mut ol);
    assert_eq!(names(&ol), ["Pia"], "Ola waits with Pia");
    assert_eq!(seats(&ann.texts()), None);
}

/// A player the host sent away waits at the front of the queue: the next free seat is theirs,
/// not the one of a player who waited longer.
#[test]
fn a_kicked_player_gets_the_next_free_seat_first() {
    let mut t = T::new(Config::default());
    let ann = t.host_approving("Ann", 1, "82.0.0.1");
    let quinn = t.player_at("Quinn", "82.0.0.4");
    t.quick_match(&quinn); // seated in Ann's game
    let ola = t.player_at("Ola", "82.0.0.5");
    t.quick_match(&ola); // waits: the only game is full
    t.send(&ann, json!({"t": "kick", "name": "Quinn"}));
    assert_eq!(seats(&ann.texts()), Some(vec!["Ola"]), "Ola takes the freed seat");
    quinn.take();
    let _ben = t.host_at("Ben", 1, 2, "82.0.0.6");
    let (mq, mo) = (quinn.msgs(), ola.msgs());
    assert!(got_lock(&mq), "Quinn, at the front, gets Ben's seat");
    assert!(!got_lock(&mo));
}

/// Kicking a name that is not seated (a typo, a player still choosing) changes nothing: the host
/// hears the seats again and that player can still sit down.
#[test]
fn kicking_someone_not_seated_only_repeats_the_seats() {
    let mut t = T::new(Config::default());
    let ann = t.host_approving("Ann", 1, "82.0.0.1");
    let pia = t.chooser_at("Pia", "82.0.0.2");
    t.quick_match(&pia);
    pia.take();
    t.send(&ann, json!({"t": "kick", "name": "Nobody"}));
    assert_eq!(seats(&ann.texts()), Some(vec![]));
    t.send(&ann, json!({"t": "kick", "name": "Pia"})); // choosing, not seated
    assert_eq!(seats(&ann.texts()), Some(vec![]));
    assert_eq!(last_games(&pia.texts()), None, "Pia's list did not change");
    t.send(&pia, json!({"t": "pick", "id": 900000001}));
    assert_eq!(last_games(&pia.texts()).unwrap()["seat"], 900000001, "not barred");
}

/// Ten minutes after a kick, the game is offered again (here: once something else happens).
#[test]
fn ten_minutes_after_a_kick_the_game_is_offered_again() {
    let mut t = T::new(Config::default());
    let ann = t.host_approving("Ann", 2, "82.0.0.1");
    let pia = t.chooser_at("Pia", "82.0.0.2");
    t.quick_match(&pia);
    t.send(&pia, json!({"t": "pick", "id": 900000001}));
    t.send(&ann, json!({"t": "kick", "name": "Pia"}));
    pia.take();
    wait_with(&mut t, TEN_MIN - 20_000, &[&ann, &pia]);
    let rex = t.chooser_at("Rex", "82.0.0.3");
    t.quick_match(&rex); // something happens before the 10 minutes are over
    assert!(last_games(&pia.texts()).is_none_or(|g| ids_of(g).is_empty()), "not yet offered");
    wait_with(&mut t, 20_000, &[&ann, &pia, &rex]);
    t.cancel(&rex);
    t.quick_match(&rex); // and again after them
    let g = last_games(&pia.texts()).cloned().expect("Pia's list is updated");
    assert_eq!(ids_of(&g), [900000001], "offered again: {g}");
    t.send(&pia, json!({"t": "pick", "id": 900000001}));
    assert_eq!(last_games(&pia.texts()).unwrap()["seat"], 900000001);
}

/// The end of a kick's 10 minutes is a change by itself: a kicked player who keeps waiting is
/// offered (older games: seated in) that game again without anyone doing anything.
#[test]
fn an_expired_kick_offers_the_game_again_by_itself() {
    let mut t = T::new(Config::default());
    let ann = t.host_approving("Ann", 2, "82.0.0.1");
    let pia = t.chooser_at("Pia", "82.0.0.2");
    let quinn = t.player_at("Quinn", "82.0.0.4");
    t.quick_match(&pia);
    t.send(&pia, json!({"t": "pick", "id": 900000001}));
    t.quick_match(&quinn); // an older game: seated by itself
    let mut ql = Vec::new();
    waiting_list(&quinn.msgs(), &mut ql);
    assert_eq!(names(&ql), ["Ann", "Pia"]);
    t.send(&ann, json!({"t": "kick", "name": "Pia"}));
    t.send(&ann, json!({"t": "kick", "name": "Quinn"}));
    assert_eq!(seats(&ann.texts()), Some(vec![]));
    waiting_list(&quinn.msgs(), &mut ql);
    assert_eq!(names(&ql), ["Pia"], "Quinn waits with Pia");
    pia.take();
    wait_with(&mut t, TEN_MIN + 30_000, &[&ann, &pia, &quinn]);
    waiting_list(&quinn.msgs(), &mut ql);
    assert_eq!(names(&ql), ["Ann"], "Quinn is seated in Ann's game again");
    let g = last_games(&pia.texts()).cloned().expect("Pia's list is updated");
    assert_eq!(g["games"], json!([[900000001, "Ann", 7, 1, 2, 0, 1]]), "offered again");
}

/// A host who stops hosting while players choose disappears from every list at once; the
/// players seated in it choose again.
#[test]
fn a_host_who_stops_disappears_from_every_list() {
    let mut t = T::new(Config::default());
    let ann = t.host_approving("Ann", 3, "82.0.0.1");
    t.tick(1_000);
    let _ben = t.host_approving("Ben", 3, "82.0.0.5");
    let ps: Vec<C> = ["Pia", "Rex", "Sue"].iter().enumerate().map(|(i, n)| t.chooser_at(n, &format!("82.0.0.{}", i + 2))).collect();
    for p in &ps {
        t.quick_match(p);
    }
    t.send(&ps[0], json!({"t": "pick", "id": 900000001}));
    for p in &ps {
        p.take();
    }
    t.send(&ann, json!({"t": "unserve"}));
    for p in &ps {
        let g = last_games(&p.texts()).cloned().unwrap();
        assert_eq!(ids_of(&g), [900000002], "only Ben's game is left: {g}");
        assert_eq!(g["seat"], 0);
    }
    assert_eq!(t.relay.status()["hosted"], 1);
    assert_eq!(t.relay.status()["queued"], 3);
}

/// A host whose game freezes (no pings) is closed after the idle time; its game disappears.
#[test]
fn a_silent_host_is_closed_and_its_game_disappears() {
    let mut t = T::new(Config::default());
    let ann = t.host_approving("Ann", 2, "82.0.0.1");
    let pia = t.chooser_at("Pia", "82.0.0.2");
    t.quick_match(&pia);
    t.send(&pia, json!({"t": "pick", "id": 900000001}));
    pia.take();
    wait_with(&mut t, 90_000, &[&pia]);
    assert_eq!(ann.closed(), Some(4001));
    let g = last_games(&pia.texts()).cloned().unwrap();
    assert_eq!(g["seat"], 0);
    assert!(ids_of(&g).is_empty());
    assert_eq!(t.relay.status()["hosted"], 0);
}

/// Hosting again (a new room, like --host=auto after a match or F5 after Esc) replaces the
/// waiting game: a chooser seated in it chooses again, an older game's player is seated in the
/// new one.
#[test]
fn hosting_again_replaces_the_waiting_game() {
    let mut t = T::new(Config::default());
    let ann = t.host_approving("Ann", 2, "82.0.0.1");
    let pia = t.chooser_at("Pia", "82.0.0.2");
    t.quick_match(&pia);
    t.send(&pia, json!({"t": "pick", "id": 900000001}));
    let quinn = t.player_at("Quinn", "82.0.0.4");
    t.quick_match(&quinn);
    assert_eq!(seats(&ann.texts()), Some(vec!["Pia", "Quinn"]));
    t.send(&ann, json!({"t": "serve", "room": "HOST_Ann_2", "players": 2, "name": "Ann", "hero": 7, "approve": true}));
    let texts = ann.texts();
    assert!(texts.contains(&json!({"t": "serving", "room": "HOST_Ann_2"})));
    assert_eq!(seats(&texts), Some(vec!["Quinn"]));
    let g = last_games(&pia.texts()).cloned().unwrap();
    assert_eq!(ids_of(&g), [900000002]);
    assert_eq!(g["seat"], 0, "Pia chooses again");
    let mut ql = Vec::new();
    waiting_list(&quinn.msgs(), &mut ql);
    assert_eq!(names(&ql), ["Ann"]);
    assert_eq!(ql[0].0, "900000002");
    assert_eq!(t.relay.status()["hosted"], 1);
}

/// Two hosts with the same nickname pick the same room name: the second is refused and its game
/// is not listed (its engine shows nothing for "taken": it waits in vain).
#[test]
fn a_second_host_with_the_same_room_is_refused_and_not_listed() {
    let mut t = T::new(Config::default());
    let first = t.host_approving("Ann", 2, "82.0.0.1");
    let second = t.conn_from(Some("82.0.0.9"));
    t.send(&second, json!({"t": "hello", "v": 1, "name": "RGHost"}));
    second.take();
    t.send(&second, json!({"t": "serve", "room": "HOST_Ann_1", "players": 3, "name": "Ann", "hero": 1, "approve": true}));
    let texts = second.texts();
    assert_eq!(texts[0]["code"], "taken", "{texts:?}");
    assert_eq!(t.relay.status()["hosted"], 1);
    let pia = t.chooser_at("Pia", "82.0.0.2");
    t.quick_match(&pia);
    let g = last_games(&pia.texts()).cloned().unwrap();
    assert_eq!(g["games"], json!([[900000001, "Ann", 7, 0, 2, 0, 1]]));
    drop(first);
}

/// `serve` with a player count the casino does not take serves the room but lists no game.
#[test]
fn odd_player_counts_are_not_listed() {
    let mut t = T::new(Config::default());
    let pia = t.chooser_at("Pia", "82.0.0.2");
    t.quick_match(&pia);
    pia.take();
    for (i, n) in [json!(0), json!(-1), json!(2.5), json!(100), json!("3"), json!(null)].into_iter().enumerate() {
        let h = t.conn_from(Some("82.0.0.1"));
        t.send(&h, json!({"t": "hello", "v": 1, "name": "RGHost"}));
        let room = format!("HOST_Odd{i}_1");
        t.send(&h, json!({"t": "serve", "room": room, "players": n, "name": "Odd", "approve": true}));
        let texts = h.texts();
        assert!(texts.contains(&json!({"t": "serving", "room": room})), "{texts:?}");
    }
    assert_eq!(t.relay.status()["hosted"], 0);
    assert!(pia.texts().is_empty(), "nothing offered");
}

/// Heroes outside the engine's ten show as random (-1, "PLUMBER" + random), a name that is not a
/// valid nickname falls back to the room's.
#[test]
fn odd_heroes_and_names_show_as_random_and_the_rooms_name() {
    let mut t = T::new(Config::default());
    let h = t.conn_from(Some("82.0.0.1"));
    t.send(&h, json!({"t": "hello", "v": 1, "name": "RGHost"}));
    t.send(&h, json!({"t": "serve", "room": "HOST_Zed_4", "players": 2, "name": "bad name!", "hero": 10, "approve": true}));
    let h2 = t.conn_from(Some("82.0.0.5"));
    t.send(&h2, json!({"t": "hello", "v": 1, "name": "RGHost"}));
    t.send(&h2, json!({"t": "serve", "room": "HOST_Kim_1", "players": 2, "name": "Kim", "hero": -3}));
    let pia = t.chooser_at("Pia", "82.0.0.2");
    t.quick_match(&pia);
    let g = last_games(&pia.texts()).cloned().unwrap();
    assert_eq!(g["games"], json!([[900000002, "Kim", -1, 0, 2, 0, 0], [900000001, "Zed", -1, 0, 2, 0, 1]]));
    t.send(&pia, json!({"t": "pick", "id": 900000002}));
    let (_, m) = pia.split();
    let host = m.iter().find(|m| m.1 == EVENT && m.2 == 9).map(|m| {
        let mut r = Reader::new(&m.3);
        r.str().unwrap();
        r.kv().unwrap()
    });
    let host = host.expect("Kim is listed");
    assert!(host.contains(&("heroType".into(), "PLUMBER".into())));
    assert!(host.contains(&("random".into(), "TRUE".into())));
}

/// A pick of a game that is gone, unknown or nonsense sends the current list again; a pick of
/// the game the player sits in changes nothing.
#[test]
fn a_pick_of_a_gone_or_unknown_game_sends_the_list_again() {
    let mut t = T::new(Config::default());
    let ann = t.host_approving("Ann", 2, "82.0.0.1");
    t.tick(1_000);
    let _ben = t.host_approving("Ben", 2, "82.0.0.5");
    let pia = t.chooser_at("Pia", "82.0.0.2");
    t.quick_match(&pia);
    t.send(&pia, json!({"t": "pick", "id": 900000002}));
    pia.take();
    t.send(&ann, json!({"t": "unserve"}));
    pia.take();
    for id in [json!(900000001), json!(900000099), json!(1)] {
        t.send(&pia, json!({"t": "pick", "id": id}));
        let g = last_games(&pia.texts()).cloned().expect("sent again");
        assert_eq!(g["seat"], 900000002, "still in Ben's game");
        assert_eq!(ids_of(&g), [900000002]);
    }
    t.send(&pia, json!({"t": "pick", "id": 900000002}));
    assert!(pia.take().is_empty(), "already there: nothing");
    for id in [json!(-1), json!("900000002"), json!(4294967296u64 + 900000002), json!(null)] {
        t.send(&pia, json!({"t": "pick", "id": id}));
    }
    assert!(pia.texts().is_empty(), "ignored");
    assert_eq!(t.relay.status()["queued"], 1);
}

/// A pick from a player who is not in Quick Match (in the casino, before Quick Match or after
/// Back) seats nobody.
#[test]
fn a_pick_outside_quick_match_seats_nobody() {
    let mut t = T::new(Config::default());
    let ann = t.host_approving("Ann", 1, "82.0.0.1");
    let pia = t.chooser_at("Pia", "82.0.0.2");
    t.send(&pia, json!({"t": "pick", "id": 900000001}));
    t.quick_match(&pia);
    t.cancel(&pia);
    pia.take();
    t.send(&pia, json!({"t": "pick", "id": 900000001}));
    assert!(pia.take().is_empty());
    assert!(ann.texts().is_empty(), "no seats change");
    assert_eq!(t.relay.status()["queued"], 0);
}

/// `start` and `kick` from a player (not a host) are ignored.
#[test]
fn start_and_kick_from_a_player_are_ignored() {
    let mut t = T::new(Config::default());
    let ann = t.host_approving("Ann", 1, "82.0.0.1");
    let pia = t.chooser_at("Pia", "82.0.0.2");
    t.quick_match(&pia);
    t.send(&pia, json!({"t": "pick", "id": 900000001}));
    ann.take();
    pia.take();
    let rex = t.chooser_at("Rex", "82.0.0.3");
    t.send(&rex, json!({"t": "start"}));
    t.send(&rex, json!({"t": "kick", "name": "Pia"}));
    t.send(&pia, json!({"t": "start"}));
    assert!(!got_lock(&pia.msgs()));
    assert!(ann.texts().is_empty());
    assert!(rex.texts().is_empty());
}

/// Quick Match twice in a row (no Back in between) lists the player once and counts them once.
#[test]
fn quick_match_again_while_waiting_lists_the_player_once() {
    let mut t = T::new(Config::default());
    let ann = t.host_approving("Ann", 3, "82.0.0.1");
    let pia = t.chooser_at("Pia", "82.0.0.2");
    let quinn = t.player_at("Quinn", "82.0.0.4");
    t.quick_match(&quinn);
    let mut ql = Vec::new();
    waiting_list(&quinn.msgs(), &mut ql);
    t.quick_match(&pia);
    t.send(&pia, json!({"t": "pick", "id": 900000001}));
    for _ in 0..3 {
        t.quick_match(&pia);
        t.send(&pia, json!({"t": "pick", "id": 900000001}));
        t.quick_match(&quinn);
    }
    waiting_list(&quinn.msgs(), &mut ql); // panics on an entry listed twice
    ql.sort_by(|a, b| a.1.cmp(&b.1));
    assert_eq!(names(&ql), ["Ann", "Pia"]);
    assert_eq!(seats(&ann.texts()).map(|mut s| {
        s.sort();
        s
    }), Some(vec!["Pia", "Quinn"]));
    assert_eq!(t.relay.status()["queued"], 2);
}

/// At most 20 games are offered: the player's own network's first, then the newest.
#[test]
fn twenty_games_are_offered_home_first_then_newest() {
    let mut t = T::new(Config::default());
    let mut hosts = Vec::new();
    for i in 0..25u32 {
        let addr = if i == 3 { "82.0.1.99".to_string() } else { format!("82.0.1.{}", i + 1) };
        hosts.push(t.host_approving(&format!("H{i}"), 2, &addr));
        t.tick(10);
    }
    let pia = t.chooser_at("Pia", "82.0.0.200");
    t.quick_match(&pia);
    let g = last_games(&pia.texts()).cloned().unwrap();
    let want: Vec<u64> = (6..=25).rev().map(|n| 900000000 + n).collect();
    assert_eq!(ids_of(&g), want, "the 20 newest");
    // someone from H3's network: H3's game comes first and seats them
    let rex = t.chooser_at("Rex", "82.0.1.99");
    t.quick_match(&rex);
    let g = last_games(&rex.texts()).cloned().unwrap();
    let ids = ids_of(&g);
    assert_eq!(ids.len(), 20);
    assert_eq!(ids[0], 900000004);
    assert_eq!(g["games"][0][5], 1, "home");
    assert_eq!(g["seat"], 900000004);
    assert!(ids[1..].windows(2).all(|w| w[0] > w[1]), "then newest first: {ids:?}");
}

/// A chooser seated in an older game keeps it on the list once 20 newer games wait (lobby.c finds
/// its strip by `seat` among the games).
#[test]
fn a_seated_player_always_sees_their_own_game() {
    let mut t = T::new(Config::default());
    let _ann = t.host_approving("Ann", 3, "82.0.0.1");
    let pia = t.chooser_at("Pia", "82.0.0.2");
    t.quick_match(&pia);
    t.send(&pia, json!({"t": "pick", "id": 900000001}));
    let mut hosts = Vec::new();
    for i in 0..20u32 {
        t.tick(10);
        hosts.push(t.host_approving(&format!("H{i}"), 2, &format!("82.0.1.{}", i + 1)));
    }
    let g = last_games(&pia.texts()).cloned().unwrap();
    assert_eq!(g["seat"], 900000001);
    assert!(ids_of(&g).contains(&900000001), "the player's own game is listed: {:?}", ids_of(&g));
}

/// On a LAN relay every private or loopback address is one network: the host's own game (from
/// 127.0.0.1) seats the players by itself; a player from a public address chooses.
#[test]
fn every_private_address_is_one_home_on_a_lan_relay() {
    let mut t = T::new(Config::default());
    let ann = t.host_approving("Ann", 3, "127.0.0.1");
    let ps: Vec<C> = ["192.168.0.20", "10.1.2.3", "172.16.5.5"].iter().enumerate().map(|(i, a)| t.chooser_at(&format!("Lan{i}"), a)).collect();
    for p in &ps {
        t.quick_match(p);
        assert_eq!(last_games(&p.texts()).unwrap()["seat"], 900000001, "seated by itself");
    }
    assert_eq!(seats(&ann.texts()), Some(vec!["Lan0", "Lan1", "Lan2"]));
    let out = t.chooser_at("Away", "82.0.0.7");
    t.quick_match(&out);
    let g = last_games(&out.texts()).cloned().unwrap();
    assert_eq!(g["seat"], 0);
    assert!(ids_of(&g).is_empty(), "the LAN game is full");
}

/// A connection whose address is unknown is never "at home", not even with a host whose address
/// is unknown too.
#[test]
fn a_client_without_an_address_is_never_at_home() {
    let mut t = T::new(Config::default());
    let h = t.conn();
    t.send(&h, json!({"t": "hello", "v": 1, "name": "RGHost"}));
    t.send(&h, json!({"t": "serve", "room": "HOST_Ann_1", "players": 2, "name": "Ann", "hero": 1, "approve": true}));
    let pia = t.conn();
    t.send(&pia, json!({"t": "hello", "v": 1, "name": "Pia", "pick": true}));
    t.send(&pia, json!({"t": "join", "room": LOBBY}));
    t.quick_match(&pia);
    let g = last_games(&pia.texts()).cloned().unwrap();
    assert_eq!(g["seat"], 0);
    assert_eq!(g["games"], json!([[900000001, "Ann", 1, 0, 2, 0, 1]]));
}

/// A player whose connection drops during "Game starting" is still one of the match's players
/// (the host's engine started with them and waits for them): the others get MATCHED_UP as soon as
/// they have answered, announcing the whole group, and the dropped player's next Quick Match sends
/// them into the match.
#[test]
fn a_player_who_drops_during_the_lock_goes_back_into_the_match() {
    let mut t = T::new(Config::default());
    let ann = t.host_approving("Ann", 2, "82.0.0.1");
    let pia = t.chooser_at("Pia", "82.0.0.2");
    let rex = t.chooser_at("Rex", "82.0.0.3");
    for c in [&pia, &rex] {
        t.quick_match(c);
        t.send(c, json!({"t": "pick", "id": 900000001}));
    }
    t.send(&ann, json!({"t": "start"}));
    let m = pia.msgs();
    assert!(got_lock(&m));
    assert_eq!(intros(&m), ["Rex"]);
    assert!(got_lock(&rex.msgs()));
    t.disconnect(&rex);
    assert!(rex.take().is_empty());
    t.lock_ok(&pia);
    assert_eq!(matched(&pia.msgs()), Some(("HOST_Ann_1".into(), 2)), "Rex is not waited for");
    let rex2 = t.chooser_at("Rex", "82.0.0.3");
    t.quick_match(&rex2);
    assert!(got_lock(&rex2.msgs()), "Rex goes back into the match");
    t.lock_ok(&rex2);
    assert_eq!(matched(&rex2.msgs()), Some(("HOST_Ann_1".into(), 1)));
}

/// A player who presses Back (or asks for Quick Match again) during "Game starting" came too late:
/// the match is theirs (no MATCHED_UP now, the others go on), and their next Quick Match sends them
/// into it. So does one who got MATCHED_UP but whose connection drops before reaching the room.
#[test]
fn a_player_who_backs_out_of_a_started_game_goes_back_into_it() {
    for how in ["back", "quick match", "drop before the room"] {
        let mut t = T::new(Config::default());
        let ann = t.host_approving("Ann", 2, "82.0.0.1");
        let pia = t.chooser_at("Pia", "82.0.0.2");
        let rex = t.chooser_at("Rex", "82.0.0.3");
        for c in [&pia, &rex] {
            t.quick_match(c);
            t.send(c, json!({"t": "pick", "id": 900000001}));
        }
        t.send(&ann, json!({"t": "start"}));
        assert!(got_lock(&rex.msgs()));
        let rex = match how {
            "back" => {
                t.cancel(&rex);
                assert_eq!(matched(&rex.msgs()), None, "{how}");
                t.quick_match(&rex);
                rex
            }
            "quick match" => {
                t.quick_match(&rex);
                rex
            }
            _ => {
                t.lock_ok(&rex);
                t.lock_ok(&pia);
                assert_eq!(matched(&rex.msgs()), Some(("HOST_Ann_1".into(), 2)), "{how}");
                t.disconnect(&rex);
                let rex2 = t.chooser_at("Rex", "82.0.0.3");
                t.quick_match(&rex2);
                rex2
            }
        };
        assert!(got_lock(&rex.msgs()), "{how}: Rex is sent into the match");
        t.lock_ok(&rex);
        assert_eq!(matched(&rex.msgs()), Some(("HOST_Ann_1".into(), 1)), "{how}");
        t.lock_ok(&pia);
        let pm = pia.msgs();
        assert!(how == "drop before the room" || matched(&pm) == Some(("HOST_Ann_1".into(), 2)), "{how}: {:?}", matched(&pm));
        t.tick(LOCK_MS);
        assert_eq!(t.relay.status()["hosted"], 0, "{how}: the game is not offered again");
    }
}

/// A lock answer from someone who is not starting a game changes nothing.
#[test]
fn a_lock_answer_from_a_stranger_starts_nothing() {
    let mut t = T::new(Config::default());
    let ann = t.host_approving("Ann", 2, "82.0.0.1");
    let pia = t.chooser_at("Pia", "82.0.0.2");
    t.quick_match(&pia);
    t.send(&pia, json!({"t": "pick", "id": 900000001}));
    pia.take();
    t.lock_ok(&pia);
    let rex = t.chooser_at("Rex", "82.0.0.3");
    t.lock_ok(&rex);
    t.tick(LOCK_MS * 2);
    assert_eq!(matched(&pia.msgs()), None);
    assert_eq!(matched(&rex.msgs()), None);
    assert_eq!(t.relay.status()["hosted"], 1);
    drop(ann);
}

/// An older game's player sits in a game whose host starts it: it does not start by itself when
/// full, only when the host says so.
#[test]
fn an_older_player_in_a_host_started_game_waits_for_the_host() {
    let mut t = T::new(Config::default());
    let ann = t.host_approving("Ann", 1, "82.0.0.1");
    let quinn = t.player_at("Quinn", "82.0.0.4");
    t.quick_match(&quinn);
    let m = quinn.msgs();
    assert!(!got_lock(&m), "full, but the host starts it");
    assert_eq!(seats(&ann.texts()), Some(vec!["Quinn"]));
    t.send(&ann, json!({"t": "start"}));
    let m = quinn.msgs();
    assert!(got_lock(&m));
    assert!(intros(&m).is_empty(), "alone with the host");
    t.lock_ok(&quinn);
    assert_eq!(matched(&quinn.msgs()), Some(("HOST_Ann_1".into(), 1)));
}

/// A seated player who leaves frees the seat: offered to the choosers again, an older game's
/// player waiting takes it.
#[test]
fn a_freed_seat_is_offered_again_and_taken_by_a_waiting_player() {
    let mut t = T::new(Config::default());
    let ann = t.host_approving("Ann", 1, "82.0.0.1");
    let pia = t.chooser_at("Pia", "82.0.0.2");
    t.quick_match(&pia);
    t.send(&pia, json!({"t": "pick", "id": 900000001}));
    let rex = t.chooser_at("Rex", "82.0.0.3");
    t.quick_match(&rex);
    assert!(ids_of(last_games(&rex.texts()).unwrap()).is_empty());
    t.disconnect(&pia);
    assert_eq!(seats(&ann.texts()), Some(vec![]));
    assert_eq!(ids_of(last_games(&rex.texts()).unwrap()), [900000001]);
    let quinn = t.player_at("Quinn", "82.0.0.4");
    t.quick_match(&quinn);
    assert_eq!(seats(&ann.texts()), Some(vec!["Quinn"]));
    assert!(ids_of(last_games(&rex.texts()).unwrap()).is_empty(), "full again");
}

/// Into a running hosted match with a player who chooses; their connection drops.
fn running_match_with_a_chooser(t: &mut T) -> (C, C) {
    let ann = t.host_approving("Ann", 1, "82.0.0.1");
    let pia = t.chooser_at("Pia", "82.0.0.2");
    t.quick_match(&pia);
    t.send(&pia, json!({"t": "pick", "id": 900000001}));
    t.send(&ann, json!({"t": "start"}));
    t.lock_ok(&pia);
    assert_eq!(matched(&pia.msgs()).map(|m| m.0).as_deref(), Some("HOST_Ann_1"));
    t.send(&pia, json!({"t": "move", "room": "HOST_Ann_1"}));
    seat_take(t, &pia);
    assert!(ann.texts().contains(&json!({"t": "player_joined", "pid": 1, "name": "Pia"})));
    t.disconnect(&pia);
    assert!(ann.texts().contains(&json!({"t": "player_left", "pid": 1, "lost": 1})));
    (ann, pia)
}

/// A chooser whose connection dropped goes straight back into the match, without a list, even
/// when a game at home is waiting.
#[test]
fn a_chooser_who_dropped_goes_back_into_the_match_without_choosing() {
    let mut t = T::new(Config::default());
    let (ann, _old) = running_match_with_a_chooser(&mut t);
    let _sis = t.host_approving("Sis", 2, "82.0.0.2"); // Pia's home hosts meanwhile
    let pia = t.chooser_at("Pia", "82.0.0.2");
    t.quick_match(&pia);
    let (texts, m) = pia.split();
    assert!(texts.iter().all(|m| m["t"] != "games" || m["on"] == false), "no list: {texts:?}");
    assert!(got_lock(&m));
    t.lock_ok(&pia);
    assert_eq!(matched(&pia.msgs()), Some(("HOST_Ann_1".into(), 1)));
    t.send(&pia, json!({"t": "move", "room": "HOST_Ann_1"}));
    seat_take(&mut t, &pia);
    assert!(ann.texts().contains(&json!({"t": "player_joined", "pid": 2, "name": "Pia"})));
    // once back, the next Quick Match is an ordinary one again
    t.send(&pia, json!({"t": "move", "room": LOBBY}));
    pia.take();
    t.quick_match(&pia);
    assert_eq!(last_games(&pia.texts()).unwrap()["seat"], 900000002, "Sis's game at home");
}

/// Hosting again (--host=auto after the match) ends the old match: a player who dropped out of
/// it no longer goes back.
#[test]
fn hosting_again_ends_the_way_back_into_the_old_match() {
    let mut t = T::new(Config::default());
    let (ann, _old) = running_match_with_a_chooser(&mut t);
    t.send(&ann, json!({"t": "serve", "room": "HOST_Ann_2", "players": 1, "name": "Ann", "hero": 7, "approve": true}));
    let pia = t.chooser_at("Pia", "82.0.0.2");
    t.quick_match(&pia);
    let (texts, m) = pia.split();
    assert!(!got_lock(&m), "no way back into a match that is over");
    assert_eq!(ids_of(last_games(&texts).unwrap()), [900000002], "Ann's new game is offered");
}

/// A host who stops (Esc, crash, its next game) during the lock window ("Game starting", up to
/// 3 s) gets no one sent into its room: its players' connections are closed instead (their game
/// says so) of leaving them in a room with no game server.
#[test]
fn no_one_is_matched_into_the_room_of_a_host_who_left_during_the_lock() {
    for how in ["unserve", "disconnect", "host again"] {
        let mut t = T::new(Config::default());
        let ann = t.host_approving("Ann", 1, "82.0.0.1");
        let pia = t.chooser_at("Pia", "82.0.0.2");
        t.quick_match(&pia);
        t.send(&pia, json!({"t": "pick", "id": 900000001}));
        t.send(&ann, json!({"t": "start"}));
        assert!(got_lock(&pia.msgs()));
        match how {
            "unserve" => t.send(&ann, json!({"t": "unserve"})),
            "disconnect" => t.disconnect(&ann),
            _ => t.send(&ann, json!({"t": "serve", "room": "HOST_Ann_2", "players": 1, "name": "Ann", "approve": true})),
        }
        t.lock_ok(&pia);
        t.tick(LOCK_MS);
        assert_eq!(pia.closed(), Some(4003), "{how}");
        assert_eq!(matched(&pia.msgs()), None, "{how}: Pia is not sent into the dead room");
    }
}

/// A player is seated in the game from their own network when its last seat frees up because its
/// occupant moved to a game from *their* network in the same pass (`Casino::place` goes round
/// again until nobody moves).
#[test]
fn a_home_seat_freed_in_the_same_pass_still_seats_the_player() {
    let mut t = T::new(Config::default());
    let ann = t.host_approving("Ann", 1, "82.0.0.1"); // Pia's home
    let quinn = t.player_at("Quinn", "82.0.0.2"); // an older game from elsewhere: seated at Ann's
    t.quick_match(&quinn);
    assert_eq!(seats(&ann.texts()), Some(vec!["Quinn"]));
    let pia = t.chooser_at("Pia", "82.0.0.1");
    t.quick_match(&pia);
    assert_eq!(last_games(&pia.texts()).unwrap()["seat"], 0, "home game full");
    let _qhome = t.host_approving("Qhome", 1, "82.0.0.2"); // Quinn's home hosts: Quinn moves there
    assert_eq!(seats(&ann.texts()), Some(vec!["Pia"]), "Pia takes the seat at home");
    assert_eq!(last_games(&pia.texts()).unwrap()["seat"], 900000001);
}

/// The same for an older game's player seated elsewhere (in an older game hosted before the one
/// at home).
#[test]
fn a_home_seat_freed_in_the_same_pass_moves_an_older_player_home() {
    let mut t = T::new(Config::default());
    let _far = t.host_approving("Far", 2, "82.0.0.3");
    t.tick(1_000);
    let ann = t.host_approving("Ann", 1, "82.0.0.1"); // Ola's home
    let quinn = t.player_at("Quinn", "82.0.0.2");
    t.quick_match(&quinn); // the newest: Ann's
    let ola = t.player_at("Ola", "82.0.0.1");
    t.quick_match(&ola); // home full: Far's
    assert_eq!(seats(&ann.texts()), Some(vec!["Quinn"]));
    let _qhome = t.host_approving("Qhome", 1, "82.0.0.2");
    assert_eq!(seats(&ann.texts()), Some(vec!["Ola"]), "Ola moves home");
}

/// A connection the relay closes (idle, replaced) gets nothing after its Close (the casino tidies
/// up its lists while it leaves), nor does one that is dropping.
#[test]
fn nothing_is_sent_to_a_connection_after_its_close() {
    let mut t = T::new(Config::default());
    let ann = t.host_approving("Ann", 2, "82.0.0.1");
    let pia = t.chooser_at("Pia", "82.0.0.2");
    t.quick_match(&pia);
    pia.take();
    wait_with(&mut t, 90_000, &[&ann]);
    let outs = pia.take();
    let close = outs.iter().position(|o| matches!(o, Out::Close(4001, _))).expect("closed as idle");
    assert_eq!(&outs[close + 1..], &[], "nothing after the Close");
    let rex = t.chooser_at("Rex", "82.0.0.3");
    t.quick_match(&rex);
    rex.take();
    t.disconnect(&rex);
    assert_eq!(rex.take(), vec![], "nothing while the connection is dropped");
}

/// Old kicks, Backs and dropped records do not grow without bound in a long-running relay:
/// the waiting list and games list still work after many players came and went.
#[test]
fn a_busy_relay_keeps_working_after_many_kicks_and_backs() {
    let mut t = T::new(Config::default());
    let ann = t.host_approving("Ann", 3, "82.0.0.1");
    for i in 0..300u32 {
        let p = t.chooser_at(&format!("K{i}"), &format!("82.0.2.{}", i % 250 + 1));
        t.quick_match(&p);
        t.send(&p, json!({"t": "pick", "id": 900000001}));
        t.send(&ann, json!({"t": "kick", "name": format!("K{i}")}));
        t.cancel(&p);
        t.disconnect(&p);
        t.tick(5_000);
        t.send(&ann, json!({"t": "ping"}));
    }
    ann.take();
    let pia = t.chooser_at("Pia", "82.0.0.2");
    t.quick_match(&pia);
    assert_eq!(ids_of(last_games(&pia.texts()).unwrap()), [900000001]);
    assert_eq!(t.relay.status()["queued"], 1);
}

/// A running hosted match with Pia and Rex in its room; the host's link drops.
fn match_whose_host_dropped(t: &mut T) -> (C, C) {
    match_whose_host_dropped_keyed(t, true)
}

fn match_whose_host_dropped_keyed(t: &mut T, keyed: bool) -> (C, C) {
    let ann = t.conn_from(Some("82.0.0.1"));
    t.send(&ann, json!({"t": "hello", "v": 1, "name": "RGHost"}));
    let mut serve = json!({"t": "serve", "room": "HOST_Ann_1", "players": 2, "name": "Ann", "hero": 7, "approve": true});
    if keyed {
        serve["key"] = json!("0123456789abcdef");
    }
    t.send(&ann, serve);
    ann.take();
    let pia = t.chooser_at("Pia", "82.0.0.2");
    let rex = t.chooser_at("Rex", "82.0.0.3");
    for c in [&pia, &rex] {
        t.quick_match(c);
        t.send(c, json!({"t": "pick", "id": 900000001}));
    }
    t.send(&ann, json!({"t": "start"}));
    for c in [&pia, &rex] {
        t.lock_ok(c);
    }
    for c in [&pia, &rex] {
        assert_eq!(matched(&c.msgs()).map(|m| m.0).as_deref(), Some("HOST_Ann_1"));
        t.send(c, json!({"t": "move", "room": "HOST_Ann_1"}));
        seat_take(t, c);
    }
    assert_eq!(ann.texts().iter().filter(|m| m["t"] == "player_joined").count(), 2);
    t.disconnect(&ann);
    (pia, rex)
}

/// The engine connects again and serves the same room (server.c `link_up`).
fn host_back(t: &mut T) -> C {
    let ann2 = t.conn_from(Some("82.0.0.1"));
    t.send(&ann2, json!({"t": "hello", "v": 1, "name": "RGHost"}));
    t.send(&ann2, json!({"t": "serve", "room": "HOST_Ann_1", "key": "0123456789abcdef", "running": true, "held": ["Pia", "Rex"]}));
    assert!(ann2.texts().contains(&json!({"t": "serving", "room": "HOST_Ann_1"})));
    ann2
}

/// An older host (no key) drops its players' sessions with its link: its match cannot go on, so
/// its next link does not get the room back, and its players are sent away after HOST_BACK_MS
/// (not back into a match whose host no longer knows them).
#[test]
fn an_older_host_whose_link_comes_back_mid_match_does_not_get_its_match_back() {
    let mut t = T::new(Config::default());
    let (pia, rex) = match_whose_host_dropped_keyed(&mut t, false);
    let ann2 = t.conn_from(Some("82.0.0.1"));
    t.send(&ann2, json!({"t": "hello", "v": 1, "name": "RGHost"}));
    t.send(&ann2, json!({"t": "serve", "room": "HOST_Ann_1", "players": 2, "name": "Ann", "hero": 7}));
    assert!(ann2.texts().iter().any(|m| m["t"] == "error" && m["code"] == "taken"));
    assert_eq!((pia.closed(), rex.closed()), (None, None));
    t.tick(16_000);
    assert_eq!((pia.closed(), rex.closed()), (Some(4003), Some(4003)));
    let pia2 = t.chooser_at("Pia", "82.0.0.2");
    t.quick_match(&pia2);
    assert!(!got_lock(&pia2.msgs()), "not back into the old match");
}

/// The relay restarted (or the host came back too late): a running serve names the players whose
/// seats the host holds, and they go back in with Quick Match; one already waiting is sent at once.
#[test]
fn a_running_serve_the_relay_does_not_know_takes_its_held_players_back() {
    let mut t = T::new(Config::default());
    let rex = t.chooser_at("Rex", "82.0.0.3");
    t.quick_match(&rex);
    let ann = t.conn_from(Some("82.0.0.1"));
    t.send(&ann, json!({"t": "hello", "v": 1, "name": "RGHost"}));
    t.send(&ann, json!({"t": "serve", "room": "HOST_Ann_7", "key": "0123456789abcdef", "running": true, "held": ["Pia", "Rex"]}));
    assert!(ann.texts().contains(&json!({"t": "serving", "room": "HOST_Ann_7"})));
    assert!(got_lock(&rex.msgs()), "Rex waited in Quick Match: back in at once");
    let pia = t.chooser_at("Pia", "82.0.0.2");
    t.quick_match(&pia);
    assert!(got_lock(&pia.msgs()), "Pia goes back in");
    t.lock_ok(&pia);
    assert_eq!(matched(&pia.msgs()).map(|m| m.0).as_deref(), Some("HOST_Ann_7"));
    let sue = t.chooser_at("Sue", "82.0.0.4");
    t.quick_match(&sue);
    assert!(!got_lock(&sue.msgs()), "not a player of that match");
    assert!(last_games(&sue.texts()).is_none_or(|g| ids_of(g).is_empty()), "the running match is not offered");
}

/// The host's link comes back mid-match: the match goes on through the new connection, which
/// lost the players' sessions with the old one: the players are disconnected and go back in with
/// Quick Match. The running match is never offered in Quick Match.
#[test]
fn a_host_whose_link_comes_back_mid_match_keeps_its_match() {
    let mut t = T::new(Config::default());
    let (pia, rex) = match_whose_host_dropped(&mut t);
    rex.take();
    t.bin(&pia, &frame(CH_GAME, &[0, 1, 2, 3]));
    assert!(rex.take().is_empty(), "nothing between the players while the host is away");
    let ann2 = host_back(&mut t);
    assert_eq!((pia.closed(), rex.closed()), (Some(4004), Some(4004)));
    assert_eq!(t.relay.status()["hosted"], 0);
    let pia2 = t.chooser_at("Pia", "82.0.0.2");
    t.quick_match(&pia2);
    assert!(got_lock(&pia2.msgs()));
    t.lock_ok(&pia2);
    assert_eq!(matched(&pia2.msgs()), Some(("HOST_Ann_1".into(), 1)));
    t.send(&pia2, json!({"t": "move", "room": "HOST_Ann_1"}));
    seat_take(&mut t, &pia2);
    assert!(ann2.texts().iter().any(|m| m["t"] == "player_joined" && m["name"] == "Pia"));
    t.bin(&pia2, &frame(CH_GAME, &[0, 1, 2, 3]));
    assert!(!ann2.take().is_empty(), "Pia's data reaches the host again");
    let sue = t.chooser_at("Sue", "82.0.0.4");
    t.quick_match(&sue);
    assert!(ids_of(last_games(&sue.texts()).unwrap()).is_empty(), "not offered");
}

/// A player whose connection dropped while the host's link was away goes back into the match once
/// the host is back (not while it is away: nobody serves the match then).
#[test]
fn a_dropped_player_goes_back_into_a_match_whose_host_reconnected() {
    let mut t = T::new(Config::default());
    let (_pia, rex) = match_whose_host_dropped(&mut t);
    t.disconnect(&rex);
    let rex2 = t.chooser_at("Rex", "82.0.0.3");
    t.quick_match(&rex2);
    assert!(!got_lock(&rex2.msgs()), "the host is away");
    let ann2 = host_back(&mut t);
    t.cancel(&rex2);
    t.quick_match(&rex2);
    assert!(got_lock(&rex2.msgs()), "Rex goes back into the match");
    t.lock_ok(&rex2);
    t.send(&rex2, json!({"t": "move", "room": "HOST_Ann_1"}));
    seat_take(&mut t, &rex2);
    assert!(ann2.texts().iter().any(|m| m["t"] == "player_joined" && m["name"] == "Rex"));
}

// ============================================================================ the randomized run

/// Small xorshift64 PRNG: deterministic, no dependencies.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        let mut r = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ 0x2545_f491_4f6c_dd1d);
        for _ in 0..4 {
            r.next();
        }
        r
    }
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
    fn chance(&mut self, pct: u64) -> bool {
        self.next() % 100 < pct
    }
}

/// Homes the clients connect from: four public IPv4 homes, a LAN (private, loopback and
/// Tailscale addresses: one network), two IPv6 /64s, and no address at all.
const LAN: usize = 4;
const NOWHERE: usize = 7;
/// How long the relay holds a started match whose host left (relay.rs HOST_BACK_MS).
const HOST_BACK: u64 = 15_000;

fn addr_in(home: usize, k: usize) -> Option<String> {
    match home {
        0..=3 => Some(format!("82.0.0.{}", home + 1)),
        LAN => Some(match k % 4 {
            0 => "127.0.0.1".to_string(),
            1 => format!("192.168.0.{}", 10 + k % 200),
            2 => format!("10.0.{}.{}", k % 7, 1 + k % 250),
            _ => format!("100.{}.{}.{}", 64 + k % 64, k % 7, 1 + k % 250),
        }),
        5 => Some(format!("2a02:1:2:3::{:x}", k + 1)),
        6 => Some(format!("2a02:1:2:4:{:x}::1", k + 1)),
        _ => None,
    }
}

fn same_home(a: usize, b: usize) -> bool {
    a == b && a != NOWHERE
}

/// One step of the run. Slots are players (`P<n>`) and hosts (`H<n>`); a step that does not fit
/// the slot's state (after minimizing) does nothing.
#[derive(Clone, Copy, Debug)]
enum Act {
    Connect(usize),
    QuickMatch(usize),
    Back(usize),
    /// The nth game of the player's list.
    Pick(usize, usize),
    /// Some id: a gone game, an unknown one, the player's own, a player id.
    PickId(usize, u32),
    LockOk(usize),
    EnterRoom(usize),
    ToLobby(usize),
    Drop(usize),
    Silent(usize),
    /// Host slot, kind (0: the host starts it, 1: older host with name and hero, 2: oldest
    /// host), seats, hero index.
    Serve(usize, u8, usize, i64),
    Start(usize),
    /// Host slot, the nth seated name, in upper case.
    Kick(usize, usize, bool),
    KickNobody(usize, usize),
    Unserve(usize),
    HostDrop(usize),
    HostSilent(usize),
    /// The host's game comes back on a new link (its old one still there or gone).
    HostRelink(usize),
    Tick(u64),
    /// Time passes in 20 s steps; the clients ping meanwhile.
    Wait(u64),
    /// Protocol abuse: player slot, which message.
    Garbage(usize, u8),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum St {
    Lobby,
    Waiting,
    Locked,
    Matched,
    InRoom,
}

/// A player's live connection, and what the relay told it.
struct Pc {
    c: C,
    st: St,
    /// The "Players in Match" list: (profile id, name, hero).
    list: Vec<(String, String, String)>,
    /// The last `games` message while on.
    games: Option<Value>,
    got_games: bool,
    hero: String,
    launch: Option<usize>,
    room: Option<String>,
    matched_at: u64,
    intros: Vec<String>,
    /// The game Quick Match must send it back into.
    rejoin: Option<usize>,
    /// The started game it was sent into (MATCHED_UP) and has not entered yet.
    arriving: Option<usize>,
    silent: bool,
    seen: u64,
}

struct Player {
    name: String,
    home: usize,
    addr: Option<String>,
    picks: bool,
    pc: Option<Pc>,
}

struct Hc {
    c: C,
    seats: Option<Value>,
    game: Option<usize>,
    silent: bool,
    seen: u64,
}

struct Host {
    nick: String,
    home: usize,
    addr: Option<String>,
    hc: Option<Hc>,
    served: usize,
    /// Its hosting session's key (the games that start when the host says so send it).
    key: String,
    /// The game it served last.
    last: Option<usize>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Phase {
    Waiting,
    Starting,
    Running,
    Over,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum RoomSt {
    None,
    Open,
    Removed,
}

struct Game {
    id: u32,
    host: usize,
    room: String,
    nick: String,
    seats: usize,
    approve: bool,
    hero: i64,
    home: usize,
    phase: Phase,
    room_st: RoomSt,
    in_room: BTreeSet<u64>,
    /// When its host stopped serving it after it started (the relay holds it HOST_BACK, then
    /// sends its players away and the game is Over).
    gone_at: Option<u64>,
    /// Served with the host's key; its host's link was lost (not `unserve`); when the host's game
    /// last came back on a new link (the players in the room are disconnected then).
    keyed: bool,
    lost: bool,
    relinked_at: Option<u64>,
}

/// A host sent a player away: none of that host's games takes the name, nor the address (the
/// player's home, unless it is the host's) for 10 minutes. Players from that home already seated
/// in the game stay.
struct KickRec {
    host: String,
    name: String,
    home: Option<usize>,
    at: u64,
    game: usize,
    exempt: Vec<String>,
}

/// A group locked into a game (LOCK_PLAYER_IN_GAME), waiting for MATCHED_UP.
struct Launch {
    game: usize,
    rejoin: bool,
    members: Vec<u64>,
    names: Vec<String>,
    intros: Vec<Vec<String>>,
    answered: BTreeSet<u64>,
    deadline: u64,
    fresh: bool,
    done: bool,
    matched: Vec<(u64, u32)>,
}

/// How much of the relay a run went through (so that a run which never gets anywhere fails).
#[derive(Clone, Debug, Default)]
struct Stats {
    steps: usize,
    /// Steps with more than 20 games waiting for players.
    crowded: usize,
    picks: usize,
    starts: usize,
    rejoins: usize,
    matched: usize,
    rooms: usize,
    kicks: usize,
    /// Steps at which a waiting player's kick from a waiting game was older than 10 minutes.
    expired_kicks: usize,
    closed_by_relay: usize,
    /// Players disconnected because their match's host left (4003).
    sent_away: usize,
    /// Hosts' games back on a new link, and the players it disconnected from their room (4004).
    relinks: usize,
    taken_back: usize,
}

impl Stats {
    fn add(&mut self, o: &Stats) {
        self.steps += o.steps;
        self.crowded += o.crowded;
        self.picks += o.picks;
        self.starts += o.starts;
        self.rejoins += o.rejoins;
        self.matched += o.matched;
        self.rooms += o.rooms;
        self.kicks += o.kicks;
        self.expired_kicks += o.expired_kicks;
        self.closed_by_relay += o.closed_by_relay;
        self.sent_away += o.sent_away;
        self.relinks += o.relinks;
        self.taken_back += o.taken_back;
    }
}

#[derive(Clone, Debug)]
struct Profile {
    name: &'static str,
    players: usize,
    hosts: usize,
    /// Of the client steps, how many are a host's.
    host_pct: usize,
    chooser_pct: u64,
    approve_pct: u64,
    named_pct: u64,
    homes: Vec<usize>,
    max_seats: usize,
    tick_pct: usize,
    wait_pct: usize,
    /// Of the waits, how many go past the 10 minutes of a kick.
    long_pct: u64,
    kick_pct: usize,
    abuse: bool,
    /// Everything that needs the model (off for protocol abuse: only safety).
    model: bool,
}

impl Profile {
    fn mixed() -> Self {
        Profile {
            name: "mixed",
            players: 9,
            hosts: 4,
            host_pct: 35,
            chooser_pct: 60,
            approve_pct: 60,
            named_pct: 70,
            homes: vec![0, 1, 2, 3, LAN, 5, 6, NOWHERE],
            max_seats: 3,
            tick_pct: 12,
            wait_pct: 3,
            long_pct: 25,
            kick_pct: 12,
            abuse: false,
            model: true,
        }
    }
}

#[derive(Debug)]
struct Failure {
    kind: &'static str,
    msg: String,
}

fn fail<X>(kind: &'static str, msg: String) -> Result<X, Failure> {
    Err(Failure { kind, msg })
}

macro_rules! ensure {
    ($cond:expr, $kind:literal, $($arg:tt)+) => {
        if !$cond {
            return fail($kind, format!($($arg)+));
        }
    };
}

fn entry(kv: &[(String, String)]) -> (String, String, String) {
    let get = |k: &str| kv.iter().find(|(key, _)| key == k).map(|(_, v)| v.clone()).unwrap_or_default();
    (get("id"), get("name"), get("heroType"))
}

fn host_entry(e: &(String, String, String)) -> Option<u32> {
    e.0.parse::<u32>().ok().filter(|&id| id > BASE)
}

/// The game a "Players in Match" list says the player sits in (its host's entry), and how many
/// host entries it has.
fn belief(list: &[(String, String, String)]) -> (Option<u32>, usize) {
    let hosts: Vec<u32> = list.iter().filter_map(host_entry).collect();
    (hosts.first().copied(), hosts.len())
}

fn hero_name(hero: i64) -> &'static str {
    usize::try_from(hero).ok().and_then(|h| HEROES.get(h).copied()).unwrap_or("PLUMBER")
}

struct World {
    t: T,
    p: Profile,
    players: Vec<Player>,
    hosts: Vec<Host>,
    games: Vec<Game>,
    launches: Vec<Launch>,
    kicks: Vec<KickRec>,
    /// Older games' players who pressed Back while seated (the relay skips those games for them).
    backed: HashSet<String>,
    /// (lower-case name, the started game they dropped out of) -> when: Quick Match sends them
    /// back while its host serves it (into the one they dropped out of last).
    rejoin: BTreeMap<(String, usize), u64>,
    dead: Vec<C>,
    started: Vec<usize>,
    expect_joined: Vec<(usize, String)>,
    serves: u32,
    stats: Stats,
}

impl World {
    fn new(p: Profile, seed: u64) -> Self {
        let mut r = Rng::new(seed ^ 0x5eed);
        let players = (0..p.players)
            .map(|i| {
                let home = p.homes[r.below(p.homes.len())];
                Player { name: format!("P{i}"), home, addr: addr_in(home, i), picks: r.chance(p.chooser_pct), pc: None }
            })
            .collect();
        let hosts = (0..p.hosts)
            .map(|i| {
                let home = p.homes[r.below(p.homes.len())];
                Host { nick: format!("H{i}"), home, addr: addr_in(home, 100 + i), hc: None, served: 0, key: format!("k{seed:x}h{i}"), last: None }
            })
            .collect();
        World {
            t: T::new(Config::default()),
            p,
            players,
            hosts,
            games: Vec::new(),
            launches: Vec::new(),
            kicks: Vec::new(),
            backed: HashSet::new(),
            rejoin: BTreeMap::new(),
            dead: Vec::new(),
            started: Vec::new(),
            expect_joined: Vec::new(),
            serves: 0,
            stats: Stats::default(),
        }
    }

    fn now(&self) -> u64 {
        self.t.now
    }

    fn game_by_id(&self, id: u64) -> Option<usize> {
        let i = id.checked_sub(BASE as u64 + 1)? as usize;
        (i < self.games.len()).then_some(i)
    }

    /// The kicks of this game's host that keep this player away: by name, or by address (`home`).
    fn kicks_of<'a>(&'a self, pl: &'a Player, g: usize) -> impl Iterator<Item = &'a KickRec> + 'a {
        let (host, name) = (self.games[g].nick.to_lowercase(), pl.name.to_lowercase());
        self.kicks.iter().filter(move |k| {
            k.host == host && (k.name == name || k.home.is_some_and(|h| pl.addr.is_some() && same_home(h, pl.home)))
        })
    }

    /// Has this game's host sent the player away in the last 10 minutes?
    fn kicked(&self, pl: &Player, g: usize) -> bool {
        let now = self.now();
        self.kicks_of(pl, g).any(|k| now < k.at + TEN_MIN)
    }

    /// Must the player not sit in this game? Kicked, unless only by address while already seated
    /// in it.
    fn kept_out(&self, pl: &Player, g: usize) -> bool {
        let now = self.now();
        let name = pl.name.to_lowercase();
        self.kicks_of(pl, g).any(|k| now < k.at + TEN_MIN && (k.name == name || !(k.game == g && k.exempt.contains(&name))))
    }

    fn kicked_ever(&self, pl: &Player, g: usize) -> bool {
        self.kicks_of(pl, g).next().is_some()
    }

    /// Started and not over: the relay's `running` (a player lost on the way may go back in).
    fn running(&self, g: usize) -> bool {
        matches!(self.games[g].phase, Phase::Starting | Phase::Running)
    }

    /// Started, and its host serves it: Quick Match sends dropped players back now.
    fn served(&self, g: usize) -> bool {
        self.running(g) && self.games[g].gone_at.is_none()
    }

    fn state(&self) -> String {
        let mut s = String::new();
        for pl in &self.players {
            if let Some(pc) = &pl.pc {
                let (b, _) = belief(&pc.list);
                let seat = pc.games.as_ref().map(|g| g["seat"].clone());
                s += &format!("  {} home {} picks {} {:?} in {b:?} seat {seat:?} list {:?}\n", pl.name, pl.home, pl.picks, pc.st, pc.list);
            }
        }
        for (i, g) in self.games.iter().enumerate().filter(|(_, g)| g.phase != Phase::Over) {
            let (id, room, home, seats, approve, phase, gone) = (g.id, &g.room, g.home, g.seats, g.approve, g.phase, g.gone_at);
            s += &format!("  game {i} {id} {room} home {home} seats {seats} approve {approve} {phase:?} host left {gone:?}\n");
        }
        s
    }

    // ------------------------------------------------------------------ generating steps

    fn gen(&self, r: &mut Rng) -> Act {
        let roll = r.below(100);
        if roll < self.p.tick_pct {
            return Act::Tick(100 + r.below(3_900) as u64);
        }
        if roll < self.p.tick_pct + self.p.wait_pct {
            let ms = if r.chance(self.p.long_pct) { TEN_MIN - 30_000 + r.below(90_000) as u64 } else { 20_000 + r.below(100_000) as u64 };
            return Act::Wait(ms);
        }
        if self.p.abuse && r.chance(12) {
            return Act::Garbage(r.below(self.p.players), r.below(256) as u8);
        }
        if r.below(100) >= self.p.host_pct {
            self.player_act(r.below(self.p.players), r)
        } else {
            self.host_act(r.below(self.p.hosts), r)
        }
    }

    fn player_act(&self, p: usize, r: &mut Rng) -> Act {
        let pl = &self.players[p];
        let Some(pc) = &pl.pc else { return Act::Connect(p) };
        let x = r.below(100);
        if pc.silent {
            return if x < 15 { Act::Drop(p) } else { Act::Tick(500) };
        }
        match pc.st {
            St::Lobby => match x {
                0..=74 => Act::QuickMatch(p),
                75..=84 => Act::Drop(p),
                85..=86 => Act::Silent(p),
                _ => Act::Tick(300),
            },
            St::Waiting => match x {
                0..=39 if pl.picks => Act::Pick(p, r.below(20)),
                0..=44 if pl.picks => Act::PickId(p, r.next() as u32),
                0..=44 => Act::Tick(200),
                45..=69 => Act::Back(p),
                70..=74 => Act::QuickMatch(p),
                75..=84 => Act::Drop(p),
                85..=86 => Act::Silent(p),
                _ => Act::Tick(200),
            },
            St::Locked => match x {
                0..=69 => Act::LockOk(p),
                70..=79 => Act::Drop(p),
                _ => Act::Tick(500),
            },
            St::Matched => match x {
                0..=69 => Act::EnterRoom(p),
                70..=79 => Act::ToLobby(p),
                80..=84 => Act::QuickMatch(p),
                _ => Act::Drop(p),
            },
            St::InRoom => match x {
                0..=39 => Act::ToLobby(p),
                40..=69 => Act::Drop(p),
                _ => Act::Tick(1_000),
            },
        }
    }

    fn serve_act(&self, h: usize, r: &mut Rng) -> Act {
        let kind = if r.chance(self.p.approve_pct) {
            0
        } else if r.chance(self.p.named_pct) {
            1
        } else {
            2
        };
        Act::Serve(h, kind, 1 + r.below(self.p.max_seats), r.below(14) as i64 - 2)
    }

    fn host_act(&self, h: usize, r: &mut Rng) -> Act {
        let host = &self.hosts[h];
        let Some(hc) = &host.hc else {
            // a lost link: its game reconnects (server.c link_up) unless the host quit
            if self.relink_game(h).is_some() && r.chance(60) {
                return Act::HostRelink(h);
            }
            return self.serve_act(h, r);
        };
        let x = r.below(100);
        if x < 3 && self.relink_game(h).is_some() {
            return Act::HostRelink(h); // a new link before the relay notices the old one is gone
        }
        if hc.silent {
            return if x < 15 { Act::HostDrop(h) } else { Act::Tick(500) };
        }
        match hc.game.map(|g| self.games[g].phase) {
            None | Some(Phase::Over) => match x {
                0..=69 => self.serve_act(h, r),
                70..=79 => Act::HostDrop(h),
                80..=81 => Act::HostSilent(h),
                _ => Act::Tick(400),
            },
            Some(Phase::Waiting) => {
                let k = self.p.kick_pct;
                match x {
                    _ if x < 30 => Act::Start(h),
                    _ if x < 30 + k => Act::Kick(h, r.below(3), r.chance(30)),
                    _ if x < 33 + k => Act::KickNobody(h, r.below(self.p.players)),
                    _ if x < 41 + k => Act::Unserve(h),
                    _ if x < 47 + k => Act::HostDrop(h),
                    _ if x < 52 + k => self.serve_act(h, r),
                    _ if x < 54 + k => Act::HostSilent(h),
                    _ => Act::Tick(300),
                }
            }
            Some(_) => match x {
                0..=19 => Act::Unserve(h),
                20..=44 => self.serve_act(h, r),
                45..=49 => Act::HostDrop(h),
                50..=51 => Act::Start(h),
                _ => Act::Tick(800),
            },
        }
    }

    // ------------------------------------------------------------------ doing them

    fn qm_hero(p: usize) -> &'static str {
        HEROES[p % HEROES.len()]
    }

    fn apply(&mut self, a: Act) -> Result<(), Failure> {
        self.started.clear();
        let now = self.now();
        match a {
            Act::Connect(p) => {
                if self.players[p].pc.is_some() {
                    return Ok(());
                }
                let pl = &self.players[p];
                let c = self.t.conn_from(pl.addr.as_deref());
                let hello = if pl.picks {
                    json!({"t": "hello", "v": 1, "name": pl.name, "pick": true})
                } else {
                    json!({"t": "hello", "v": 1, "name": pl.name})
                };
                self.t.send(&c, hello);
                self.t.send(&c, json!({"t": "join", "room": LOBBY}));
                self.players[p].pc = Some(Pc {
                    c,
                    st: St::Lobby,
                    list: Vec::new(),
                    games: None,
                    got_games: false,
                    hero: Self::qm_hero(p).to_string(),
                    launch: None,
                    room: None,
                    matched_at: 0,
                    intros: Vec::new(),
                    rejoin: None,
                    arriving: None,
                    silent: false,
                    seen: now,
                });
            }
            Act::QuickMatch(p) => {
                let Some(pc) = self.live_pc(p, &[St::Lobby, St::Waiting, St::Matched]) else { return Ok(()) };
                let (c, arriving) = (pc.c.clone(), pc.arriving);
                let name = self.players[p].name.to_lowercase();
                // asked again before getting into the game it was sent to: that is the way back
                if let Some(g) = arriving.filter(|&g| self.running(g)) {
                    self.rejoin.insert((name.clone(), g), now);
                }
                // kept while its host is away (it may come back); the one they dropped out of last
                let back = self
                    .rejoin
                    .iter()
                    .filter(|((n, g), _)| *n == name && self.served(*g))
                    .max_by_key(|((_, g), t)| (**t, self.games[*g].room.to_lowercase()))
                    .map(|(r, _)| r.clone());
                if let Some(r) = &back {
                    self.rejoin.remove(r);
                }
                let pc = self.players[p].pc.as_mut().unwrap();
                pc.arriving = None;
                pc.rejoin = back.map(|r| r.1);
                qm_as(&mut self.t, &c, Self::qm_hero(p));
                self.seen_p(p);
            }
            Act::Back(p) => {
                let Some(pc) = self.live_pc(p, &[St::Waiting]) else { return Ok(()) };
                let c = pc.c.clone();
                if !self.players[p].picks && belief(&pc.list).0.is_some() {
                    self.backed.insert(self.players[p].name.to_lowercase());
                }
                self.t.cancel(&c);
                self.seen_p(p);
            }
            Act::Pick(p, n) => {
                let Some(pc) = self.live_pc(p, &[St::Waiting]) else { return Ok(()) };
                let Some(g) = &pc.games else { return Ok(()) };
                let ids = ids_of(g);
                if ids.is_empty() {
                    return Ok(());
                }
                let c = pc.c.clone();
                let id = ids[n % ids.len()];
                self.stats.picks += 1;
                self.t.send(&c, json!({"t": "pick", "id": id}));
                self.seen_p(p);
            }
            Act::PickId(p, x) => {
                let Some(pc) = self.live_pc(p, &[St::Waiting]) else { return Ok(()) };
                let c = pc.c.clone();
                let seat = pc.games.as_ref().map_or(0, |g| g["seat"].as_u64().unwrap_or(0));
                let id = match x % 4 {
                    0 if !self.games.is_empty() => self.games[x as usize / 4 % self.games.len()].id as u64,
                    1 => BASE as u64 + self.serves as u64 + 1 + (x as u64 % 50),
                    2 => seat,
                    _ => 1 + x as u64 % 20,
                };
                self.t.send(&c, json!({"t": "pick", "id": id}));
                self.seen_p(p);
            }
            Act::LockOk(p) => {
                let Some(pc) = self.live_pc(p, &[St::Locked]) else { return Ok(()) };
                let c = pc.c.clone();
                if let Some(l) = pc.launch {
                    self.launches[l].answered.insert(c.id);
                }
                self.t.lock_ok(&c);
                self.seen_p(p);
            }
            Act::EnterRoom(p) => {
                let Some(pc) = self.live_pc(p, &[St::Matched]) else { return Ok(()) };
                let (c, matched_at) = (pc.c.clone(), pc.matched_at);
                let Some(room) = pc.room.clone() else { return Ok(()) };
                let Some(g) = self.games.iter().position(|g| g.room == room) else { return Ok(()) };
                let ok = match self.games[g].room_st {
                    RoomSt::Open => true,
                    RoomSt::None => now < matched_at + 60_000,
                    RoomSt::Removed => false,
                };
                if !ok {
                    return Ok(());
                }
                self.t.send(&c, json!({"t": "move", "room": room}));
                seat_take(&mut self.t, &c);
                self.stats.rooms += 1;
                let name = self.players[p].name.to_lowercase();
                self.rejoin.remove(&(name, g)); // there again
                let game = &mut self.games[g];
                game.room_st = RoomSt::Open;
                game.in_room.insert(c.id);
                // its host hears of the player while it serves the room
                if game.phase != Phase::Over && game.gone_at.is_none() {
                    self.expect_joined.push((game.host, self.players[p].name.clone()));
                }
                let pc = self.players[p].pc.as_mut().unwrap();
                if pc.arriving == Some(g) {
                    pc.arriving = None;
                }
                pc.st = St::InRoom;
                pc.list.clear();
                self.seen_p(p);
            }
            Act::ToLobby(p) => {
                let Some(pc) = self.live_pc(p, &[St::InRoom, St::Matched]) else { return Ok(()) };
                let (c, st) = (pc.c.clone(), pc.st);
                if st == St::InRoom {
                    let name = self.players[p].name.to_lowercase();
                    if let Some(g) = self.room_of(c.id) {
                        self.rejoin.remove(&(name, g)); // left the normal way
                        self.leave_room(g, c.id);
                    }
                }
                self.t.send(&c, json!({"t": "move", "room": LOBBY}));
                let pc = self.players[p].pc.as_mut().unwrap();
                pc.st = St::Lobby;
                pc.room = None;
                pc.list.clear();
                self.seen_p(p);
            }
            Act::Drop(p) => {
                let Some(pc) = self.players[p].pc.take() else { return Ok(()) };
                self.player_gone(p, &pc);
                self.t.disconnect(&pc.c);
                let during = pc.c.take();
                ensure!(during.is_empty(), "after-close", "{} got {during:?} while dropped", self.players[p].name);
                self.dead.push(pc.c);
            }
            Act::Silent(p) => {
                if let Some(pc) = self.players[p].pc.as_mut() {
                    pc.silent = true;
                }
            }
            Act::Serve(h, kind, seats, hero) => self.serve(h, kind, seats, hero)?,
            Act::Start(h) => {
                let Some(hc) = self.live_hc(h) else { return Ok(()) };
                let (c, g) = (hc.c.clone(), hc.game);
                if let Some(g) = g {
                    self.started.push(g);
                }
                self.t.send(&c, json!({"t": "start"}));
                self.seen_h(h);
            }
            Act::Kick(h, n, upper) => {
                let Some(hc) = self.live_hc(h) else { return Ok(()) };
                let Some(names) = hc.seats.as_ref().and_then(|s| s["names"].as_array().cloned()) else { return Ok(()) };
                if names.is_empty() {
                    return Ok(());
                }
                let name = names[n % names.len()].as_str().unwrap().to_string();
                let name = if upper { name.to_uppercase() } else { name };
                self.kick(h, &name);
            }
            Act::KickNobody(h, p) => {
                if self.live_hc(h).is_none() {
                    return Ok(());
                }
                let name = if p % 3 == 0 { "Nobody".to_string() } else { self.players[p].name.clone() };
                self.kick(h, &name);
            }
            Act::Unserve(h) => {
                let Some(hc) = self.live_hc(h) else { return Ok(()) };
                let (c, g) = (hc.c.clone(), hc.game);
                if let Some(g) = g {
                    self.end_game(g, false);
                }
                self.hosts[h].hc.as_mut().unwrap().game = None;
                self.t.send(&c, json!({"t": "unserve"}));
                self.seen_h(h);
            }
            Act::HostDrop(h) => {
                let Some(hc) = self.hosts[h].hc.take() else { return Ok(()) };
                if let Some(g) = hc.game {
                    self.end_game(g, true);
                }
                self.t.disconnect(&hc.c);
                let during = hc.c.take();
                ensure!(during.is_empty(), "after-close", "host {h} got {during:?} while dropped");
                self.dead.push(hc.c);
            }
            Act::HostSilent(h) => {
                if let Some(hc) = self.hosts[h].hc.as_mut() {
                    hc.silent = true;
                }
            }
            Act::HostRelink(h) => self.relink(h)?,
            Act::Tick(ms) => self.pass(ms, ms)?,
            Act::Wait(ms) => self.pass(ms, 20_000)?,
            Act::Garbage(p, k) => self.garbage(p, k),
        }
        self.observe()?;
        self.check_end()
    }

    fn live_pc(&self, p: usize, states: &[St]) -> Option<&Pc> {
        self.players[p].pc.as_ref().filter(|pc| !pc.silent && (states.contains(&pc.st) || self.p.abuse))
    }

    fn live_hc(&self, h: usize) -> Option<&Hc> {
        self.hosts[h].hc.as_ref().filter(|hc| !hc.silent)
    }

    fn seen_p(&mut self, p: usize) {
        let now = self.now();
        if let Some(pc) = self.players[p].pc.as_mut() {
            pc.seen = now;
        }
    }

    fn seen_h(&mut self, h: usize) {
        let now = self.now();
        if let Some(hc) = self.hosts[h].hc.as_mut() {
            hc.seen = now;
        }
    }

    fn room_of(&self, conn: u64) -> Option<usize> {
        self.games.iter().position(|g| g.in_room.contains(&conn))
    }

    /// An empty room without its server is removed.
    fn leave_room(&mut self, g: usize, conn: u64) {
        let game = &mut self.games[g];
        game.in_room.remove(&conn);
        let unserved = game.phase == Phase::Over || game.gone_at.is_some();
        if game.in_room.is_empty() && unserved && game.room_st == RoomSt::Open {
            game.room_st = RoomSt::Removed;
        }
    }

    /// A player's connection is gone (dropped or closed by the relay). A player of a started game
    /// (in its room, locked into it, or sent there and not in it yet) may go back into it.
    fn player_gone(&mut self, p: usize, pc: &Pc) {
        let mut of = match pc.st {
            St::Locked => pc.launch.map(|l| self.launches[l].game),
            _ => None,
        };
        if pc.st == St::InRoom {
            if let Some(g) = self.room_of(pc.c.id) {
                of = Some(g);
                self.leave_room(g, pc.c.id);
            }
        }
        // a locked player's match runs for the relay from its start (the model may not have seen
        // the start yet: its host's stream comes after the players')
        let locked = |g: usize| pc.st == St::Locked && self.games[g].phase == Phase::Waiting;
        if let Some(g) = of.or(pc.arriving).filter(|&g| self.running(g) || locked(g)) {
            self.rejoin.insert((self.players[p].name.to_lowercase(), g), self.t.now);
        }
    }

    /// The host no longer serves this game: a waiting one is over, a started one waits HOST_BACK
    /// for its host.
    fn end_game(&mut self, g: usize, lost: bool) {
        let now = self.now();
        // a group locked into it in this observation started it (the start is settled after the
        // hosts' streams)
        let started = self.running(g) || self.launches.iter().any(|l| l.fresh && !l.rejoin && l.game == g);
        let game = &mut self.games[g];
        if game.phase == Phase::Over || game.gone_at.is_some() {
            return;
        }
        if started {
            game.gone_at = Some(now);
            game.lost = lost;
        } else {
            game.phase = Phase::Over;
        }
        if game.in_room.is_empty() && game.room_st == RoomSt::Open {
            game.room_st = RoomSt::Removed;
        }
    }

    /// Started games whose host did not come back within HOST_BACK are over: their players are
    /// sent away (the observation that follows checks it), the room is gone.
    fn hosts_gone(&mut self) {
        let now = self.now();
        for g in 0..self.games.len() {
            let game = &mut self.games[g];
            if game.phase != Phase::Over && game.gone_at.is_some_and(|t| now >= t + HOST_BACK) {
                game.phase = Phase::Over;
                game.room_st = RoomSt::Removed;
                self.rejoin.retain(|r, _| r.1 != g);
            }
        }
    }

    /// After an observation: no one is left in the room of a match whose host did not come back.
    fn check_gone(&self) -> Result<(), Failure> {
        if !self.p.model {
            return Ok(()); // protocol abuse moves players the model does not follow
        }
        let now = self.now();
        for game in &self.games {
            if game.gone_at.is_some_and(|t| now >= t + HOST_BACK) {
                ensure!(game.in_room.is_empty(), "gone-room-kept", "{} still has {:?}, its host left", game.room, game.in_room);
            }
        }
        Ok(())
    }

    fn serve(&mut self, h: usize, kind: u8, seats: usize, hero: i64) -> Result<(), Failure> {
        let now = self.now();
        if self.hosts[h].hc.is_none() {
            let c = self.t.conn_from(self.hosts[h].addr.as_deref());
            self.t.send(&c, json!({"t": "hello", "v": 1, "name": "RGHost"}));
            self.hosts[h].hc = Some(Hc { c, seats: None, game: None, silent: false, seen: now });
        }
        let Some(hc) = self.live_hc(h) else { return Ok(()) };
        let (c, old) = (hc.c.clone(), hc.game);
        if let Some(g) = old {
            self.end_game(g, false);
        }
        let host = &mut self.hosts[h];
        host.served += 1;
        host.last = Some(self.games.len());
        let room = format!("HOST_{}_{}", host.nick, host.served);
        let msg = match kind {
            0 => json!({"t": "serve", "room": room, "players": seats, "name": host.nick, "hero": hero, "approve": true, "key": host.key}),
            1 => json!({"t": "serve", "room": room, "players": seats, "name": host.nick, "hero": hero}),
            _ => json!({"t": "serve", "room": room, "players": seats}),
        };
        self.serves += 1;
        self.games.push(Game {
            id: BASE + self.serves,
            host: h,
            room,
            nick: host.nick.clone(),
            seats,
            approve: kind == 0,
            hero: if kind < 2 && (0..10).contains(&hero) { hero } else { -1 },
            home: host.home,
            phase: Phase::Waiting,
            room_st: RoomSt::None,
            in_room: BTreeSet::new(),
            gone_at: None,
            keyed: kind == 0,
            lost: false,
            relinked_at: None,
        });
        host.hc.as_mut().unwrap().game = Some(self.games.len() - 1);
        self.t.send(&c, msg);
        self.seen_h(h);
        Ok(())
    }

    /// Can the host's game come back on a new link now? With its key from a link the relay still
    /// has (a waiting game or a started one), or within HOST_BACK of leaving its started game
    /// (never an older host without a key: it dropped its players with its link).
    fn relink_game(&self, h: usize) -> Option<usize> {
        let g = self.hosts[h].last?;
        let game = &self.games[g];
        match &self.hosts[h].hc {
            Some(hc) => (hc.game == Some(g) && game.keyed && game.phase != Phase::Over).then_some(g),
            None => game.gone_at.is_some_and(|t| self.now() < t + HOST_BACK && game.keyed).then_some(g),
        }
    }

    /// The host's game comes back on a new link (a phone that changed networks reconnects): it
    /// serves its room again (`running` once the match started). The old link is closed as
    /// replaced; the players in the room lost their sessions with it, so they are disconnected
    /// (4004) and go back in with Quick Match.
    fn relink(&mut self, h: usize) -> Result<(), Failure> {
        let Some(g) = self.relink_game(h) else { return Ok(()) };
        let now = self.now();
        let old = self.hosts[h].hc.take();
        let c = self.t.conn_from(self.hosts[h].addr.as_deref());
        self.t.send(&c, json!({"t": "hello", "v": 1, "name": "RGHost"}));
        let (game, host) = (&self.games[g], &self.hosts[h]);
        let running = game.phase != Phase::Waiting;
        let msg = if game.keyed {
            json!({"t": "serve", "room": game.room, "players": game.seats, "name": game.nick, "hero": game.hero, "approve": true, "key": host.key, "running": running})
        } else {
            json!({"t": "serve", "room": game.room, "players": game.seats})
        };
        self.t.send(&c, msg);
        if let Some(old) = old {
            let during = old.c.take();
            ensure!(matches!(during.as_slice(), [Out::Close(4002, _)]), "relink-old", "host {h}'s old link got {during:?}");
            self.dead.push(old.c);
        }
        let game = &mut self.games[g];
        game.gone_at = None;
        game.relinked_at = Some(now);
        self.hosts[h].hc = Some(Hc { c, seats: None, game: Some(g), silent: false, seen: now });
        self.stats.relinks += 1;
        // players who came back while the host was away and wait in Quick Match go back in now
        // (casino.rs send_back): the match they dropped out of last, among the served ones
        for p in 0..self.players.len() {
            let name = self.players[p].name.to_lowercase();
            let waits = self.players[p].pc.as_ref().is_some_and(|pc| pc.st == St::Waiting);
            if !waits || !self.rejoin.contains_key(&(name.clone(), g)) {
                continue;
            }
            let back = self
                .rejoin
                .iter()
                .filter(|((n, x), _)| *n == name && self.served(*x))
                .max_by_key(|((_, x), t)| (**t, self.games[*x].room.to_lowercase()))
                .map(|(r, _)| r.clone());
            if let Some(r) = back {
                self.rejoin.remove(&r);
                self.players[p].pc.as_mut().unwrap().rejoin = Some(r.1);
            }
        }
        Ok(())
    }

    fn kick(&mut self, h: usize, name: &str) {
        let hc = self.hosts[h].hc.as_ref().unwrap();
        let c = hc.c.clone();
        if let Some(g) = hc.game.filter(|&g| self.games[g].phase == Phase::Waiting) {
            let id = self.games[g].id;
            let sits = |pl: &Player| pl.pc.as_ref().is_some_and(|pc| pc.st == St::Waiting && belief(&pc.list).0 == Some(id));
            if let Some(pl) = self.players.iter().find(|pl| pl.name.eq_ignore_ascii_case(name) && sits(pl)) {
                let game = &self.games[g];
                let home = Some(pl.home).filter(|&h| pl.addr.is_some() && !same_home(game.home, h));
                let exempt = self.players.iter().filter(|o| sits(o)).map(|o| o.name.to_lowercase()).collect();
                let kick = KickRec { host: game.nick.to_lowercase(), name: pl.name.to_lowercase(), home, at: self.now(), game: g, exempt };
                self.kicks.push(kick);
                self.stats.kicks += 1;
            }
        }
        self.t.send(&c, json!({"t": "kick", "name": name}));
        self.seen_h(h);
    }

    /// Time passes in steps of at most `step` ms; clients that are not silent ping every 15 s.
    fn pass(&mut self, ms: u64, step: u64) -> Result<(), Failure> {
        let mut left = ms;
        while left > 0 {
            let d = left.min(step);
            let now = self.now();
            for p in 0..self.players.len() {
                if let Some(pc) = self.players[p].pc.as_mut().filter(|pc| !pc.silent && pc.seen + 15_000 <= now) {
                    pc.seen = now;
                    let c = pc.c.clone();
                    self.t.send(&c, json!({"t": "ping"}));
                }
            }
            for h in 0..self.hosts.len() {
                if let Some(hc) = self.hosts[h].hc.as_mut().filter(|hc| !hc.silent && hc.seen + 15_000 <= now) {
                    hc.seen = now;
                    let c = hc.c.clone();
                    self.t.send(&c, json!({"t": "ping"}));
                }
            }
            self.t.tick(d);
            left -= d;
            self.hosts_gone();
            self.observe()?;
            self.check_timeouts()?;
            self.check_gone()?;
        }
        Ok(())
    }

    /// Messages a broken or hostile client could send.
    fn garbage(&mut self, p: usize, k: u8) {
        let Some(pc) = self.players[p].pc.as_ref() else { return };
        let c = pc.c.clone();
        let t = &mut self.t;
        match k % 16 {
            0 => t.relay.on_text(t.now, c.id, "{\"t\":\"pick\""),
            1 => t.send(&c, json!({"t": "pick"})),
            2 => t.send(&c, json!({"t": "pick", "id": [1, 2]})),
            3 => t.send(&c, json!({"t": "kick", "name": 5})),
            4 => t.send(&c, json!({"t": "start", "room": "HOST_H0_1"})),
            5 => t.send(&c, json!({"t": "serve", "room": format!("HOST_X{p}_1"), "players": 1 + k as usize % 3, "approve": true})),
            6 => t.send(&c, json!({"t": "unserve"})),
            7 => t.bin(&c, &frame(CH_MATCHMAKER, &[REQUEST, 9])),
            8 => t.bin(&c, &frame(CH_MATCHMAKER, &Writer::new().u8(REQUEST).u8(9).str("NOBODY").done())),
            9 => t.cancel(&c),
            10 => t.lock_ok(&c),
            11 => qm_as(t, &c, "PLUMBER"),
            12 => t.send(&c, json!({"t": "move", "room": format!("HOST_H{}_{}", k % 4, k % 3)})),
            13 => t.send(&c, json!({"t": "leave"})),
            14 => t.send(&c, json!({"t": "join", "room": LOBBY})),
            _ => t.send(&c, json!({"t": "pick", "id": BASE as u64 + 1 + (k as u64 % 8)})),
        }
        self.seen_p(p);
    }

    // ------------------------------------------------------------------ what the clients got

    fn observe(&mut self) -> Result<(), Failure> {
        // players first: in one relay call a MATCHED_UP (lock timeout) comes before the idle
        // closes of the same tick
        for p in 0..self.players.len() {
            let Some(mut pc) = self.players[p].pc.take() else { continue };
            let outs = pc.c.take();
            let closed = self.player_stream(p, &mut pc, outs)?;
            if closed {
                self.player_gone(p, &pc);
                self.stats.closed_by_relay += 1;
                self.dead.push(pc.c);
            } else {
                self.players[p].pc = Some(pc);
            }
        }
        for h in 0..self.hosts.len() {
            let Some(mut hc) = self.hosts[h].hc.take() else { continue };
            let outs = hc.c.take();
            let closed = self.host_stream(h, &mut hc, outs)?;
            if closed {
                self.stats.closed_by_relay += 1;
                if let Some(g) = hc.game {
                    self.end_game(g, true);
                }
                self.dead.push(hc.c);
            } else {
                self.hosts[h].hc = Some(hc);
            }
        }
        for c in &self.dead {
            let got = c.take();
            ensure!(got.is_empty(), "to-the-dead", "connection c{} got {got:?} after it was gone", c.id);
        }
        self.settle()
    }

    fn player_stream(&mut self, p: usize, pc: &mut Pc, outs: Vec<Out>) -> Result<bool, Failure> {
        let strict = self.p.model;
        let name = self.players[p].name.clone();
        let picks = self.players[p].picks;
        let mut closed = false;
        let now = self.now();
        for o in outs {
            if closed {
                return fail("after-close", format!("{name} got {o:?} after its Close"));
            }
            match o {
                Out::Close(code, why) => {
                    // 4003: its match's host is gone (when it would be sent in, or HOST_BACK after
                    // the host left while it is in the room)
                    let gone = match pc.st {
                        St::Locked => pc.launch.is_some_and(|l| self.games[self.launches[l].game].gone_at.is_some()),
                        St::InRoom => self.room_of(pc.c.id).is_some_and(|g| self.games[g].gone_at.is_some_and(|t| now >= t + HOST_BACK)),
                        _ => false,
                    };
                    // 4004: in the room of a host's game that just came back on a new link
                    let back = pc.st == St::InRoom && self.room_of(pc.c.id).is_some_and(|g| self.games[g].relinked_at == Some(now));
                    ensure!(
                        !strict || code == 4001 || (code == 4002 && pc.silent) || (code == 4003 && gone) || (code == 4004 && back),
                        "close",
                        "{name} ({:?}) closed: {code} {why}",
                        pc.st
                    );
                    self.stats.sent_away += (code == 4003) as usize;
                    self.stats.taken_back += (code == 4004) as usize;
                    closed = true;
                }
                Out::Text(s) => {
                    let v: Value = serde_json::from_str(&s).unwrap();
                    match v["t"].as_str().unwrap_or("") {
                        "games" => {
                            ensure!(picks || !strict, "games-to-older", "{name} (an older game) got {v}");
                            pc.got_games = true;
                            pc.games = if v["on"] == true { Some(v) } else { None };
                        }
                        "welcome" => ensure!(!strict || v["name"] == name.as_str(), "welcome", "{name} welcomed as {v}"),
                        "error" => ensure!(!strict, "error", "{name} got {v}"),
                        "joined" | "left" | "pong" | "peer_joined" | "peer_left" => {}
                        _ => ensure!(!strict, "unexpected", "{name} got {v}"),
                    }
                }
                Out::Binary(b) => {
                    for (ch, data) in parse_frames(&b) {
                        if (CH_CASINO..=CH_FRIENDLY).contains(&ch) && data.len() >= 2 {
                            self.casino_msg(&name, pc, ch, data[0], data[1], &data[2..])?;
                        }
                    }
                }
            }
        }
        Ok(closed)
    }

    fn casino_msg(&mut self, name: &str, pc: &mut Pc, ch: u8, t: u8, action: u8, rest: &[u8]) -> Result<(), Failure> {
        let strict = self.p.model;
        let now = self.now();
        let mut r = Reader::new(rest);
        let short = |what: &str| Failure { kind: "short", msg: format!("{name}: truncated {what}") };
        match (t, action) {
            (RESPONSE, 9) => {
                r.u8().map_err(|_| short("answer"))?;
                r.u32().map_err(|_| short("answer"))?;
                let n = r.u8().map_err(|_| short("answer"))?;
                pc.list.clear();
                for _ in 0..n {
                    pc.list.push(entry(&r.kv().map_err(|_| short("answer"))?));
                }
                pc.st = St::Waiting;
                pc.launch = None;
                pc.intros.clear();
            }
            (RESPONSE, 10) => {
                pc.st = St::Lobby;
                pc.list.clear();
                pc.launch = None;
                pc.intros.clear();
            }
            (EVENT, 9) => {
                ensure!(!strict || pc.st == St::Waiting, "list-state", "{name} ({:?}) got a list entry", pc.st);
                let id = r.str().map_err(|_| short("entry"))?;
                let e = entry(&r.kv().map_err(|_| short("entry"))?);
                ensure!(e.0 == id, "list-id", "{name}: entry {id} names profile {}", e.0);
                ensure!(!pc.list.iter().any(|x| x.0 == id), "listed-twice", "{name}: {id} ({}) listed twice: {:?}", e.1, pc.list);
                pc.list.push(e);
            }
            (EVENT, 10) => {
                ensure!(!strict || pc.st == St::Waiting, "list-state", "{name} ({:?}) got a list removal", pc.st);
                let id = r.str().map_err(|_| short("removal"))?;
                ensure!(!strict || pc.list.iter().any(|x| x.0 == id), "removed-unlisted", "{name}: {id} removed but not listed: {:?}", pc.list);
                pc.list.retain(|x| x.0 != id);
            }
            (EVENT, 8) => {
                ensure!(!strict || pc.st == St::Waiting, "intro-state", "{name} ({:?}) got an introduction", pc.st);
                r.str().map_err(|_| short("intro"))?;
                pc.intros.push(entry(&r.kv().map_err(|_| short("intro"))?).1);
            }
            (REQUEST, 29) if ch == CH_CASINO => {
                ensure!(!strict || pc.st == St::Waiting, "lock-state", "{name} ({:?}) locked into a game", pc.st);
                let (b, _) = belief(&pc.list);
                // a way back goes first: also for a player seated in another game when their match's
                // host came back (casino.rs send_back)
                let (game, rejoin) = match b.and_then(|id| self.game_by_id(id as u64)) {
                    _ if pc.rejoin.is_some() => (pc.rejoin.take(), true),
                    Some(g) => (Some(g), false),
                    None => (None, false),
                };
                pc.st = St::Locked;
                let Some(g) = game else {
                    ensure!(!strict, "lock-without-game", "{name} locked without a game (list {:?})", pc.list);
                    pc.launch = None;
                    return Ok(());
                };
                let l = if rejoin { None } else { self.launches.iter().rposition(|l| l.fresh && !l.rejoin && l.game == g) };
                let l = l.unwrap_or_else(|| {
                    self.launches.push(Launch {
                        game: g,
                        rejoin,
                        members: Vec::new(),
                        names: Vec::new(),
                        intros: Vec::new(),
                        answered: BTreeSet::new(),
                        deadline: now + LOCK_MS,
                        fresh: true,
                        done: false,
                        matched: Vec::new(),
                    });
                    self.launches.len() - 1
                });
                let launch = &mut self.launches[l];
                launch.members.push(pc.c.id);
                launch.names.push(name.to_string());
                launch.intros.push(std::mem::take(&mut pc.intros));
                pc.launch = Some(l);
            }
            (EVENT, 31) => {
                let parsed = (|| -> Res<(String, u32)> {
                    r.str()?;
                    let room = r.str()?;
                    r.str()?;
                    r.u8()?;
                    r.u8()?;
                    Ok((room, r.u32()?))
                })();
                let (room, count) = parsed.map_err(|_| short("MATCHED_UP"))?;
                ensure!(!strict || pc.st == St::Locked, "matched-state", "{name} ({:?}) got MATCHED_UP {room}", pc.st);
                if let Some(l) = pc.launch {
                    let g = self.launches[l].game;
                    ensure!(!strict || room == self.games[g].room, "matched-room", "{name} sent to {room}, not {}", self.games[g].room);
                    ensure!(!strict || self.served(g), "dead-room", "{name} sent into {room}, whose host is gone");
                    self.launches[l].matched.push((pc.c.id, count));
                    pc.arriving = Some(g);
                }
                pc.st = St::Matched;
                pc.room = Some(room);
                pc.matched_at = now;
                self.stats.matched += 1;
                pc.launch = None;
                pc.list.clear();
            }
            _ => {}
        }
        Ok(())
    }

    fn host_stream(&mut self, h: usize, hc: &mut Hc, outs: Vec<Out>) -> Result<bool, Failure> {
        let strict = self.p.model;
        let mut closed = false;
        for o in outs {
            if closed {
                return fail("after-close", format!("host {h} got {o:?} after its Close"));
            }
            match o {
                Out::Close(code, why) => {
                    ensure!(!strict || code == 4001 || (code == 4002 && hc.silent), "close", "host {h} closed: {code} {why}");
                    closed = true;
                }
                Out::Text(s) => {
                    let v: Value = serde_json::from_str(&s).unwrap();
                    match v["t"].as_str().unwrap_or("") {
                        "seats" => {
                            let g = hc.game.map(|g| &self.games[g]);
                            ensure!(
                                !strict || g.is_some_and(|g| g.approve && g.room == v["room"].as_str().unwrap_or("")),
                                "seats-unasked",
                                "host {h} got {v} (its game: {:?})",
                                g.map(|g| (&g.room, g.approve))
                            );
                            hc.seats = Some(v);
                        }
                        "player_joined" => {
                            let n = v["name"].as_str().unwrap_or("").to_string();
                            let i = self.expect_joined.iter().position(|(eh, en)| *eh == h && *en == n);
                            ensure!(!strict || i.is_some(), "joined-unexpected", "host {h} got {v}");
                            if let Some(i) = i {
                                self.expect_joined.remove(i);
                            }
                        }
                        "error" => ensure!(!strict, "error", "host {h} got {v}"),
                        _ => {}
                    }
                }
                Out::Binary(_) => {}
            }
        }
        Ok(closed)
    }

    /// Checks the groups locked in this observation and the MATCHED_UPs that went out.
    fn settle(&mut self) -> Result<(), Failure> {
        let strict = self.p.model;
        for l in 0..self.launches.len() {
            if !self.launches[l].fresh {
                continue;
            }
            self.launches[l].fresh = false;
            let launch = &self.launches[l];
            let g = launch.game;
            let game = &self.games[g];
            if !strict {
                if !launch.rejoin && game.phase == Phase::Waiting {
                    self.games[g].phase = Phase::Starting;
                }
                continue;
            }
            if launch.rejoin {
                ensure!(self.served(g), "rejoin-phase", "{:?} sent back into {} ({:?}, host left at {:?})", launch.names, game.room, game.phase, game.gone_at);
                ensure!(launch.intros[0].is_empty(), "rejoin-intros", "{:?} introduced on rejoin", launch.intros);
                self.stats.rejoins += 1;
                continue;
            }
            ensure!(game.phase == Phase::Waiting, "start-phase", "{:?} locked into {} ({:?})", launch.names, game.room, game.phase);
            ensure!(launch.members.len() == game.seats, "start-seats", "{} started with {:?} of {} seats", game.room, launch.names, game.seats);
            if game.approve {
                ensure!(self.started.contains(&g), "start-unasked", "{} started without its host: {:?}", game.room, launch.names);
                let mut told: Vec<String> = self.hosts[game.host]
                    .hc
                    .as_ref()
                    .and_then(|hc| hc.seats.as_ref())
                    .filter(|s| s["room"] == game.room.as_str())
                    .map(|s| s["names"].as_array().unwrap().iter().map(|n| n.as_str().unwrap().to_string()).collect())
                    .unwrap_or_default();
                told.sort();
                let mut names = launch.names.clone();
                names.sort();
                ensure!(told == names, "start-not-seated", "{} started with {names:?}, its host saw {told:?}", game.room);
            }
            for (i, got) in launch.intros.iter().enumerate() {
                let mut want: Vec<String> = launch.names.iter().enumerate().filter(|(j, _)| *j != i).map(|(_, n)| n.clone()).collect();
                want.sort();
                let mut got = got.clone();
                got.sort();
                ensure!(got == want, "intros", "{} in {}: introduced to {got:?}, not {want:?}", launch.names[i], game.room);
            }
            self.games[g].phase = Phase::Starting;
            self.stats.starts += 1;
        }
        for l in 0..self.launches.len() {
            if self.launches[l].done || self.launches[l].matched.is_empty() {
                continue;
            }
            let launch = &self.launches[l];
            let left: Vec<&str> = launch
                .members
                .iter()
                .filter(|&&m| self.players.iter().any(|pl| pl.pc.as_ref().is_some_and(|pc| pc.c.id == m && pc.st == St::Locked)))
                .map(|m| launch.names[launch.members.iter().position(|x| x == m).unwrap()].as_str())
                .collect();
            if strict {
                ensure!(left.is_empty(), "matched-some", "{}: MATCHED_UP left out {left:?}", self.games[launch.game].room);
                // the host's engine waits for everyone the match started with
                let n = launch.members.len() as u32;
                ensure!(launch.matched.iter().all(|m| m.1 == n), "matched-count", "{:?} of {}, counts {:?}", launch.names, n, launch.matched);
            }
            self.finish(l);
        }
        // once every player still locked in has answered, the others are not waited for
        for launch in self.launches.iter().filter(|l| !l.done) {
            let live: Vec<u64> = launch.members.iter().copied().filter(|&m| self.locked_pc(m)).collect();
            let all = !live.is_empty() && live.iter().all(|m| launch.answered.contains(m));
            ensure!(!strict || !all, "lock-answered", "{:?} all answered, no MATCHED_UP for {}", launch.names, self.games[launch.game].room);
        }
        Ok(())
    }

    /// Is this connection a player's, locked into a game (waiting for MATCHED_UP)?
    fn locked_pc(&self, conn: u64) -> bool {
        self.players.iter().any(|pl| pl.pc.as_ref().is_some_and(|pc| pc.c.id == conn && pc.st == St::Locked))
    }

    fn finish(&mut self, l: usize) {
        let launch = &mut self.launches[l];
        launch.done = true;
        launch.matched.clear();
        let g = launch.game;
        if self.games[g].phase == Phase::Starting {
            self.games[g].phase = Phase::Running;
        }
    }

    /// After a tick: every lock older than the timeout has its MATCHED_UP.
    fn check_timeouts(&mut self) -> Result<(), Failure> {
        let now = self.now();
        for l in 0..self.launches.len() {
            let launch = &self.launches[l];
            if launch.done || now < launch.deadline {
                continue;
            }
            let live = launch
                .members
                .iter()
                .any(|&m| self.players.iter().any(|pl| pl.pc.as_ref().is_some_and(|pc| pc.c.id == m && pc.st == St::Locked)));
            ensure!(!self.p.model || !live, "lock-timeout", "{:?} still wait for MATCHED_UP into {}", launch.names, self.games[launch.game].room);
            self.finish(l);
        }
        Ok(())
    }

    // ------------------------------------------------------------------ the invariants

    fn check_end(&mut self) -> Result<(), Failure> {
        let now = self.now();
        if !self.p.model {
            return self.check_safety();
        }
        for pl in &self.players {
            if let Some(pc) = &pl.pc {
                ensure!(pc.rejoin.is_none(), "rejoin", "{} was not sent back into {}", pl.name, self.games[pc.rejoin.unwrap()].room);
            }
        }
        ensure!(self.expect_joined.is_empty(), "joined-missing", "hosts did not hear of {:?}", self.expect_joined);
        // who believes they sit where
        let mut believers: HashMap<usize, Vec<String>> = HashMap::new();
        let mut queue: Vec<String> = Vec::new();
        let mut waiting = 0;
        for pl in &self.players {
            let Some(pc) = &pl.pc else { continue };
            if pc.st != St::Waiting {
                ensure!(!pl.picks || pc.games.is_none(), "games-on", "{} ({:?}) still has a list of games", pl.name, pc.st);
                continue;
            }
            waiting += 1;
            ensure!(pl.picks || !pc.got_games, "games-to-older", "{} got games", pl.name);
            let (b, n) = belief(&pc.list);
            ensure!(n <= 1, "two-games", "{} lists {n} hosts: {:?}", pl.name, pc.list);
            if pl.picks {
                let Some(g) = &pc.games else { return fail("games-off", format!("{} waits without a list of games", pl.name)) };
                ensure!(g["seat"] == b.unwrap_or(0), "seat", "{}: seat {} but listed with {b:?}", pl.name, g["seat"]);
            }
            match b {
                Some(id) => {
                    let Some(g) = self.game_by_id(id as u64) else { return fail("unknown-game", format!("{} sits in {id}", pl.name)) };
                    believers.entry(g).or_default().push(pl.name.clone());
                }
                None => queue.push(pl.name.clone()),
            }
        }
        for (&g, bs) in &believers {
            let game = &self.games[g];
            ensure!(game.phase == Phase::Waiting, "seated-gone", "{bs:?} still sit in {} ({:?})", game.room, game.phase);
            ensure!(bs.len() <= game.seats, "over-seats", "{bs:?} sit in {} with {} seats", game.room, game.seats);
            for b in bs {
                let pl = self.players.iter().find(|pl| pl.name == *b).unwrap();
                ensure!(!self.kept_out(pl, g), "kicked-seated", "{b} sits in {}, whose host sent them away", game.room);
            }
        }
        let seated = |g: usize| believers.get(&g).map_or(0, Vec::len);
        let live: Vec<usize> = (0..self.games.len()).filter(|&g| self.games[g].phase == Phase::Waiting).collect();
        for &g in &live {
            let game = &self.games[g];
            ensure!(game.approve || seated(g) < game.seats, "full-not-started", "{} is full but did not start", game.room);
            if game.approve {
                let s = self.hosts[game.host].hc.as_ref().and_then(|hc| hc.seats.as_ref());
                let Some(s) = s.filter(|s| s["room"] == game.room.as_str()) else {
                    return fail("seats-missing", format!("the host of {} never heard who sits there", game.room));
                };
                let mut told: Vec<&str> = s["names"].as_array().unwrap().iter().map(|n| n.as_str().unwrap()).collect();
                told.sort();
                let mut want: Vec<&str> = believers.get(&g).map_or(Vec::new(), |b| b.iter().map(String::as_str).collect());
                want.sort();
                ensure!(told == want, "seats", "the host of {} sees {told:?}, seated: {want:?}", game.room);
                ensure!(s["of"] == game.seats, "seats-of", "{}: {s}", game.room);
            }
        }
        // the "Players in Match" lists agree with each other, and profile ids are consistent
        let mut ids: HashMap<String, String> = HashMap::new();
        for pl in &self.players {
            let Some(pc) = pc_waiting(pl) else { continue };
            let (b, _) = belief(&pc.list);
            let mut want: Vec<String> = match b.and_then(|id| self.game_by_id(id as u64)) {
                Some(g) => {
                    let game = &self.games[g];
                    let mut w: Vec<String> = believers[&g].iter().filter(|n| **n != pl.name).cloned().collect();
                    w.push(format!("host {} {}", game.nick, hero_name(game.hero)));
                    w
                }
                None => queue.iter().filter(|n| **n != pl.name).cloned().collect(),
            };
            want.sort();
            let mut got: Vec<String> = pc
                .list
                .iter()
                .map(|e| if host_entry(e).is_some() { format!("host {} {}", e.1, e.2) } else { e.1.clone() })
                .collect();
            got.sort();
            ensure!(got == want, "list", "{}'s Players in Match: {got:?}, should be {want:?}", pl.name);
            for e in pc.list.iter().filter(|e| host_entry(e).is_none()) {
                let other = self.players.iter().find(|o| o.name == e.1).and_then(|o| o.pc.as_ref());
                ensure!(other.is_some_and(|o| o.hero == e.2), "list-hero", "{} lists {} as {}", pl.name, e.1, e.2);
                let id = ids.entry(e.1.clone()).or_insert_with(|| e.0.clone());
                ensure!(*id == e.0, "profile-id", "{} has ids {id} and {}", e.1, e.0);
            }
        }
        let mut seen_ids = HashSet::new();
        for id in ids.values() {
            ensure!(seen_ids.insert(id), "profile-id", "two players with id {id}: {ids:?}");
        }
        // the games each chooser is offered
        let mut expired = 0;
        for pl in &self.players {
            let Some(pc) = pc_waiting(pl) else { continue };
            let Some(g) = &pc.games else { continue };
            let seat = g["seat"].as_u64().unwrap();
            let entries = g["games"].as_array().unwrap();
            ensure!(entries.len() <= 20, "too-many", "{}: {} games", pl.name, entries.len());
            let mut prev: Option<(u64, u64)> = None;
            for e in entries {
                let id = e[0].as_u64().unwrap();
                let Some(gi) = self.game_by_id(id) else { return fail("offered-unknown", format!("{}: {e}", pl.name)) };
                let game = &self.games[gi];
                ensure!(game.phase == Phase::Waiting, "offered-gone", "{} is offered {} ({:?})", pl.name, game.room, game.phase);
                let want = json!([id, game.nick, game.hero, seated(gi), game.seats, same_home(game.home, pl.home) as u8, game.approve as u8]);
                ensure!(*e == want, "offered-entry", "{} is offered {e}, should be {want}", pl.name);
                ensure!(seated(gi) < game.seats || id == seat, "offered-full", "{} is offered the full {}", pl.name, game.room);
                ensure!(
                    id == seat || !self.kicked(pl, gi),
                    "offered-kicked",
                    "{} is offered {}, whose host sent them away",
                    pl.name,
                    game.room
                );
                let home = e[5].as_u64().unwrap();
                if let Some((ph, pid)) = prev {
                    ensure!(ph > home || (ph == home && pid > id), "order", "{}: games out of order: {g}", pl.name);
                }
                prev = Some((home, id));
            }
            let listed = ids_of(g);
            if seat != 0 {
                ensure!(listed.contains(&seat), "own-game-unlisted", "{}: seat {seat} not among {listed:?}", pl.name);
            }
            // a kick that ran out offers its games again by itself (the relay's own timer)
            expired += live.iter().any(|&gi| self.kicked_ever(pl, gi) && !self.kicked(pl, gi)) as usize;
            let eligible_game = |gi: usize| {
                let id = self.games[gi].id as u64;
                id == seat || (seated(gi) < self.games[gi].seats && !self.kicked(pl, gi))
            };
            if live.len() > 20 {
                // the first 20 games in that order (one may give way to the player's own)
                let mut eligible: Vec<(u8, u64)> = live
                    .iter()
                    .filter(|&&gi| eligible_game(gi))
                    .map(|&gi| (same_home(self.games[gi].home, pl.home) as u8, self.games[gi].id as u64))
                    .collect();
                eligible.sort_by(|a, b| b.cmp(a));
                let others: Vec<u64> = eligible.iter().map(|e| e.1).filter(|&id| id != seat).collect();
                let got: Vec<u64> = listed.iter().copied().filter(|&id| id != seat).collect();
                ensure!(
                    listed.len() == eligible.len().min(20) && others.starts_with(&got),
                    "offered-top",
                    "{} is offered {listed:?}, should be the first 20 of {eligible:?}",
                    pl.name
                );
            }
            if live.len() <= 20 {
                // every game with a free seat is offered
                let want: BTreeSet<u64> = live.iter().filter(|&&gi| eligible_game(gi)).map(|&gi| self.games[gi].id as u64).collect();
                let got: BTreeSet<u64> = listed.iter().copied().collect();
                ensure!(got == want, "offered-missing", "{} is offered {got:?}, should be {want:?}", pl.name);
            }
        }
        // older games' players are seated when a seat is free; everyone at home first
        for pl in &self.players {
            let Some(pc) = pc_waiting(pl) else { continue };
            let (b, _) = belief(&pc.list);
            let free = |gi: usize| {
                let game = &self.games[gi];
                game.phase == Phase::Waiting && seated(gi) < game.seats && !self.kicked(pl, gi)
            };
            if !pl.picks && b.is_none() {
                let f: Vec<&str> = live.iter().filter(|&&gi| free(gi)).map(|&gi| self.games[gi].room.as_str()).collect();
                ensure!(f.is_empty(), "older-waits", "{} waits while {f:?} have a free seat", pl.name);
            }
            let exempt = (pl.picks && b.is_some()) || (!pl.picks && self.backed.contains(&pl.name.to_lowercase()));
            if !exempt {
                let home_free = live.iter().any(|&gi| free(gi) && same_home(self.games[gi].home, pl.home));
                let at_home = b.and_then(|id| self.game_by_id(id as u64)).is_some_and(|gi| same_home(self.games[gi].home, pl.home));
                ensure!(!home_free || at_home, "home-first", "{} (home {}) is not seated at home:\n{}", pl.name, pl.home, self.state());
            }
        }
        let st = self.t.relay.status();
        ensure!(st["hosted"] == live.len(), "status-hosted", "status {st}, {} waiting games", live.len());
        ensure!(st["queued"] == waiting, "status-queued", "status {st}, {waiting} waiting players");
        let conns = self.players.iter().filter(|p| p.pc.is_some()).count() + self.hosts.iter().filter(|h| h.hc.is_some()).count();
        ensure!(st["players"] == conns, "status-players", "status {st}, {conns} connections");
        self.stats.expired_kicks += expired;
        self.stats.crowded += (live.len() > 20) as usize;
        let _ = now;
        Ok(())
    }

    /// Without the model (protocol abuse): what must hold whatever the clients send.
    fn check_safety(&self) -> Result<(), Failure> {
        for pl in &self.players {
            let Some(pc) = &pl.pc else { continue };
            let Some(g) = &pc.games else { continue };
            let seat = g["seat"].as_u64().unwrap_or(0);
            let entries = g["games"].as_array().unwrap();
            ensure!(entries.len() <= 20, "too-many", "{}: {g}", pl.name);
            for e in entries {
                let (id, seated, seats) = (e[0].as_u64().unwrap(), e[3].as_u64().unwrap(), e[4].as_u64().unwrap());
                ensure!(seated < seats || (id == seat && seated <= seats), "offered-full", "{}: {g}", pl.name);
            }
            if seat != 0 {
                ensure!(ids_of(g).contains(&seat), "own-game-unlisted", "{}: {g}", pl.name);
            }
        }
        let st = self.t.relay.status();
        let conns = self.players.iter().filter(|p| p.pc.is_some()).count() + self.hosts.iter().filter(|h| h.hc.is_some()).count();
        ensure!(st["players"] == conns, "status-players", "status {st}, {conns} connections");
        Ok(())
    }
}

fn pc_waiting(pl: &Player) -> Option<&Pc> {
    pl.pc.as_ref().filter(|pc| pc.st == St::Waiting)
}

// ------------------------------------------------------------------ running and minimizing

fn panic_text(e: Box<dyn std::any::Any + Send>) -> String {
    e.downcast_ref::<String>().cloned().or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_default()
}

fn apply_caught(w: &mut World, a: Act) -> Result<(), Failure> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| w.apply(a))) {
        Ok(r) => r,
        Err(e) => fail("panic", panic_text(e)),
    }
}

/// Replays these steps; the failing step and what failed.
fn replay(p: &Profile, seed: u64, acts: &[Act]) -> Result<(), (usize, Failure)> {
    let mut w = World::new(p.clone(), seed);
    for (i, a) in acts.iter().enumerate() {
        apply_caught(&mut w, *a).map_err(|f| (i, f))?;
    }
    Ok(())
}

/// Delta debugging: drops chunks of steps while the same kind of failure remains.
fn minimize(p: &Profile, seed: u64, acts: Vec<Act>, kind: &str) -> Vec<Act> {
    let fails = |a: &[Act]| match replay(p, seed, a) {
        Err((i, f)) if f.kind == kind => Some(i),
        _ => None,
    };
    let Some(at) = fails(&acts) else { return acts };
    let mut acts = acts[..=at].to_vec();
    let mut n = 2;
    let mut rounds = 0;
    while acts.len() >= 2 && rounds < 400 {
        rounds += 1;
        let chunk = acts.len().div_ceil(n);
        let smaller = (0..n).take_while(|i| i * chunk < acts.len()).find_map(|i| {
            let (lo, hi) = (i * chunk, ((i + 1) * chunk).min(acts.len()));
            let mut cand = acts[..lo].to_vec();
            cand.extend_from_slice(&acts[hi..]);
            fails(&cand).map(|at| {
                cand.truncate(at + 1);
                cand
            })
        });
        match smaller {
            Some(cand) => {
                acts = cand;
                n = (n - 1).max(2);
            }
            None if n >= acts.len() => break,
            None => n = (n * 2).min(acts.len()),
        }
    }
    acts
}

/// Runs random steps for each seed from `first`; panics with the seed, the step and the minimized
/// steps on the first failure. What the runs went through, summed. The default size keeps `cargo
/// test` quick; a long run sets RG_FUZZ_STEPS (steps per seed) and RG_FUZZ_SEEDS (seeds per run),
/// e.g. `RG_FUZZ_STEPS=3000 RG_FUZZ_SEEDS=24 cargo test --release -p rg-server --test edge_fuzz`;
/// RG_FUZZ_BASE shifts the seeds (other runs than the usual ones).
fn fuzz(p: &Profile, first: u64) -> Stats {
    let size = |var: &str, default: u64| std::env::var(var).ok().and_then(|v| v.parse().ok()).unwrap_or(default);
    let (seeds, steps) = (size("RG_FUZZ_SEEDS", 6), size("RG_FUZZ_STEPS", 600) as usize);
    let first = first + size("RG_FUZZ_BASE", 0);
    let mut total = Stats::default();
    for seed in first..first + seeds {
        let mut r = Rng::new(seed);
        let mut w = World::new(p.clone(), seed);
        let mut acts = Vec::with_capacity(steps);
        for step in 0..steps {
            let a = w.gen(&mut r);
            acts.push(a);
            w.stats.steps += 1;
            if let Err(f) = apply_caught(&mut w, a) {
                let state = w.state();
                let small = minimize(p, seed, acts, f.kind);
                let again = replay(p, seed, &small).err().map(|(i, f)| format!("step {i}: {}", f.msg)).unwrap_or_default();
                panic!(
                    "{} seed {seed} step {step}: [{}] {}\nstate:\n{state}minimized to {} steps ({again}):\n{:?}",
                    p.name,
                    f.kind,
                    f.msg,
                    small.len(),
                    small
                );
            }
        }
        total.add(&w.stats);
    }
    eprintln!("{}: {total:?}", p.name);
    total
}

// ------------------------------------------------------------------ the runs

fn many_games() -> Profile {
    Profile { name: "many-games", players: 10, hosts: 30, host_pct: 55, wait_pct: 1, ..Profile::mixed() }
}

#[test]
fn fuzz_players_who_choose_and_older_games_mixed() {
    let s = fuzz(&Profile::mixed(), 0);
    assert!(s.picks > 0 && s.starts > 0 && s.matched > 0 && s.rooms > 0 && s.rejoins > 0 && s.kicks > 0, "{s:?}");
    assert!(s.sent_away > 0 && s.relinks > 0, "hosts who left, hosts back: {s:?}");
}

#[test]
fn fuzz_only_players_who_choose_and_hosts_who_start() {
    let p = Profile { name: "choosers", chooser_pct: 100, approve_pct: 100, ..Profile::mixed() };
    let s = fuzz(&p, 100);
    assert!(s.picks > 0 && s.starts > 0 && s.matched > 0 && s.kicks > 0, "{s:?}");
}

#[test]
fn fuzz_only_older_games_and_older_hosts() {
    let p = Profile { name: "older", chooser_pct: 0, approve_pct: 0, named_pct: 50, ..Profile::mixed() };
    let s = fuzz(&p, 200);
    assert!(s.starts > 0 && s.matched > 0 && s.rejoins > 0, "{s:?}");
}

#[test]
fn fuzz_a_lan_relay() {
    let p = Profile { name: "lan", homes: vec![LAN], ..Profile::mixed() };
    let s = fuzz(&p, 300);
    assert!(s.starts > 0 && s.matched > 0, "{s:?}");
}

#[test]
fn fuzz_a_crowded_relay_with_two_homes() {
    let p = Profile { name: "crowded", players: 14, hosts: 8, homes: vec![0, 1], tick_pct: 8, ..Profile::mixed() };
    let s = fuzz(&p, 400);
    assert!(s.starts > 0 && s.matched > 0, "{s:?}");
}

#[test]
fn fuzz_more_than_twenty_games_waiting() {
    let s = fuzz(&many_games(), 700);
    assert!(s.crowded > 0 && s.picks > 0, "{s:?}");
}

#[test]
fn fuzz_kicks_and_long_waits() {
    let p = Profile { name: "kicks", kick_pct: 30, wait_pct: 8, long_pct: 60, max_seats: 2, ..Profile::mixed() };
    let s = fuzz(&p, 500);
    assert!(s.kicks > 0 && s.expired_kicks > 0 && s.closed_by_relay > 0, "{s:?}");
}

#[test]
fn fuzz_protocol_abuse_never_breaks_the_relay() {
    let p = Profile { name: "abuse", abuse: true, model: false, ..Profile::mixed() };
    fuzz(&p, 600);
}


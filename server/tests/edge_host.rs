//! Host-side edge cases of "Choosing a game": `seats`, `start`, `kick`, hosts that stop or come
//! back, older hosts, several hosts at one home.
#![allow(dead_code)]

include!("harness/mod.rs");

/// The list id of the n-th game hosted on this relay (900000000 + n).
fn gid(n: u64) -> u64 {
    900_000_000 + n
}

/// A player whose game chooses, on the "Players in Match" screen (inbox emptied).
fn waiting_chooser(t: &mut T, name: &str, addr: &str) -> C {
    let c = t.chooser_at(name, addr);
    t.quick_match(&c);
    c.take();
    c
}

/// A host's game link: hello as RGHost and `serve`; the answers to `serve` stay in the inbox.
fn open_host(t: &mut T, nick: &str, room: &str, players: u32, approve: bool, addr: &str) -> C {
    let h = t.conn_from(Some(addr));
    t.send(&h, json!({"t": "hello", "v": 1, "name": "RGHost"}));
    h.take();
    serve(t, &h, nick, room, players, approve);
    h
}

fn serve(t: &mut T, h: &C, nick: &str, room: &str, players: u32, approve: bool) {
    t.send(h, json!({"t": "serve", "room": room, "players": players, "name": nick, "hero": 7, "approve": approve}));
}

/// A hosting session's key (random per session in the engine).
const KEY: &str = "0123456789abcdef";

/// A host's game link as the engine opens it since the keys: hello as RGHost and `serve` with the
/// session's key (`running` once its match has started); the answers stay in the inbox.
fn keyed_host(t: &mut T, room: &str, players: u32, key: &str, running: bool, addr: &str) -> C {
    let h = t.conn_from(Some(addr));
    t.send(&h, json!({"t": "hello", "v": 1, "name": "RGHost"}));
    h.take();
    let mut m = json!({"t": "serve", "room": room, "players": players, "name": "Ann", "hero": 7, "approve": true, "key": key});
    if running {
        m["running"] = json!(true);
    }
    t.send(&h, m);
    h
}

/// Ann's started match (HOST_Ann_1, keyed) with Pia and Rex in its room, seated; the inboxes are
/// empty.
fn keyed_match(t: &mut T) -> (C, C, C) {
    let ann = keyed_host(t, "HOST_Ann_1", 2, KEY, false, "82.0.0.1");
    let pia = waiting_chooser(t, "Pia", "82.0.0.3");
    let rex = waiting_chooser(t, "Rex", "82.0.0.4");
    pick(t, &pia, gid(1));
    pick(t, &rex, gid(1));
    start(t, &ann);
    for c in [&pia, &rex] {
        t.lock_ok(c);
    }
    for c in [&pia, &rex] {
        assert_eq!(matched_room(&c.msgs()).as_deref(), Some("HOST_Ann_1"));
        t.send(c, json!({"t": "move", "room": "HOST_Ann_1"}));
        t.bin(c, &frame(CH_TABLE, &Writer::new().u8(REQUEST).u8(13).u32(7).done()));
    }
    assert_eq!(ann.texts().iter().filter(|m| m["t"] == "player_joined").count(), 2);
    for c in [&pia, &rex] {
        c.take();
    }
    (ann, pia, rex)
}

fn pick(t: &mut T, c: &C, id: u64) {
    t.send(c, json!({"t": "pick", "id": id}));
}

fn start(t: &mut T, h: &C) {
    t.send(h, json!({"t": "start"}));
}

fn kick(t: &mut T, h: &C, name: &str) {
    t.send(h, json!({"t": "kick", "name": name}));
}

/// Back on the "Players in Match" screen, then Quick Match again.
fn again(t: &mut T, c: &C) {
    t.cancel(c);
    t.quick_match(c);
}

/// The last `games` message this chooser got (empties the inbox).
fn last_offer(c: &C) -> Option<Value> {
    last_games(&c.texts()).cloned()
}

/// Lets `ms` pass in 10 s steps; the connections in `keep` ping so they are not closed as idle.
fn later(t: &mut T, ms: u64, keep: &[&C]) {
    let mut left = ms;
    while left > 0 {
        for c in keep {
            t.send(c, json!({"t": "ping"}));
        }
        let step = left.min(10_000);
        t.tick(step);
        left -= step;
    }
    for c in keep {
        t.send(c, json!({"t": "ping"}));
    }
}

fn locks(msgs: &[Msg]) -> usize {
    msgs.iter().filter(|m| m.0 == CH_CASINO && m.1 == REQUEST && m.2 == 29).count()
}

fn matched_ups(msgs: &[Msg]) -> usize {
    msgs.iter().filter(|m| m.1 == EVENT && m.2 == 31).count()
}

// ------------------------------------------------------------------ seats

/// An approving host hears its (empty) seats right after `serving`; an older host never gets
/// `seats` (its engine does not know them).
#[test]
fn an_approving_host_hears_its_seats_an_older_host_does_not() {
    let mut t = T::new(Config::default());
    let ann = open_host(&mut t, "Ann", "HOST_Ann_1", 2, true, "82.0.0.1");
    let texts = ann.texts();
    assert_eq!(texts[0], json!({"t": "serving", "room": "HOST_Ann_1"}));
    assert!(seats(&texts).unwrap_or_default().is_empty(), "nobody seated yet: {texts:?}");
    let old = t.host_at("Old", 1, 3, "82.0.0.2");
    let pia = waiting_chooser(&mut t, "Pia", "82.0.0.3");
    pick(&mut t, &pia, gid(1));
    assert_eq!(ann.texts(), vec![json!({"t": "seats", "room": "HOST_Ann_1", "names": ["Pia"], "of": 2})]);
    let quinn = waiting_chooser(&mut t, "Quinn", "82.0.0.4");
    pick(&mut t, &quinn, gid(2)); // the older host's game: full, it starts by itself
    assert!(got_lock(&quinn.msgs()));
    assert!(old.texts().iter().all(|m| m["t"] != "seats"), "older hosts get no seats");
}

/// Every pick, move to another game, Back and drop reaches the host as one `seats` message, and
/// only the host whose seats changed hears of it.
#[test]
fn the_host_hears_every_change_of_its_seats() {
    let mut t = T::new(Config::default());
    let ann = t.host_approving("Ann", 3, "82.0.0.1");
    t.tick(1_000);
    let ben = t.host_approving("Ben", 2, "82.0.0.2");
    let pia = waiting_chooser(&mut t, "Pia", "82.0.0.3");
    let quinn = waiting_chooser(&mut t, "Quinn", "82.0.0.4");
    let rex = waiting_chooser(&mut t, "Rex", "82.0.0.5");
    ann.take();
    ben.take();
    let ann_seats = |names: &[&str]| vec![json!({"t": "seats", "room": "HOST_Ann_1", "names": names, "of": 3})];
    pick(&mut t, &pia, gid(1));
    assert_eq!(ann.texts(), ann_seats(&["Pia"]));
    assert!(ben.texts().is_empty(), "Ben's seats did not change");
    pick(&mut t, &quinn, gid(1));
    assert_eq!(ann.texts(), ann_seats(&["Pia", "Quinn"]));
    // Pia moves to Ben's game: both hosts hear it
    pick(&mut t, &pia, gid(2));
    assert_eq!(ann.texts(), ann_seats(&["Quinn"]));
    assert_eq!(ben.texts(), vec![json!({"t": "seats", "room": "HOST_Ben_1", "names": ["Pia"], "of": 2})]);
    // Quinn goes Back, Rex sits down and his game crashes
    t.cancel(&quinn);
    assert_eq!(ann.texts(), ann_seats(&[]));
    pick(&mut t, &rex, gid(1));
    assert_eq!(ann.texts(), ann_seats(&["Rex"]));
    t.disconnect(&rex);
    assert_eq!(ann.texts(), ann_seats(&[]));
    assert!(ben.texts().is_empty());
}

/// A player of an older game seated by the relay shows in the seats too, and leaves them when a
/// game from their own network appears (that host hears them arrive).
#[test]
fn an_older_player_shows_in_the_seats_and_leaves_them_for_a_game_at_home() {
    let mut t = T::new(Config::default());
    let ann = t.host_approving("Ann", 2, "82.0.0.1");
    let quinn = t.player_at("Quinn", "82.0.0.4");
    t.quick_match(&quinn);
    let mut list = Vec::new();
    waiting_list(&quinn.msgs(), &mut list);
    assert_eq!(names(&list), ["Ann"]);
    assert_eq!(seats(&ann.texts()), Some(vec!["Quinn"]));
    let sis = open_host(&mut t, "Sis", "HOST_Sis_1", 1, true, "82.0.0.4");
    assert_eq!(seats(&sis.texts()), Some(vec!["Quinn"]));
    assert_eq!(seats(&ann.texts()), Some(vec![]));
    let m = quinn.msgs();
    waiting_list(&m, &mut list);
    assert_eq!(names(&list), ["Sis"]);
    assert!(!got_lock(&m), "Sis's game is full but waits for Sis");
}

/// Nothing is resent while a host's seats stay the same: changes in other games, a pick of the
/// game a player already sits in, a refused pick.
#[test]
fn seats_are_sent_only_when_they_change() {
    let mut t = T::new(Config::default());
    let ann = t.host_approving("Ann", 2, "82.0.0.1");
    t.tick(1_000);
    let ben = t.host_approving("Ben", 1, "82.0.0.2");
    let pia = waiting_chooser(&mut t, "Pia", "82.0.0.3");
    pick(&mut t, &pia, gid(1));
    ann.take();
    pick(&mut t, &pia, gid(1)); // already there
    let quinn = waiting_chooser(&mut t, "Quinn", "82.0.0.4");
    pick(&mut t, &quinn, gid(2));
    kick(&mut t, &ben, "Quinn");
    let rex = waiting_chooser(&mut t, "Rex", "82.0.0.5");
    pick(&mut t, &rex, 12_345); // no such game
    pick(&mut t, &rex, gid(2)); // Ben's game, free again
    assert!(ann.texts().is_empty(), "Ann's seats did not change");
    let tom = t.player_at("Tom", "82.0.0.6"); // an older game: Ann's is the only free seat
    t.quick_match(&tom);
    assert_eq!(ann.texts(), vec![json!({"t": "seats", "room": "HOST_Ann_1", "names": ["Pia", "Tom"], "of": 2})]);
}

// ------------------------------------------------------------------ start

/// `start` before every seat is taken does nothing, but the host hears its seats again (its panel
/// shows START again): also when a player went Back just before the host's OK.
#[test]
fn start_before_all_seats_are_taken_is_ignored_and_the_host_hears_who_is_there() {
    let mut t = T::new(Config::default());
    let ann = t.host_approving("Ann", 2, "82.0.0.1");
    start(&mut t, &ann);
    assert_eq!(ann.texts(), vec![json!({"t": "seats", "room": "HOST_Ann_1", "names": [], "of": 2})]);
    let pia = waiting_chooser(&mut t, "Pia", "82.0.0.3");
    pick(&mut t, &pia, gid(1));
    pia.take();
    ann.take();
    start(&mut t, &ann);
    assert!(!got_lock(&pia.msgs()));
    assert_eq!(seats(&ann.texts()), Some(vec!["Pia"]));
    // full for a moment: Quinn goes Back before the host's OK arrives
    let quinn = waiting_chooser(&mut t, "Quinn", "82.0.0.4");
    pick(&mut t, &quinn, gid(1));
    assert_eq!(seats(&ann.texts()), Some(vec!["Pia", "Quinn"]));
    t.cancel(&quinn);
    ann.take();
    start(&mut t, &ann);
    assert!(!got_lock(&pia.msgs()) && !got_lock(&quinn.msgs()));
    assert_eq!(seats(&ann.texts()), Some(vec!["Pia"]), "the host's panel learns why");
    assert_eq!(t.relay.status()["hosted"], 1);
}

/// `start` on a full game: every seated player (choosers and older games) is locked and matched
/// up, the choosers' lists turn off, and the game is offered to no one any more.
#[test]
fn starting_a_full_game_matches_everyone_in_it_and_takes_it_off_the_lists() {
    let mut t = T::new(Config::default());
    let ann = t.host_approving("Ann", 2, "82.0.0.1");
    let rex = t.chooser_at("Rex", "82.0.0.5");
    t.quick_match(&rex);
    assert_eq!(offered(&last_offer(&rex).unwrap()), ["Ann"]);
    let pia = waiting_chooser(&mut t, "Pia", "82.0.0.3");
    pick(&mut t, &pia, gid(1));
    let quinn = t.player_at("Quinn", "82.0.0.4"); // an older game: seated by the relay
    t.quick_match(&quinn);
    assert!(offered(&last_offer(&rex).unwrap()).is_empty(), "full: no longer offered to Rex");
    assert_eq!(seats(&ann.texts()), Some(vec!["Pia", "Quinn"]));
    pia.take();
    quinn.take();
    start(&mut t, &ann);
    let (texts, m) = pia.split();
    assert!(got_lock(&m));
    assert_eq!(last_games(&texts), Some(&json!({"t": "games", "on": false})));
    assert!(got_lock(&quinn.msgs()));
    assert!(rex.texts().is_empty(), "Rex's list did not change");
    assert_eq!(t.relay.status()["hosted"], 0);
    t.lock_ok(&pia);
    t.lock_ok(&quinn);
    assert_eq!(matched_room(&pia.msgs()).as_deref(), Some("HOST_Ann_1"));
    assert_eq!(matched_room(&quinn.msgs()).as_deref(), Some("HOST_Ann_1"));
    // Rex picks it from an old list: refused, and he is not in that match
    pick(&mut t, &rex, gid(1));
    let (texts, m) = rex.split();
    assert!(!got_lock(&m));
    let g = last_games(&texts).unwrap();
    assert_eq!(g["seat"], 0);
    assert!(offered(g).is_empty());
    // newcomers do not get it either
    let sam = t.chooser_at("Sam", "82.0.0.6");
    t.quick_match(&sam);
    assert!(offered(&last_offer(&sam).unwrap()).is_empty());
    let tom = t.player_at("Tom", "82.0.0.7");
    t.quick_match(&tom);
    assert!(!got_lock(&tom.msgs()));
}

/// A second `start` (the host pressed OK twice, or after the match began) starts nothing more.
#[test]
fn a_second_start_does_nothing() {
    let mut t = T::new(Config::default());
    let ann = t.host_approving("Ann", 1, "82.0.0.1");
    let pia = waiting_chooser(&mut t, "Pia", "82.0.0.3");
    pick(&mut t, &pia, gid(1));
    pia.take();
    start(&mut t, &ann);
    assert_eq!(locks(&pia.msgs()), 1);
    ann.take();
    start(&mut t, &ann);
    assert_eq!(locks(&pia.msgs()), 0, "no second lock");
    assert!(ann.texts().is_empty());
    t.lock_ok(&pia);
    let m = pia.msgs();
    assert_eq!(matched_ups(&m), 1);
    assert_eq!(matched_room(&m).as_deref(), Some("HOST_Ann_1"));
    start(&mut t, &ann);
    t.tick(5_000);
    let m = pia.msgs();
    assert_eq!((locks(&m), matched_ups(&m)), (0, 0));
}

/// `start` from a seated player, a lobby player, a host link that serves nothing, another host or
/// a connection that did not say hello starts nothing; another host's start only concerns its
/// own game.
#[test]
fn start_from_a_player_or_another_host_starts_nothing() {
    let mut t = T::new(Config::default());
    let ann = t.host_approving("Ann", 1, "82.0.0.1");
    t.tick(1_000);
    let ben = t.host_approving("Ben", 2, "82.0.0.2");
    let pia = waiting_chooser(&mut t, "Pia", "82.0.0.3");
    pick(&mut t, &pia, gid(1)); // Ann's game is full
    pia.take();
    ann.take();
    ben.take();
    start(&mut t, &pia);
    let lou = t.player_at("Lou", "82.0.0.9");
    start(&mut t, &lou);
    let spare = t.hello("RGHost");
    start(&mut t, &spare);
    assert!(spare.texts().is_empty());
    start(&mut t, &ben);
    assert_eq!(seats(&ben.texts()), Some(vec![]), "Ben's game is not full");
    let anon = t.conn();
    start(&mut t, &anon);
    assert_eq!(anon.texts()[0]["code"], "not_logged_in");
    assert!(!got_lock(&pia.msgs()));
    assert!(ann.texts().is_empty());
    assert_eq!(t.relay.status()["hosted"], 2);
    // Ben's start once his game is full starts his game only
    let quinn = waiting_chooser(&mut t, "Quinn", "82.0.0.4");
    pick(&mut t, &quinn, gid(2));
    let rex = waiting_chooser(&mut t, "Rex", "82.0.0.5");
    pick(&mut t, &rex, gid(2));
    start(&mut t, &ben);
    assert!(got_lock(&quinn.msgs()) && got_lock(&rex.msgs()));
    assert!(!got_lock(&pia.msgs()));
    start(&mut t, &ann);
    assert!(got_lock(&pia.msgs()));
}

/// `start` from an older host (no approve) before its game is full does nothing either.
#[test]
fn an_older_host_starts_by_itself_when_full_and_only_then() {
    let mut t = T::new(Config::default());
    let old = t.host_at("Old", 2, 3, "82.0.0.1");
    let pia = t.chooser_at("Pia", "82.0.0.3");
    t.quick_match(&pia);
    let g = last_offer(&pia).unwrap();
    assert_eq!(g["games"][0], json!([gid(1), "Old", 3, 0, 2, 0, 0]), "ask 0: starts when all seats are taken");
    pick(&mut t, &pia, gid(1));
    start(&mut t, &old);
    assert!(!got_lock(&pia.msgs()));
    assert!(old.texts().is_empty(), "no seats for an older host");
    let quinn = t.player_at("Quinn", "82.0.0.4");
    t.quick_match(&quinn);
    let (texts, m) = pia.split();
    assert!(got_lock(&m), "full: it starts by itself");
    assert_eq!(last_games(&texts).unwrap()["on"], false);
    assert!(got_lock(&quinn.msgs()));
    t.lock_ok(&pia);
    t.lock_ok(&quinn);
    assert_eq!(matched_room(&pia.msgs()).as_deref(), Some("HOST_Old_1"));
}

/// A full game whose host starts it never starts by itself, however long it waits.
#[test]
fn an_approving_host_s_full_game_never_starts_by_itself() {
    let mut t = T::new(Config::default());
    let ann = t.host_approving("Ann", 2, "82.0.0.1");
    let quinn = t.player_at("Quinn", "82.0.0.4");
    t.quick_match(&quinn);
    let rex = t.player_at("Rex", "82.0.0.5");
    t.quick_match(&rex);
    assert_eq!(seats(&ann.texts()), Some(vec!["Quinn", "Rex"]));
    quinn.take();
    rex.take();
    later(&mut t, 3_600_000, &[&ann, &quinn, &rex]);
    assert!(!got_lock(&quinn.msgs()) && !got_lock(&rex.msgs()));
    assert_eq!(t.relay.status()["hosted"], 1);
    start(&mut t, &ann);
    assert!(got_lock(&quinn.msgs()) && got_lock(&rex.msgs()));
}

/// A host whose game quits right after its OK (between `start` and the players' lock answers):
/// its players are not sent into a room nobody serves any more; their connections are closed
/// (their game says so) instead of leaving them on "Game starting". A host who stops there (Esc)
/// the same.
#[test]
fn a_host_who_quits_while_its_game_starts_sends_no_one_into_its_room() {
    for stops in [false, true] {
        let mut t = T::new(Config::default());
        let ann = t.host_approving("Ann", 1, "82.0.0.1");
        let pia = waiting_chooser(&mut t, "Pia", "82.0.0.3");
        pick(&mut t, &pia, gid(1));
        pia.take();
        start(&mut t, &ann);
        assert!(got_lock(&pia.msgs()));
        if stops {
            t.send(&ann, json!({"t": "unserve"}));
        } else {
            t.disconnect(&ann);
        }
        t.lock_ok(&pia);
        assert_eq!(pia.closed(), Some(4003), "stops {stops}: Pia's game hears the host is gone");
        assert_eq!(matched_room(&pia.msgs()), None, "stops {stops}: HOST_Ann_1 has no game server any more");
        // back with a new connection: an ordinary Quick Match (the host is gone)
        let pia2 = t.chooser_at("Pia", "82.0.0.3");
        t.quick_match(&pia2);
        let (texts, m) = pia2.split();
        assert!(!got_lock(&m), "stops {stops}");
        assert_eq!(last_games(&texts).unwrap()["games"], json!([]), "stops {stops}");
    }
}

// ------------------------------------------------------------------ kick

/// `kick` from a player or from another host removes no one, and leaves no mark: the player may
/// still sit in either game.
#[test]
fn kick_from_a_player_or_another_host_removes_no_one() {
    let mut t = T::new(Config::default());
    let ann = t.host_approving("Ann", 2, "82.0.0.1");
    t.tick(1_000);
    let ben = t.host_approving("Ben", 2, "82.0.0.2");
    let pia = waiting_chooser(&mut t, "Pia", "82.0.0.3");
    pick(&mut t, &pia, gid(1));
    let quinn = waiting_chooser(&mut t, "Quinn", "82.0.0.4");
    pia.take();
    ann.take();
    ben.take();
    kick(&mut t, &quinn, "Pia");
    let spare = t.hello("RGHost");
    kick(&mut t, &spare, "Pia");
    assert!(ann.texts().is_empty() && pia.texts().is_empty());
    kick(&mut t, &ben, "Pia");
    assert_eq!(seats(&ben.texts()), Some(vec![]), "Ben hears his own seats again");
    assert!(ann.texts().is_empty(), "Pia is still in Ann's game");
    assert!(pia.texts().is_empty());
    pick(&mut t, &pia, gid(2));
    assert_eq!(seats(&ben.texts()), Some(vec!["Pia"]));
    pick(&mut t, &pia, gid(1));
    assert_eq!(seats(&ann.texts()), Some(vec!["Pia"]));
}

/// A kick of a name not seated in the host's game (unknown, in another game, the host's own)
/// changes nothing and gives the host its seats again; a kick without a name is ignored.
#[test]
fn kicking_an_unknown_name_or_a_player_of_another_game_changes_nothing() {
    let mut t = T::new(Config::default());
    let ann = t.host_approving("Ann", 2, "82.0.0.1");
    t.tick(1_000);
    let ben = t.host_approving("Ben", 2, "82.0.0.2");
    let pia = waiting_chooser(&mut t, "Pia", "82.0.0.3");
    pick(&mut t, &pia, gid(1));
    let quinn = waiting_chooser(&mut t, "Quinn", "82.0.0.4");
    pick(&mut t, &quinn, gid(2));
    for c in [&ann, &ben, &pia, &quinn] {
        c.take();
    }
    for name in ["Nobody", "Quinn", "Ann", "RGHost", ""] {
        kick(&mut t, &ann, name);
        assert_eq!(ann.texts(), vec![json!({"t": "seats", "room": "HOST_Ann_1", "names": ["Pia"], "of": 2})], "{name}");
    }
    t.send(&ann, json!({"t": "kick"}));
    t.send(&ann, json!({"t": "kick", "name": 7}));
    assert!(ann.texts().is_empty());
    assert!(ben.texts().is_empty() && quinn.texts().is_empty() && pia.texts().is_empty());
    // Quinn was not sent away from Ann's game
    pick(&mut t, &quinn, gid(1));
    assert_eq!(seats(&ann.texts()), Some(vec!["Pia", "Quinn"]));
    assert_eq!(seats(&ben.texts()), Some(vec![]));
}

/// The kick names exactly one player (case aside), not one whose name only starts alike.
#[test]
fn kick_removes_exactly_the_named_player() {
    let mut t = T::new(Config::default());
    let ann = t.host_approving("Ann", 3, "82.0.0.1");
    let p1 = waiting_chooser(&mut t, "Pia", "82.0.0.3");
    let p2 = waiting_chooser(&mut t, "pia", "82.0.0.4"); // the relay calls this one pia2
    pick(&mut t, &p1, gid(1));
    pick(&mut t, &p2, gid(1));
    assert_eq!(seats(&ann.texts()), Some(vec!["Pia", "pia2"]));
    p2.take();
    kick(&mut t, &ann, "PIA");
    assert_eq!(seats(&ann.texts()), Some(vec!["pia2"]));
    assert_eq!(last_offer(&p1).unwrap()["seat"], 0);
    assert!(p2.texts().iter().all(|m| m["t"] != "games" || m["seat"] == gid(1)), "pia2 keeps her seat");
    kick(&mut t, &ann, "Pia2");
    assert_eq!(seats(&ann.texts()), Some(vec![]));
}

/// A kicked chooser waits without a seat; that game is gone from their list and screen and a
/// pick of it (an old list) is refused; another game takes them.
#[test]
fn a_kicked_chooser_waits_without_a_seat_and_is_not_offered_that_game() {
    let mut t = T::new(Config::default());
    let ann = t.host_approving("Ann", 2, "82.0.0.1");
    t.tick(1_000);
    let ben = t.host_approving("Ben", 2, "82.0.0.2");
    let pia = waiting_chooser(&mut t, "Pia", "82.0.0.3");
    let mut list = Vec::new();
    pick(&mut t, &pia, gid(1));
    waiting_list(&pia.msgs(), &mut list);
    assert_eq!(names(&list), ["Ann"]);
    ann.take();
    kick(&mut t, &ann, "Pia");
    assert_eq!(seats(&ann.texts()), Some(vec![]));
    let (texts, m) = pia.split();
    let g = last_games(&texts).unwrap();
    assert_eq!((g["on"].clone(), g["seat"].clone()), (json!(true), json!(0)), "still choosing, no seat");
    assert_eq!(offered(g), ["Ben"], "Ann's game is not offered to Pia");
    waiting_list(&m, &mut list);
    assert!(list.is_empty(), "Ann is gone from Pia's screen");
    pick(&mut t, &pia, gid(1));
    let g = last_offer(&pia).unwrap();
    assert_eq!(g["seat"], 0);
    assert_eq!(offered(&g), ["Ben"]);
    assert!(ann.texts().is_empty());
    ben.take();
    pick(&mut t, &pia, gid(2));
    assert_eq!(last_offer(&pia).unwrap()["seat"], gid(2));
    assert_eq!(seats(&ben.texts()), Some(vec!["Pia"]));
}

/// A kicked player of an older game is seated in another game right away, or waits; Back and
/// Quick Match again does not put them back into a game that sent them away.
#[test]
fn a_kicked_older_player_goes_to_another_game_or_waits() {
    let mut t = T::new(Config::default());
    let ann = t.host_approving("Ann", 1, "82.0.0.1");
    t.tick(1_000);
    let ben = t.host_approving("Ben", 1, "82.0.0.2");
    let quinn = t.player_at("Quinn", "82.0.0.4");
    t.quick_match(&quinn);
    let mut list = Vec::new();
    waiting_list(&quinn.msgs(), &mut list);
    assert_eq!(names(&list), ["Ben"], "the newest game");
    ann.take();
    ben.take();
    kick(&mut t, &ben, "Quinn");
    assert_eq!(seats(&ben.texts()), Some(vec![]));
    assert_eq!(seats(&ann.texts()), Some(vec!["Quinn"]));
    waiting_list(&quinn.msgs(), &mut list);
    assert_eq!(names(&list), ["Ann"]);
    kick(&mut t, &ann, "Quinn");
    assert_eq!(seats(&ann.texts()), Some(vec![]));
    waiting_list(&quinn.msgs(), &mut list);
    assert!(list.is_empty(), "sent away from both: Quinn waits");
    again(&mut t, &quinn);
    let m = quinn.msgs();
    waiting_list(&m, &mut list);
    assert!(list.is_empty() && !got_lock(&m));
    assert!(ann.texts().is_empty() && ben.texts().is_empty());
    // the next player still gets those games
    let rex = t.player_at("Rex", "82.0.0.5");
    t.quick_match(&rex);
    assert_eq!(seats(&ben.texts()), Some(vec!["Rex"]));
}

/// A kick frees a seat: the choosers waiting are offered that game again.
#[test]
fn a_kick_frees_a_seat_that_others_are_offered() {
    let mut t = T::new(Config::default());
    let ann = t.host_approving("Ann", 1, "82.0.0.1");
    let pia = waiting_chooser(&mut t, "Pia", "82.0.0.3");
    pick(&mut t, &pia, gid(1));
    let rex = t.chooser_at("Rex", "82.0.0.5");
    t.quick_match(&rex);
    assert!(offered(&last_offer(&rex).unwrap()).is_empty(), "full");
    kick(&mut t, &ann, "Pia");
    assert_eq!(offered(&last_offer(&rex).unwrap()), ["Ann"]);
    assert!(offered(&last_offer(&pia).unwrap()).is_empty());
    ann.take();
    pick(&mut t, &rex, gid(1));
    assert_eq!(seats(&ann.texts()), Some(vec!["Rex"]));
}

/// The seat a kicked older player leaves goes to the next player waiting, in one `seats` update.
#[test]
fn a_kicked_player_s_seat_goes_to_the_next_one_waiting() {
    let mut t = T::new(Config::default());
    let ann = t.host_approving("Ann", 2, "82.0.0.1");
    let pia = waiting_chooser(&mut t, "Pia", "82.0.0.3");
    pick(&mut t, &pia, gid(1));
    let quinn = t.player_at("Quinn", "82.0.0.4");
    t.quick_match(&quinn);
    let tom = t.player_at("Tom", "82.0.0.6"); // no free seat: waits
    t.quick_match(&tom);
    assert_eq!(seats(&ann.texts()), Some(vec!["Pia", "Quinn"]));
    quinn.take();
    tom.take();
    kick(&mut t, &ann, "Quinn");
    assert_eq!(ann.texts(), vec![json!({"t": "seats", "room": "HOST_Ann_1", "names": ["Pia", "Tom"], "of": 2})]);
    assert!(!got_lock(&tom.msgs()), "full, but the host starts it");
    assert!(!got_lock(&quinn.msgs()));
}

/// The kicked player goes to the front of the queue: the next game goes to them before a player
/// who has waited longer.
#[test]
fn the_kicked_player_goes_to_the_front_of_the_queue() {
    let mut t = T::new(Config::default());
    let ann = t.host_approving("Ann", 1, "82.0.0.1");
    let sam = t.player_at("Sam", "82.0.0.6");
    t.quick_match(&sam);
    kick(&mut t, &ann, "Sam"); // Sam waits
    let quinn = t.player_at("Quinn", "82.0.0.4");
    t.quick_match(&quinn); // seated in Ann's game: Sam may not
    assert_eq!(seats(&ann.texts()), Some(vec!["Quinn"]));
    kick(&mut t, &ann, "Quinn"); // now Quinn waits, ahead of Sam
    assert_eq!(t.relay.status()["queued"], 2);
    sam.take();
    quinn.take();
    t.host_at("Ben", 1, 2, "82.0.0.2");
    assert!(got_lock(&quinn.msgs()), "the kicked player first");
    assert!(!got_lock(&sam.msgs()));
}

/// A second kick of the same player finds no one: the host hears its seats again, the player is
/// queued once.
#[test]
fn kicking_a_player_twice() {
    let mut t = T::new(Config::default());
    let ann = t.host_approving("Ann", 1, "82.0.0.1");
    let pia = waiting_chooser(&mut t, "Pia", "82.0.0.3");
    pick(&mut t, &pia, gid(1));
    kick(&mut t, &ann, "Pia");
    ann.take();
    kick(&mut t, &ann, "Pia");
    assert_eq!(seats(&ann.texts()), Some(vec![]));
    assert_eq!(t.relay.status()["queued"], 1);
    let rex = waiting_chooser(&mut t, "Rex", "82.0.0.5");
    pick(&mut t, &rex, gid(1));
    assert_eq!(seats(&ann.texts()), Some(vec!["Rex"]));
    assert_eq!(t.relay.status()["queued"], 2);
}

/// A kick after the host's OK (the game is starting) removes no one.
#[test]
fn a_kick_after_the_start_removes_no_one() {
    let mut t = T::new(Config::default());
    let ann = t.host_approving("Ann", 1, "82.0.0.1");
    let pia = waiting_chooser(&mut t, "Pia", "82.0.0.3");
    pick(&mut t, &pia, gid(1));
    start(&mut t, &ann);
    assert!(got_lock(&pia.msgs()));
    ann.take();
    kick(&mut t, &ann, "Pia");
    assert!(ann.texts().is_empty());
    t.lock_ok(&pia);
    assert_eq!(matched_room(&pia.msgs()).as_deref(), Some("HOST_Ann_1"));
}

/// A kicked player who comes back with a new connection (same name) is still not offered that
/// game.
#[test]
fn a_kicked_player_who_reconnects_is_still_not_offered_that_game() {
    let mut t = T::new(Config::default());
    let ann = t.host_approving("Ann", 1, "82.0.0.1");
    let pia = waiting_chooser(&mut t, "Pia", "82.0.0.3");
    pick(&mut t, &pia, gid(1));
    kick(&mut t, &ann, "Pia");
    t.disconnect(&pia);
    let pia2 = t.chooser_at("Pia", "82.0.0.33");
    t.quick_match(&pia2);
    assert!(offered(&last_offer(&pia2).unwrap()).is_empty());
    let rex = t.chooser_at("Rex", "82.0.0.5");
    t.quick_match(&rex);
    assert_eq!(offered(&last_offer(&rex).unwrap()), ["Ann"]);
}

/// The kick lasts 10 minutes: at 9:50 the game is still not offered (chooser) nor given (older
/// game); at 10:10 Quick Match seats the older player and offers it to the chooser again.
#[test]
fn the_kick_lasts_ten_minutes() {
    let mut t = T::new(Config::default());
    let ann = t.host_approving("Ann", 2, "82.0.0.1");
    let pia = waiting_chooser(&mut t, "Pia", "82.0.0.3");
    pick(&mut t, &pia, gid(1));
    let quinn = t.player_at("Quinn", "82.0.0.4");
    t.quick_match(&quinn);
    kick(&mut t, &ann, "Pia");
    kick(&mut t, &ann, "Quinn");
    for c in [&ann, &pia, &quinn] {
        c.take();
    }
    later(&mut t, 590_000, &[&ann, &pia, &quinn]);
    again(&mut t, &pia);
    let g = last_offer(&pia).unwrap();
    assert_eq!(g["on"], true);
    assert!(offered(&g).is_empty(), "9:50: still not offered");
    again(&mut t, &quinn);
    let m = quinn.msgs();
    let mut list = Vec::new();
    waiting_list(&m, &mut list);
    assert!(!names(&list).contains(&"Ann") && !got_lock(&m), "9:50: still not seated");
    assert_eq!(seats(&ann.texts()), None);
    later(&mut t, 20_000, &[&ann, &pia, &quinn]);
    again(&mut t, &quinn);
    waiting_list(&quinn.msgs(), &mut list);
    assert_eq!(names(&list), ["Ann"], "10:10: seated again");
    assert_eq!(seats(&ann.texts()), Some(vec!["Quinn"]));
    assert_eq!(offered(&last_offer(&pia).unwrap()), ["Ann"], "offered again");
    pick(&mut t, &pia, gid(1));
    assert_eq!(last_offer(&pia).unwrap()["seat"], gid(1));
    assert_eq!(seats(&ann.texts()), Some(vec!["Quinn", "Pia"]));
}

/// Kicked players who just keep waiting get that game back once the 10 minutes are over: the
/// chooser's list offers it again, the older game's player is seated in it.
#[test]
fn waiting_kicked_players_get_the_game_back_when_the_ten_minutes_are_over() {
    let mut t = T::new(Config::default());
    let ann = t.host_approving("Ann", 2, "82.0.0.1");
    let pia = waiting_chooser(&mut t, "Pia", "82.0.0.3");
    pick(&mut t, &pia, gid(1));
    let quinn = t.player_at("Quinn", "82.0.0.4");
    t.quick_match(&quinn);
    kick(&mut t, &ann, "Pia");
    kick(&mut t, &ann, "Quinn");
    for c in [&ann, &pia, &quinn] {
        c.take();
    }
    later(&mut t, 11 * 60_000, &[&ann, &pia, &quinn]);
    let g = last_offer(&pia).expect("Pia's list changes when the kick runs out");
    assert_eq!(offered(&g), ["Ann"]);
    assert_eq!(seats(&ann.texts()), Some(vec!["Quinn"]), "Quinn waits for a game, Ann's has a free seat");
}

// ------------------------------------------------------------------ hosts that stop or come back

/// The host stops (Esc): its seated choosers wait again without a seat and choose again, its
/// older players get the next game; no more `seats` for that host.
#[test]
fn the_choosers_of_a_host_who_stops_choose_again() {
    let mut t = T::new(Config::default());
    let ann = t.host_approving("Ann", 1, "82.0.0.1");
    let pia = waiting_chooser(&mut t, "Pia", "82.0.0.3");
    let mut list = Vec::new();
    pick(&mut t, &pia, gid(1));
    waiting_list(&pia.msgs(), &mut list);
    assert_eq!(names(&list), ["Ann"]);
    let rex = t.player_at("Rex", "82.0.0.5"); // an older game, no free seat: waits
    t.quick_match(&rex);
    ann.take();
    t.send(&ann, json!({"t": "unserve"}));
    assert_eq!(seats(&ann.texts()), None);
    let (texts, m) = pia.split();
    assert_eq!(last_games(&texts), Some(&json!({"t": "games", "on": true, "seat": 0, "games": []})));
    waiting_list(&m, &mut list);
    assert_eq!(names(&list), ["Rex"], "Pia now waits with Rex");
    assert_eq!(t.relay.status()["hosted"], 0);
    assert_eq!(t.relay.status()["queued"], 2);
    // a new game: Rex is seated by the relay, Pia is offered it
    let ben = open_host(&mut t, "Ben", "HOST_Ben_1", 2, true, "82.0.0.2");
    assert_eq!(seats(&ben.texts()), Some(vec!["Rex"]));
    let g = last_offer(&pia).unwrap();
    assert_eq!((g["seat"].clone(), offered(&g)), (json!(0), vec!["Ben"]));
    pick(&mut t, &pia, gid(2));
    assert_eq!(seats(&ben.texts()), Some(vec!["Rex", "Pia"]));
}

/// The older players of a host who stops go to the front of the queue.
#[test]
fn the_players_of_a_host_who_stops_go_to_the_front_of_the_queue() {
    let mut t = T::new(Config::default());
    let ann = t.host_approving("Ann", 1, "82.0.0.1");
    let quinn = t.player_at("Quinn", "82.0.0.4");
    t.quick_match(&quinn);
    let rex = t.player_at("Rex", "82.0.0.5"); // waits: Ann's game is full
    t.quick_match(&rex);
    t.send(&ann, json!({"t": "unserve"}));
    assert_eq!(t.relay.status()["queued"], 2);
    quinn.take();
    rex.take();
    t.host_at("Ben", 1, 2, "82.0.0.2");
    assert!(got_lock(&quinn.msgs()), "Quinn had a seat: first");
    assert!(!got_lock(&rex.msgs()));
}

/// The host's game crashes with players seated: the older one goes to the other game, the chooser
/// waits and is offered it; both screens lose the host.
#[test]
fn the_players_of_a_host_whose_game_crashes() {
    let mut t = T::new(Config::default());
    let ben = t.host_approving("Ben", 2, "82.0.0.2");
    t.tick(1_000);
    let ann = t.host_approving("Ann", 2, "82.0.0.1");
    let pia = waiting_chooser(&mut t, "Pia", "82.0.0.3");
    pick(&mut t, &pia, gid(2));
    let quinn = t.player_at("Quinn", "82.0.0.4");
    t.quick_match(&quinn);
    let (mut plist, mut qlist) = (Vec::new(), Vec::new());
    waiting_list(&pia.msgs(), &mut plist);
    waiting_list(&quinn.msgs(), &mut qlist);
    assert_eq!(names(&plist), ["Ann", "Quinn"]);
    assert_eq!(names(&qlist), ["Ann", "Pia"]);
    assert_eq!(seats(&ann.texts()), Some(vec!["Pia", "Quinn"]));
    ben.take();
    t.disconnect(&ann);
    assert_eq!(seats(&ben.texts()), Some(vec!["Quinn"]));
    waiting_list(&quinn.msgs(), &mut qlist);
    assert_eq!(names(&qlist), ["Ben"]);
    let (texts, m) = pia.split();
    let g = last_games(&texts).unwrap();
    assert_eq!((g["seat"].clone(), offered(g)), (json!(0), vec!["Ben"]));
    waiting_list(&m, &mut plist);
    assert!(plist.is_empty());
    assert_eq!(t.relay.status()["hosted"], 1);
}

/// After stopping, the same host link serves its next game: a new list id, offered again, and
/// it starts as usual.
#[test]
fn a_host_who_stopped_hosts_the_next_game() {
    let mut t = T::new(Config::default());
    let ann = t.host_approving("Ann", 1, "82.0.0.1");
    let pia = waiting_chooser(&mut t, "Pia", "82.0.0.3");
    pick(&mut t, &pia, gid(1));
    t.send(&ann, json!({"t": "unserve"}));
    assert_eq!(last_offer(&pia).unwrap()["seat"], 0);
    ann.take();
    serve(&mut t, &ann, "Ann", "HOST_Ann_2", 1, true);
    let texts = ann.texts();
    assert_eq!(texts[0], json!({"t": "serving", "room": "HOST_Ann_2"}));
    assert_eq!(seats(&texts), Some(vec![]));
    let g = last_offer(&pia).unwrap();
    assert_eq!(g["seat"], 0, "Pia chooses again");
    assert_eq!(g["games"][0][0], gid(2));
    pick(&mut t, &pia, gid(1)); // the old game is gone
    assert_eq!(last_offer(&pia).unwrap()["seat"], 0);
    pick(&mut t, &pia, gid(2));
    assert_eq!(ann.texts(), vec![json!({"t": "seats", "room": "HOST_Ann_2", "names": ["Pia"], "of": 1})]);
    pia.take();
    start(&mut t, &ann);
    assert!(got_lock(&pia.msgs()));
    t.lock_ok(&pia);
    assert_eq!(matched_room(&pia.msgs()).as_deref(), Some("HOST_Ann_2"));
}

/// Serving the next game without stopping first replaces the waiting one.
#[test]
fn serving_the_next_game_replaces_the_waiting_one() {
    let mut t = T::new(Config::default());
    let ann = t.host_approving("Ann", 1, "82.0.0.1");
    let pia = waiting_chooser(&mut t, "Pia", "82.0.0.3");
    pick(&mut t, &pia, gid(1));
    let quinn = t.player_at("Quinn", "82.0.0.4"); // waits
    t.quick_match(&quinn);
    ann.take();
    serve(&mut t, &ann, "Ann", "HOST_Ann_2", 1, true);
    assert_eq!(t.relay.status()["hosted"], 1);
    assert_eq!(seats(&ann.texts()), Some(vec!["Quinn"]), "the older player is seated in the new game");
    let g = last_offer(&pia).unwrap();
    assert_eq!(g["seat"], 0);
    assert!(offered(&g).is_empty(), "the new game is full");
    let mut list = Vec::new();
    waiting_list(&quinn.msgs(), &mut list);
    let ids: Vec<&str> = list.iter().map(|e| e.0.as_str()).collect();
    assert_eq!(ids, ["900000002"], "Quinn's screen shows the new game's host");
}

/// After a match the host hosts again (host=auto: unserve, then the next room); its player's
/// Play Again finds the new game, which starts as usual.
#[test]
fn after_a_match_the_host_hosts_again_and_its_player_finds_the_new_game() {
    let mut t = T::new(Config::default());
    let ann = t.host_approving("Ann", 1, "82.0.0.1");
    let pia = waiting_chooser(&mut t, "Pia", "82.0.0.3");
    pick(&mut t, &pia, gid(1));
    start(&mut t, &ann);
    t.lock_ok(&pia);
    assert_eq!(matched_room(&pia.msgs()).as_deref(), Some("HOST_Ann_1"));
    t.send(&pia, json!({"t": "move", "room": "HOST_Ann_1"}));
    t.bin(&pia, &frame(CH_TABLE, &Writer::new().u8(REQUEST).u8(13).u32(7).done()));
    assert!(ann.texts().contains(&json!({"t": "player_joined", "pid": 1, "name": "Pia"})));
    // the match is over: the host's listener closes and it hosts the next game
    t.send(&ann, json!({"t": "unserve"}));
    serve(&mut t, &ann, "Ann", "HOST_Ann_2", 1, true);
    let texts = ann.texts();
    assert_eq!(texts[0], json!({"t": "serving", "room": "HOST_Ann_2"}));
    assert_eq!(seats(&texts), Some(vec![]));
    // Pia's Play Again: back to the casino, Quick Match
    t.send(&pia, json!({"t": "move", "room": "CasinoSystem"}));
    pia.take();
    t.quick_match(&pia);
    let (texts, m) = pia.split();
    assert!(!got_lock(&m), "no way back into the finished match");
    let g = last_games(&texts).unwrap();
    assert_eq!((g["seat"].clone(), offered(g)), (json!(0), vec!["Ann"]));
    assert_eq!(g["games"][0][0], gid(2));
    pick(&mut t, &pia, gid(2));
    assert_eq!(seats(&ann.texts()), Some(vec!["Pia"]));
    start(&mut t, &ann);
    t.lock_ok(&pia);
    assert_eq!(matched_room(&pia.msgs()).as_deref(), Some("HOST_Ann_2"));
}

/// The host's link drops and its engine connects again (same room): the game is offered again,
/// its former choosers choose again, and players it sent away stay away.
#[test]
fn a_host_whose_link_drops_and_comes_back_is_offered_again() {
    let mut t = T::new(Config::default());
    let h1 = t.host_approving("Ann", 2, "82.0.0.1");
    let pia = waiting_chooser(&mut t, "Pia", "82.0.0.3");
    pick(&mut t, &pia, gid(1));
    let quinn = t.player_at("Quinn", "82.0.0.4");
    t.quick_match(&quinn);
    kick(&mut t, &h1, "Quinn");
    assert_eq!(seats(&h1.texts()), Some(vec!["Pia"]));
    t.disconnect(&h1);
    assert_eq!(last_offer(&pia).unwrap()["seat"], 0);
    quinn.take();
    let h2 = open_host(&mut t, "Ann", "HOST_Ann_1", 2, true, "82.0.0.1");
    let texts = h2.texts();
    assert_eq!(texts[0], json!({"t": "serving", "room": "HOST_Ann_1"}));
    assert_eq!(seats(&texts), Some(vec![]), "Quinn is still not seated there");
    let g = last_offer(&pia).unwrap();
    assert_eq!((g["seat"].clone(), offered(&g)), (json!(0), vec!["Ann"]));
    pick(&mut t, &pia, gid(2));
    assert_eq!(seats(&h2.texts()), Some(vec!["Pia"]));
    assert!(!got_lock(&quinn.msgs()));
}

/// The host's link comes back before the relay noticed the old one died (a phone changing
/// networks: the old TCP connection is half open). The engine sends `serve` again on every new
/// link, with its session's key: the new link takes the game over (same list id, its seated
/// players keep their seats and the new link hears them), the old one is closed.
#[test]
fn a_host_whose_link_comes_back_before_the_relay_notices_keeps_its_game() {
    let mut t = T::new(Config::default());
    let old = keyed_host(&mut t, "HOST_Ann_1", 2, KEY, false, "82.0.0.1");
    let pia = waiting_chooser(&mut t, "Pia", "82.0.0.3");
    pick(&mut t, &pia, gid(1));
    assert_eq!(seats(&old.texts()), Some(vec!["Pia"]));
    later(&mut t, 25_000, &[&pia]); // the old link is silent from now on
    pia.take();
    let new = keyed_host(&mut t, "HOST_Ann_1", 2, KEY, false, "82.0.0.7");
    assert_eq!(old.closed(), Some(4002), "the old link is replaced");
    let texts = new.texts();
    assert_eq!(texts[0], json!({"t": "serving", "room": "HOST_Ann_1"}));
    assert_eq!(seats(&texts), Some(vec!["Pia"]), "the new link hears who sits there");
    assert!(pia.texts().iter().all(|m| m["t"] != "games"), "nothing changes for Pia");
    assert_eq!(t.relay.status()["hosted"], 1);
    let rex = t.chooser_at("Rex", "82.0.0.5");
    t.quick_match(&rex);
    let g = last_offer(&rex).unwrap();
    assert_eq!(g["games"], json!([[gid(1), "Ann", 7, 1, 2, 0, 1]]));
    pick(&mut t, &rex, gid(1));
    assert_eq!(seats(&new.texts()), Some(vec!["Pia", "Rex"]));
    start(&mut t, &new);
    for c in [&pia, &rex] {
        t.lock_ok(c);
    }
    for c in [&pia, &rex] {
        assert_eq!(matched(&c.msgs()), Some(("HOST_Ann_1".into(), 2)));
    }
}

/// While a room is served, a `serve` for it with another key or without one is refused
/// (`taken`): the room, its game and its seats stay its host's.
#[test]
fn a_serve_with_another_key_or_none_is_taken() {
    for other in [Some("fedcba9876543210"), None] {
        let mut t = T::new(Config::default());
        let ann = keyed_host(&mut t, "HOST_Ann_1", 1, KEY, false, "82.0.0.1");
        let pia = waiting_chooser(&mut t, "Pia", "82.0.0.3");
        pick(&mut t, &pia, gid(1));
        ann.take();
        pia.take();
        let b = t.conn_from(Some("82.0.0.1"));
        t.send(&b, json!({"t": "hello", "v": 1, "name": "RGHost"}));
        b.take();
        let mut m = json!({"t": "serve", "room": "HOST_Ann_1", "players": 1, "name": "Ann", "approve": true});
        if let Some(k) = other {
            m["key"] = json!(k);
        }
        t.send(&b, m);
        assert_eq!(b.texts(), vec![json!({"t": "error", "op": "serve", "room": "HOST_Ann_1", "code": "taken"})]);
        assert_eq!(ann.closed(), None, "key {other:?}");
        assert!(ann.texts().is_empty(), "key {other:?}");
        assert_eq!(last_offer(&pia), None, "key {other:?}: Pia keeps her seat");
        assert_eq!(t.relay.status()["hosted"], 1);
        start(&mut t, &ann);
        t.lock_ok(&pia);
        assert_eq!(matched(&pia.msgs()), Some(("HOST_Ann_1".into(), 1)), "key {other:?}");
    }
}

/// A host's game that says its match is running (a link back after a relay restart, or late)
/// is served but never offered as a waiting game.
#[test]
fn a_running_serve_is_never_offered() {
    let mut t = T::new(Config::default());
    let ann = keyed_host(&mut t, "HOST_Ann_7", 2, KEY, true, "82.0.0.1");
    assert_eq!(ann.texts(), vec![json!({"t": "serving", "room": "HOST_Ann_7"})], "no seats: not a waiting game");
    assert_eq!(t.relay.status()["hosted"], 0);
    let pia = t.chooser_at("Pia", "82.0.0.1");
    t.quick_match(&pia);
    let (texts, m) = pia.split();
    assert_eq!(last_games(&texts).unwrap()["games"], json!([]));
    assert!(!got_lock(&m));
    let quinn = t.player_at("Quinn", "82.0.0.1");
    t.quick_match(&quinn);
    assert!(!got_lock(&quinn.msgs()));
    assert!(ann.texts().is_empty(), "no seats");
}

/// The host's link drops mid-match and comes back (same room and key, `running`): the new link
/// serves the room, but its game lost the players' sessions with the old link, so the players in
/// the room are disconnected (4004) and go back into the match with Quick Match; the match is
/// never offered as a waiting game.
#[test]
fn a_host_whose_link_comes_back_mid_match_takes_its_players_back() {
    let mut t = T::new(Config::default());
    let (ann, pia, rex) = keyed_match(&mut t);
    t.disconnect(&ann);
    t.tick(5_000);
    assert_eq!((pia.closed(), rex.closed()), (None, None), "they wait for the host");
    t.bin(&pia, &frame(CH_GAME, &[0, 1, 2, 3]));
    assert!(rex.take().is_empty(), "the match is not passed between the players");
    let ann2 = keyed_host(&mut t, "HOST_Ann_1", 2, KEY, true, "82.0.0.9");
    assert_eq!(ann2.texts(), vec![json!({"t": "serving", "room": "HOST_Ann_1"})], "no player_left: the new link has no sessions");
    assert_eq!((pia.closed(), rex.closed()), (Some(4004), Some(4004)));
    assert_eq!(t.relay.status()["hosted"], 0);
    let sue = t.chooser_at("Sue", "82.0.0.5");
    t.quick_match(&sue);
    assert_eq!(last_offer(&sue).unwrap()["games"], json!([]), "not offered");
    for (name, addr) in [("Pia", "82.0.0.3"), ("Rex", "82.0.0.4")] {
        let p = t.chooser_at(name, addr);
        t.quick_match(&p);
        assert!(got_lock(&p.msgs()), "{name} goes back into the match");
        t.lock_ok(&p);
        assert_eq!(matched(&p.msgs()), Some(("HOST_Ann_1".into(), 1)));
        t.send(&p, json!({"t": "move", "room": "HOST_Ann_1"}));
        t.bin(&p, &frame(CH_TABLE, &Writer::new().u8(REQUEST).u8(13).u32(7).done()));
        assert!(ann2.texts().iter().any(|m| m["t"] == "player_joined" && m["name"] == name));
        t.bin(&p, &frame(CH_GAME, &[9, 9]));
        assert!(!ann2.take().is_empty(), "{name}'s match reaches the host");
    }
}

/// The host's link drops mid-match and does not come back: after HOST_BACK_MS the players in its
/// room are disconnected (4003, their game says the connection is lost instead of showing a frozen
/// board) and the room is forgotten; a player who only arrives then is sent away too, and the
/// host's late serve of that room is a new game.
#[test]
fn a_host_who_does_not_come_back_ends_the_match() {
    let mut t = T::new(Config::default());
    let (ann, pia, rex) = keyed_match(&mut t);
    t.disconnect(&rex); // remembered: it may come back while the host does
    t.disconnect(&ann);
    later(&mut t, 10_000, &[&pia]);
    assert_eq!(pia.closed(), None);
    let rex2 = t.chooser_at("Rex", "82.0.0.4");
    t.quick_match(&rex2);
    assert!(!got_lock(&rex2.msgs()), "no way back while nobody serves the match");
    later(&mut t, 5_000, &[&pia, &rex2]);
    assert_eq!(pia.closed(), Some(4003), "the host is gone");
    let lister = t.hello("Lister");
    t.send(&lister, json!({"t": "rooms"}));
    let rooms = lister.texts()[0]["rooms"].clone();
    assert!(!rooms.as_array().unwrap().iter().any(|r| r["name"] == "HOST_Ann_1"), "{rooms}");
    // a player matched into it who arrives only now
    let late = t.chooser_at("Late", "82.0.0.6");
    t.send(&late, json!({"t": "move", "room": "HOST_Ann_1"}));
    assert_eq!(late.closed(), Some(4003));
    // Rex's next Quick Match is an ordinary one
    rex2.take();
    t.cancel(&rex2);
    t.quick_match(&rex2);
    assert!(!got_lock(&rex2.msgs()));
    // the host's game serves that room again: a new game, offered
    let ann2 = keyed_host(&mut t, "HOST_Ann_1", 1, KEY, false, "82.0.0.1");
    assert_eq!(ann2.texts()[0], json!({"t": "serving", "room": "HOST_Ann_1"}));
    assert_eq!(t.relay.status()["hosted"], 1);
}

/// A host who stops mid-match (`unserve`) without serving it again: the players still in its room
/// are disconnected after HOST_BACK_MS (a start that never comes, or an end screen), not at once;
/// players who left the room before are not.
#[test]
fn the_players_left_in_the_room_of_a_host_who_stopped_are_sent_away() {
    let mut t = T::new(Config::default());
    let (ann, pia, rex) = keyed_match(&mut t);
    t.send(&rex, json!({"t": "move", "room": "CasinoSystem"}));
    t.send(&ann, json!({"t": "unserve"}));
    later(&mut t, 10_000, &[&pia, &rex]);
    assert_eq!(pia.closed(), None);
    t.bin(&pia, &frame(CH_GAME, &[0, 1]));
    later(&mut t, 5_000, &[&pia, &rex]);
    assert_eq!(pia.closed(), Some(4003));
    assert_eq!(rex.closed(), None, "Rex had left the room");
}

/// Rex drops out of Ann's match while Ann's link is away and plays Ben's game meanwhile (Ben's
/// inbox is emptied); the way back into Ann's match is kept for when Ann is back.
fn rex_in_two_matches(t: &mut T) -> (C, C, C) {
    let (ann, _pia, rex) = keyed_match(t);
    t.disconnect(&ann);
    t.disconnect(&rex);
    t.tick(100);
    let ben = t.host_approving("Ben", 1, "82.0.0.9");
    let rex2 = waiting_chooser(t, "Rex", "82.0.0.4");
    pick(t, &rex2, gid(2));
    assert_eq!(seats(&ben.texts()), Some(vec!["Rex"]));
    (ann, ben, rex2)
}

/// A player with a way back into two matches (they dropped out of one while its host was away,
/// then of the next one) goes back into the one they dropped out of last, whatever the order of
/// the relay's maps.
#[test]
fn a_player_back_from_two_matches_goes_into_the_one_they_left_last() {
    let mut t = T::new(Config::default());
    let (_ann, ben, rex2) = rex_in_two_matches(&mut t);
    start(&mut t, &ben);
    t.lock_ok(&rex2);
    assert_eq!(matched_room(&rex2.msgs()).as_deref(), Some("HOST_Ben_1"));
    t.send(&rex2, json!({"t": "move", "room": "HOST_Ben_1"}));
    seat_take(&mut t, &rex2);
    t.tick(100);
    t.disconnect(&rex2);
    let ann2 = keyed_host(&mut t, "HOST_Ann_1", 2, KEY, true, "82.0.0.1");
    assert!(ann2.texts().contains(&json!({"t": "serving", "room": "HOST_Ann_1"})));
    let rex3 = t.chooser_at("Rex", "82.0.0.4");
    t.quick_match(&rex3);
    t.lock_ok(&rex3);
    assert_eq!(matched_room(&rex3.msgs()).as_deref(), Some("HOST_Ben_1"), "the last match");
    // done with Ben's match (left the normal way): Ann's is next
    t.send(&rex3, json!({"t": "move", "room": "HOST_Ben_1"}));
    seat_take(&mut t, &rex3);
    t.send(&rex3, json!({"t": "move", "room": "CasinoSystem"}));
    rex3.take();
    t.quick_match(&rex3);
    t.lock_ok(&rex3);
    assert_eq!(matched_room(&rex3.msgs()).as_deref(), Some("HOST_Ann_1"));
}

/// A player seated in a waiting game while their match's host was away, sent back into the match
/// once the host is back, frees the seat: a player waiting for a game at home gets it by itself.
#[test]
fn going_back_into_a_match_frees_the_seat_for_the_next_player() {
    let mut t = T::new(Config::default());
    let (_ann, ben, rex2) = rex_in_two_matches(&mut t);
    let sue = waiting_chooser(&mut t, "Sue", "82.0.0.9"); // Ben's home: his game is full
    assert_eq!(seats(&ben.texts()), None);
    let _ann2 = keyed_host(&mut t, "HOST_Ann_1", 2, KEY, true, "82.0.0.1");
    t.quick_match(&rex2);
    assert!(got_lock(&rex2.msgs()), "back into Ann's match");
    assert_eq!(seats(&ben.texts()), Some(vec!["Sue"]));
    assert_eq!(last_games(&sue.texts()).unwrap()["seat"], gid(2));
}

/// The room of a started match that its host left is still the host's for HOST_BACK_MS: another
/// host's game (another key, or none) cannot take it then.
#[test]
fn a_room_whose_host_is_coming_back_is_taken_for_others() {
    let mut t = T::new(Config::default());
    let (ann, pia, _rex) = keyed_match(&mut t);
    t.disconnect(&ann);
    for other in [Some("fedcba9876543210"), None] {
        let b = t.conn_from(Some("82.0.0.8"));
        t.send(&b, json!({"t": "hello", "v": 1, "name": "RGHost"}));
        b.take();
        let mut m = json!({"t": "serve", "room": "HOST_Ann_1", "players": 1, "name": "Ann", "approve": true});
        if let Some(k) = other {
            m["key"] = json!(k);
        }
        t.send(&b, m);
        assert_eq!(b.texts()[0]["code"], "taken", "key {other:?}");
    }
    assert_eq!(pia.closed(), None);
    assert_eq!(t.relay.status()["hosted"], 0);
}

/// An older host (no key) whose link drops mid-match cannot take its room back: without a key
/// anyone could serve that name. Its players stay until HOST_BACK, then they are sent away.
#[test]
fn an_older_host_whose_link_comes_back_mid_match_does_not_get_its_room() {
    let mut t = T::new(Config::default());
    let ann = t.host_approving("Ann", 1, "82.0.0.1");
    let pia = waiting_chooser(&mut t, "Pia", "82.0.0.3");
    pick(&mut t, &pia, gid(1));
    start(&mut t, &ann);
    t.lock_ok(&pia);
    assert_eq!(matched_room(&pia.msgs()).as_deref(), Some("HOST_Ann_1"));
    t.send(&pia, json!({"t": "move", "room": "HOST_Ann_1"}));
    t.disconnect(&ann);
    let ann2 = open_host(&mut t, "Ann", "HOST_Ann_1", 1, true, "82.0.0.1");
    assert_eq!(ann2.texts()[0]["code"], "taken");
    assert_eq!(pia.closed(), None);
    t.tick(16_000);
    assert_eq!(pia.closed(), Some(4003));
    let pia2 = t.chooser_at("Pia", "82.0.0.3");
    t.quick_match(&pia2);
    assert!(!got_lock(&pia2.msgs()), "not back into the old match");
}

/// A `serve` whose seat count is not a whole number 1..99 is served (a plain room) but lists no
/// game; its `start` and `kick` do nothing.
#[test]
fn a_serve_without_a_valid_seat_count_lists_no_game() {
    for players in [json!(0), json!(-1), json!(2.5), json!(100), json!("2"), Value::Null] {
        let mut t = T::new(Config::default());
        let h = t.conn_from(Some("82.0.0.1"));
        t.send(&h, json!({"t": "hello", "v": 1, "name": "RGHost"}));
        h.take();
        let what = players.to_string();
        t.send(&h, json!({"t": "serve", "room": "HOST_Ann_1", "players": players, "name": "Ann", "approve": true}));
        assert_eq!(h.texts(), vec![json!({"t": "serving", "room": "HOST_Ann_1"})], "players {what}");
        assert_eq!(t.relay.status()["hosted"], 0, "players {what}");
        let pia = t.chooser_at("Pia", "82.0.0.3");
        t.quick_match(&pia);
        assert!(offered(&last_offer(&pia).unwrap()).is_empty());
        start(&mut t, &h);
        kick(&mut t, &h, "Pia");
        assert!(h.texts().is_empty());
        assert!(!got_lock(&pia.msgs()));
    }
}

// ------------------------------------------------------------------ several hosts at one home

/// Several hosts at one home: a chooser from there sits in the newest of them by itself, an older
/// game's player in the next home game with a free seat (before a newer game elsewhere); a chooser
/// who picked stays put; one sent away sits in the other home game, not the one that sent them.
#[test]
fn several_hosts_at_one_home() {
    let mut t = T::new(Config::default());
    let ann = t.host_approving("Ann", 2, "82.0.0.1");
    t.tick(1_000);
    let ben = t.host_approving("Ben", 1, "82.0.0.1");
    t.tick(1_000);
    let cy = t.host_approving("Cy", 2, "82.0.0.2");
    let pia = t.chooser_at("Pia", "82.0.0.1");
    t.quick_match(&pia);
    let g = last_offer(&pia).unwrap();
    assert_eq!(g["seat"], gid(2), "the newest game at home");
    assert_eq!(offered(&g), ["Ben", "Ann", "Cy"], "home first, newest first within");
    let home: Vec<u64> = (0..3).map(|i| g["games"][i][5].as_u64().unwrap()).collect();
    assert_eq!(home, [1, 1, 0]);
    assert_eq!(seats(&ben.texts()), Some(vec!["Pia"]));
    let quinn = t.player_at("Quinn", "82.0.0.1");
    t.quick_match(&quinn);
    let mut list = Vec::new();
    waiting_list(&quinn.msgs(), &mut list);
    assert_eq!(names(&list), ["Ann"], "Ben's is full: Ann's, also at home, before Cy's newer one");
    assert_eq!(seats(&ann.texts()), Some(vec!["Quinn"]));
    // Pia picks Ann's game and stays there
    pick(&mut t, &pia, gid(1));
    assert_eq!(seats(&ben.texts()), Some(vec![]));
    assert_eq!(seats(&ann.texts()), Some(vec!["Quinn", "Pia"]));
    let rex = t.player_at("Rex", "82.0.0.9");
    t.quick_match(&rex);
    assert_eq!(seats(&cy.texts()), Some(vec!["Rex"]));
    assert_eq!(last_offer(&pia).unwrap()["seat"], gid(1), "not moved back to Ben's");
    assert!(ben.texts().is_empty());
    // Ann sends Pia away: the other game at home takes her
    kick(&mut t, &ann, "Pia");
    let g = last_offer(&pia).unwrap();
    assert_eq!(g["seat"], gid(2));
    assert_eq!(offered(&g), ["Ben", "Cy"]);
    assert_eq!(seats(&ben.texts()), Some(vec!["Pia"]));
    assert_eq!(seats(&ann.texts()), Some(vec!["Quinn"]));
}

/// A player sent away by the only host at their home is not seated there again by the home rule
/// (neither a chooser nor an older game's player), also after Back and Quick Match.
#[test]
fn a_kick_at_home_is_not_undone_by_the_home_rule() {
    let mut t = T::new(Config::default());
    let ann = t.host_approving("Ann", 2, "82.0.0.1");
    let pia = t.chooser_at("Pia", "82.0.0.1");
    t.quick_match(&pia);
    assert_eq!(last_offer(&pia).unwrap()["seat"], gid(1), "seated at home");
    let quinn = t.player_at("Quinn", "82.0.0.1");
    t.quick_match(&quinn);
    assert_eq!(seats(&ann.texts()), Some(vec!["Pia", "Quinn"]));
    kick(&mut t, &ann, "Pia");
    kick(&mut t, &ann, "Quinn");
    assert_eq!(seats(&ann.texts()), Some(vec![]));
    let g = last_offer(&pia).unwrap();
    assert_eq!(g["seat"], 0);
    assert!(offered(&g).is_empty());
    again(&mut t, &pia);
    again(&mut t, &quinn);
    assert!(!got_lock(&quinn.msgs()));
    assert_eq!(last_offer(&pia).unwrap()["seat"], 0);
    assert_eq!(seats(&ann.texts()), None, "still nobody seated");
}

/// On a LAN relay every game counts as the player's own network (the relay's own host on
/// loopback too): seated by itself, and after a kick in the other game.
#[test]
fn on_a_lan_relay_every_game_is_at_home() {
    let mut t = T::new(Config::default());
    let lan1 = t.host_approving("Lan1", 1, "127.0.0.1"); // the desktop game running the relay
    t.tick(1_000);
    let lan2 = t.host_approving("Lan2", 1, "192.168.1.10");
    let pia = t.chooser_at("Pia", "192.168.1.20");
    t.quick_match(&pia);
    let g = last_offer(&pia).unwrap();
    assert_eq!(g["seat"], gid(2));
    let home: Vec<u64> = (0..2).map(|i| g["games"][i][5].as_u64().unwrap()).collect();
    assert_eq!(home, [1, 1]);
    assert_eq!(seats(&lan2.texts()), Some(vec!["Pia"]));
    kick(&mut t, &lan2, "Pia");
    assert_eq!(last_offer(&pia).unwrap()["seat"], gid(1));
    assert_eq!(seats(&lan1.texts()), Some(vec!["Pia"]));
    kick(&mut t, &lan1, "Pia");
    let g = last_offer(&pia).unwrap();
    assert_eq!(g["seat"], 0);
    assert!(offered(&g).is_empty(), "sent away from both");
}

/// Two hosts who play under the same nickname: the engine names the room `HOST_<nick>_<n>` with a
/// random n, and a host whose `serve` is refused (`taken`: the same n by chance, or an older host
/// counting from 1) serves another n. Both games are offered, and each host hears only its own
/// seats.
#[test]
fn two_hosts_with_the_same_nickname_both_get_their_game_listed() {
    let mut t = T::new(Config::default());
    let a = keyed_host(&mut t, "HOST_Ann_1", 1, KEY, false, "82.0.0.1");
    assert_eq!(a.texts()[0]["t"], "serving");
    let b = keyed_host(&mut t, "HOST_Ann_1", 1, "00000000ffffffff", false, "82.0.0.2");
    assert_eq!(b.texts()[0]["code"], "taken");
    t.send(&b, json!({"t": "serve", "room": "HOST_Ann_482913", "players": 1, "name": "Ann", "hero": 7, "approve": true, "key": "00000000ffffffff"}));
    assert_eq!(b.texts()[0], json!({"t": "serving", "room": "HOST_Ann_482913"}));
    let pia = t.chooser_at("Pia", "82.0.0.3");
    t.quick_match(&pia);
    assert_eq!(offered(&last_offer(&pia).unwrap()), ["Ann", "Ann"]);
    pick(&mut t, &pia, gid(2));
    assert_eq!(seats(&b.texts()), Some(vec!["Pia"]));
    assert!(a.texts().is_empty());
}

/// A host that stops before its match ran on its engine (Back/Esc after START) says so
/// (`"abort":true`): the players sent to it are sent away at once, not after HOST_BACK_MS; one
/// still on the way in is turned away at the door. A normal `unserve` after the match leaves
/// whoever is still in the room for HOST_BACK_MS.
#[test]
fn a_host_that_aborts_its_started_match_sends_its_players_away_at_once() {
    for abort in [true, false] {
        let mut t = T::new(Config::default());
        let ann = t.host_approving("Ann", 2, "82.0.0.1");
        let pia = waiting_chooser(&mut t, "Pia", "82.0.0.3");
        pick(&mut t, &pia, gid(1));
        let rex = waiting_chooser(&mut t, "Rex", "82.0.0.4");
        pick(&mut t, &rex, gid(1));
        start(&mut t, &ann);
        t.lock_ok(&pia);
        t.lock_ok(&rex);
        assert_eq!(matched_room(&pia.msgs()).as_deref(), Some("HOST_Ann_1"));
        assert_eq!(matched_room(&rex.msgs()).as_deref(), Some("HOST_Ann_1"));
        t.send(&pia, json!({"t": "move", "room": "HOST_Ann_1"}));
        ann.take();
        t.send(&ann, if abort { json!({"t": "unserve", "abort": true}) } else { json!({"t": "unserve"}) });
        assert_eq!(ann.closed(), None, "the host stays");
        assert_eq!(pia.closed(), if abort { Some(4003) } else { None }, "abort {abort}");
        t.send(&rex, json!({"t": "move", "room": "HOST_Ann_1"}));
        assert_eq!(rex.closed(), if abort { Some(4003) } else { None }, "abort {abort}");
        t.tick(16_000);
        assert_eq!(pia.closed(), Some(4003));
        let pia2 = t.chooser_at("Pia", "82.0.0.3");
        t.quick_match(&pia2);
        assert!(!got_lock(&pia2.msgs()), "not back into that match");
    }
}

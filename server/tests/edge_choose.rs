//! Edge cases of choosing a hosted game (since 2026-11-25, docs/MULTIPLAYER.md "Choosing a game"):
//! the `games` lists, picks, moving between games, Back, leaving, kicks, choosers next to older games.
#![allow(dead_code)]

include!("harness/mod.rs");

/// The waiting list id of the n-th hosted game of a relay.
fn id(n: u64) -> u64 {
    900_000_000 + n
}

/// A host's game as the engine offers it since 2026-11-25: its nickname, seats (not counting the
/// host), hero (the engine's index), address, and whether its host starts it (`approve`). The
/// host's inbox keeps what it heard after `serving` (an approving host's first `seats`).
fn serve(t: &mut T, nick: &str, players: u32, hero: i32, addr: &str, approve: bool) -> C {
    let h = t.conn_from(Some(addr));
    t.send(&h, json!({"t": "hello", "v": 1, "name": "RGHost"}));
    h.take();
    let room = format!("HOST_{nick}_1");
    t.send(&h, json!({"t": "serve", "room": room, "players": players, "name": nick, "hero": hero, "approve": approve}));
    h.inbox.borrow_mut().retain(|o| !matches!(o, Out::Text(s) if s.starts_with(r#"{"t":"serving""#)));
    h
}

/// The `games` messages among these texts.
fn all_games(texts: &[Value]) -> Vec<&Value> {
    texts.iter().filter(|t| t["t"] == "games").collect()
}

/// The last `games` message this player got (drops what else came).
fn games(c: &C) -> Value {
    last_games(&c.texts()).cloned().expect("a games message")
}

/// The list ids of the games offered, in order.
fn ids(g: &Value) -> Vec<u64> {
    g["games"].as_array().unwrap().iter().map(|e| e[0].as_u64().unwrap()).collect()
}

fn pick(t: &mut T, c: &C, game: u64) {
    t.send(c, json!({"t": "pick", "id": game}));
}

fn start(t: &mut T, host: &C) {
    t.send(host, json!({"t": "start"}));
}

fn kick(t: &mut T, host: &C, name: &str) {
    t.send(host, json!({"t": "kick", "name": name}));
}

/// Did the casino answer CASINO_CANCEL_MATCHMAKING (Back)?
fn back_answered(msgs: &[Msg]) -> bool {
    msgs.iter().any(|m| m.1 == RESPONSE && m.2 == 10)
}

/// Lets `ms` pass in steps of 10 s, keeping these connections alive (pings).
fn wait(t: &mut T, ms: u64, alive: &[&C]) {
    let mut left = ms;
    while left > 0 {
        let step = left.min(10_000);
        t.tick(step);
        for c in alive {
            t.send(c, json!({"t": "ping"}));
        }
        left -= step;
    }
}

fn no_seats() -> Option<Vec<&'static str>> {
    Some(Vec::new())
}

// ------------------------------------------------------------------ list content and order

/// No game yet: an empty list; then the list follows games as they are hosted and go away.
#[test]
fn an_empty_list_first_then_games_as_they_come_and_go() {
    let mut t = T::new(Config::default());
    let p = t.chooser_at("Pia", "82.0.0.3");
    t.quick_match(&p);
    assert_eq!(games(&p), json!({"t": "games", "on": true, "seat": 0, "games": []}));
    let ann = serve(&mut t, "Ann", 2, 1, "82.0.0.1", true);
    assert_eq!(games(&p)["games"], json!([[id(1), "Ann", 1, 0, 2, 0, 1]]));
    t.tick(1_000);
    let ben = serve(&mut t, "Ben", 3, 2, "82.0.0.2", true);
    assert_eq!(ids(&games(&p)), [id(2), id(1)], "newest first");
    t.send(&ann, json!({"t": "unserve"}));
    assert_eq!(ids(&games(&p)), [id(2)]);
    t.disconnect(&ben); // the host quits
    assert_eq!(games(&p), json!({"t": "games", "on": true, "seat": 0, "games": []}));
}

/// Games from the player's own network first (newest first among them), then the others, newest
/// first; the player sits in the newest game at home.
#[test]
fn games_at_home_come_first_then_the_newest() {
    let mut t = T::new(Config::default());
    serve(&mut t, "Ann", 2, 1, "82.0.0.3", true); // Pia's home, the oldest
    t.tick(1_000);
    serve(&mut t, "Ben", 2, 1, "82.0.0.2", true);
    t.tick(1_000);
    serve(&mut t, "Cid", 2, 1, "82.0.0.3", true); // Pia's home too
    t.tick(1_000);
    serve(&mut t, "Dan", 2, 1, "82.0.0.4", true); // the newest
    let p = t.chooser_at("Pia", "82.0.0.3");
    t.quick_match(&p);
    let g = games(&p);
    assert_eq!(offered(&g), ["Cid", "Ann", "Dan", "Ben"]);
    let home: Vec<u64> = g["games"].as_array().unwrap().iter().map(|e| e[5].as_u64().unwrap()).collect();
    assert_eq!(home, [1, 1, 0, 0]);
    assert_eq!(g["seat"], id(3), "seated in the newest game at home");
}

/// Each entry: [id, host, hero index or -1, seated, seats, home, the host starts it].
#[test]
fn entries_name_the_host_hero_counts_home_and_who_starts_it() {
    let mut t = T::new(Config::default());
    serve(&mut t, "Ann", 3, 3, "82.0.0.1", true);
    serve(&mut t, "Old", 1, -1, "82.0.0.2", false); // random hero, starts when full
    serve(&mut t, "Max", 2, 12, "82.0.0.4", false); // no such hero
    let z = t.conn_from(Some("82.0.0.6"));
    t.send(&z, json!({"t": "hello", "v": 1, "name": "RGHost"}));
    let bad = json!({"t": "serve", "room": "HOST_Zed_1", "players": 1, "name": "bad name!", "hero": 0, "approve": true});
    t.send(&z, bad); // not a nickname: the name from the room
    t.host("RGHost", "HOST_Old_Timer_3", 2); // a host from before 2026-11-25, address unknown
    let p = t.chooser_at("Pia", "82.0.0.3");
    t.quick_match(&p);
    let g = games(&p);
    assert_eq!(g["seat"], 0);
    assert_eq!(
        g["games"],
        json!([
            [id(5), "Old_Timer", -1, 0, 2, 0, 0],
            [id(4), "Zed", 0, 0, 1, 0, 1],
            [id(3), "Max", -1, 0, 2, 0, 0],
            [id(2), "Old", -1, 0, 1, 0, 0],
            [id(1), "Ann", 3, 0, 3, 0, 1],
        ])
    );
}

/// At most 20 games: the newest.
#[test]
fn at_most_twenty_games_the_newest() {
    let mut t = T::new(Config::default());
    for i in 1..=25 {
        serve(&mut t, &format!("H{i}"), 2, 1, &format!("82.0.1.{i}"), true);
    }
    let p = t.chooser_at("Pia", "82.0.0.3");
    t.quick_match(&p);
    assert_eq!(ids(&games(&p)), (6..=25).rev().map(id).collect::<Vec<_>>());
}

/// The oldest of 25 games is from the player's home: it is listed (first) and they sit in it.
#[test]
fn a_game_at_home_is_listed_first_even_among_many() {
    let mut t = T::new(Config::default());
    serve(&mut t, "Sis", 2, 1, "82.0.0.3", true);
    for i in 2..=25 {
        serve(&mut t, &format!("H{i}"), 2, 1, &format!("82.0.1.{i}"), true);
    }
    let p = t.chooser_at("Pia", "82.0.0.3");
    t.quick_match(&p);
    let g = games(&p);
    assert_eq!(g["seat"], id(1));
    let want: Vec<u64> = std::iter::once(id(1)).chain((7..=25).rev().map(id)).collect();
    assert_eq!(ids(&g), want);
}

/// The player picked the oldest of 20 games; five more are hosted. Their own game must stay on
/// the list (lobby.c draws the "HOST'S GAME n/m" strip only for a game it finds in the list).
#[test]
fn the_own_game_stays_listed_when_more_than_twenty_wait() {
    let mut t = T::new(Config::default());
    for i in 1..=20 {
        serve(&mut t, &format!("H{i}"), 2, 1, &format!("82.0.1.{i}"), true);
    }
    let p = t.chooser_at("Pia", "82.0.0.3");
    t.quick_match(&p);
    assert_eq!(ids(&games(&p)).last(), Some(&id(1)));
    pick(&mut t, &p, id(1));
    assert_eq!(games(&p)["seat"], id(1));
    for i in 21..=25 {
        serve(&mut t, &format!("H{i}"), 2, 1, &format!("82.0.1.{i}"), true);
    }
    let g = games(&p);
    assert_eq!(g["seat"], id(1));
    assert!(ids(&g).contains(&id(1)), "Pia's game is not on her list: {:?}", ids(&g));
    assert!(ids(&g).len() <= 20);
}

/// Full games and games that sent the player away take no place among the 20.
#[test]
fn full_games_do_not_use_up_the_twenty() {
    let mut t = T::new(Config::default());
    for i in 1..=21 {
        serve(&mut t, &format!("H{i}"), 1, 1, &format!("82.0.1.{i}"), true);
    }
    let q = t.player_at("Quinn", "82.0.0.4"); // an older game: seated in the newest, now full
    t.quick_match(&q);
    let p = t.chooser_at("Pia", "82.0.0.3");
    t.quick_match(&p);
    assert_eq!(ids(&games(&p)), (1..=20).rev().map(id).collect::<Vec<_>>());
}

/// A full game is listed only for the players sitting in it.
#[test]
fn a_full_game_is_listed_only_for_its_own_players() {
    let mut t = T::new(Config::default());
    serve(&mut t, "Ann", 1, 7, "82.0.0.1", true);
    let p = t.chooser_at("Pia", "82.0.0.3");
    t.quick_match(&p);
    pick(&mut t, &p, id(1));
    let g = games(&p);
    assert_eq!(g["seat"], id(1));
    assert_eq!(g["games"], json!([[id(1), "Ann", 7, 1, 1, 0, 1]]));
    let r = t.chooser_at("Rex", "82.0.0.5");
    t.quick_match(&r);
    assert_eq!(games(&r)["games"], json!([]));
}

/// Older games never get a `games` message.
#[test]
fn older_games_get_no_lists() {
    let mut t = T::new(Config::default());
    serve(&mut t, "Ann", 2, 1, "82.0.0.1", true);
    let q = t.player_at("Quinn", "82.0.0.4");
    t.quick_match(&q);
    serve(&mut t, "Ben", 2, 1, "82.0.0.2", true);
    t.cancel(&q);
    t.quick_match(&q);
    assert!(all_games(&q.texts()).is_empty());
}

/// Only `"pick": true` makes a game a chooser.
#[test]
fn only_pick_true_makes_a_chooser() {
    let mut t = T::new(Config::default());
    serve(&mut t, "Ann", 9, 1, "82.0.0.1", true);
    for (i, flag) in [json!(false), json!("true"), json!(1), json!(null)].into_iter().enumerate() {
        let addr = format!("82.0.0.{}", 10 + i);
        let c = t.conn_from(Some(&addr));
        t.send(&c, json!({"t": "hello", "v": 1, "name": format!("P{i}"), "pick": flag}));
        t.send(&c, json!({"t": "join", "room": "CasinoSystem"}));
        t.quick_match(&c);
        let (texts, m) = c.split();
        assert!(all_games(&texts).is_empty(), "pick: {flag}");
        let mut list = Vec::new();
        waiting_list(&m, &mut list);
        assert_eq!(list[0].1, "Ann", "pick: {flag}: seated by itself like an older game");
    }
}

/// A chooser in the casino lobby who has not asked for Quick Match gets no list.
#[test]
fn no_list_before_quick_match() {
    let mut t = T::new(Config::default());
    let p = t.chooser_at("Pia", "82.0.0.3");
    serve(&mut t, "Ann", 2, 1, "82.0.0.1", true);
    serve(&mut t, "Sis", 2, 1, "82.0.0.3", true); // at home, too
    assert!(all_games(&p.texts()).is_empty());
    assert_eq!(t.relay.status()["queued"], 0, "nobody seated");
}

/// A game from elsewhere does not seat a chooser by itself (an older game it does).
#[test]
fn a_chooser_is_not_seated_by_itself_in_a_game_from_elsewhere() {
    let mut t = T::new(Config::default());
    let ann = serve(&mut t, "Ann", 2, 1, "82.0.0.1", true);
    ann.take();
    let p = t.chooser_at("Pia", "82.0.0.3");
    t.quick_match(&p);
    let (texts, m) = p.split();
    assert_eq!(last_games(&texts).unwrap()["seat"], 0);
    let mut list = Vec::new();
    waiting_list(&m, &mut list);
    assert!(list.is_empty(), "not in Ann's game");
    assert!(ann.texts().is_empty());
    let q = t.player_at("Quinn", "82.0.0.4");
    t.quick_match(&q);
    assert_eq!(seats(&ann.texts()), Some(vec!["Quinn"]));
}

/// On a LAN relay every game counts as the player's own network: seated in the newest with a
/// free seat.
#[test]
fn on_a_lan_relay_every_game_is_at_home() {
    let mut t = T::new(Config::default());
    serve(&mut t, "Ann", 2, 1, "127.0.0.1", true); // the relay runs in Ann's game
    t.tick(1_000);
    serve(&mut t, "Ben", 2, 1, "192.168.1.10", true);
    let p = t.chooser_at("Pia", "192.168.1.20");
    t.quick_match(&p);
    let g = games(&p);
    assert_eq!(g["seat"], id(2), "the newest");
    assert_eq!(g["games"], json!([[id(2), "Ben", 1, 1, 2, 1, 1], [id(1), "Ann", 1, 0, 2, 1, 1]]));
    let r = t.chooser_at("Rex", "10.0.0.7");
    t.quick_match(&r);
    assert_eq!(games(&r)["seat"], id(2));
    let s = t.chooser_at("Sue", "192.168.1.30");
    t.quick_match(&s);
    assert_eq!(games(&s)["seat"], id(1), "Ben's game is full: Ann's");
}

/// Without addresses (a relay that does not know them) no game is anyone's home.
#[test]
fn without_addresses_no_game_is_at_home() {
    let mut t = T::new(Config::default());
    t.host("RGHost", "HOST_Ann_1", 2);
    let p = t.conn();
    t.send(&p, json!({"t": "hello", "v": 1, "name": "Pia", "pick": true}));
    t.send(&p, json!({"t": "join", "room": "CasinoSystem"}));
    t.quick_match(&p);
    let g = games(&p);
    assert_eq!(g["seat"], 0);
    assert_eq!(g["games"], json!([[id(1), "Ann", -1, 0, 2, 0, 0]]));
}

// ------------------------------------------------------------------ picks

/// An id that is no waiting game: the same list again, nothing else changes. (Spaced out: picks
/// faster than one per 500 ms after a few at once are ignored.)
#[test]
fn picking_an_unknown_id_resends_the_same_list() {
    let mut t = T::new(Config::default());
    let ann = serve(&mut t, "Ann", 2, 1, "82.0.0.1", true);
    let p = t.chooser_at("Pia", "82.0.0.3");
    t.quick_match(&p);
    let before = games(&p);
    ann.take();
    for bad in [0, 1, 12_345, id(0), id(2), u64::from(u32::MAX)] {
        t.tick(500);
        pick(&mut t, &p, bad);
        let (texts, m) = p.split();
        assert_eq!(all_games(&texts), [&before], "id {bad}");
        assert!(m.is_empty(), "id {bad}: the waiting list stays");
    }
    assert!(ann.texts().is_empty(), "nobody sat down");
}

/// Picks without a usable id are ignored; before login they are refused.
#[test]
fn malformed_picks_are_ignored() {
    let mut t = T::new(Config::default());
    let ann = serve(&mut t, "Ann", 2, 1, "82.0.0.1", true);
    let p = t.chooser_at("Pia", "82.0.0.3");
    t.quick_match(&p);
    p.take();
    ann.take();
    for m in [
        json!({"t": "pick"}),
        json!({"t": "pick", "id": null}),
        json!({"t": "pick", "id": "900000001"}),
        json!({"t": "pick", "id": -1}),
        json!({"t": "pick", "id": 900000001.5}),
        json!({"t": "pick", "id": (1u64 << 32) + id(1)}),
        json!({"t": "pick", "id": [id(1)]}),
    ] {
        t.send(&p, m.clone());
        assert!(p.take().is_empty(), "{m}");
    }
    assert!(ann.texts().is_empty(), "nobody sat down");
    let c = t.conn();
    t.send(&c, json!({"t": "pick", "id": id(1)}));
    assert_eq!(c.texts(), [json!({"t": "error", "op": "pick", "code": "not_logged_in"})]);
    pick(&mut t, &p, id(1)); // a proper pick still works
    assert_eq!(seats(&ann.texts()), Some(vec!["Pia"]));
}

/// Picking the game one already sits in (also one chosen at home) sends nothing to anyone.
#[test]
fn picking_the_game_one_sits_in_changes_nothing() {
    let mut t = T::new(Config::default());
    let ann = serve(&mut t, "Ann", 2, 1, "82.0.0.1", true);
    let p = t.chooser_at("Pia", "82.0.0.3");
    t.quick_match(&p);
    pick(&mut t, &p, id(1));
    assert_eq!(seats(&ann.texts()), Some(vec!["Pia"]));
    p.take();
    pick(&mut t, &p, id(1));
    assert!(p.take().is_empty());
    assert!(ann.take().is_empty());
    let sis = serve(&mut t, "Sis", 2, 1, "82.0.0.9", true);
    let r = t.chooser_at("Rex", "82.0.0.9");
    t.quick_match(&r);
    assert_eq!(games(&r)["seat"], id(2), "seated at home");
    sis.take();
    pick(&mut t, &r, id(2));
    assert!(r.take().is_empty());
    assert!(sis.take().is_empty());
}

/// A full game: the pick is refused, the list comes again and the player keeps their seat.
#[test]
fn picking_a_full_game_is_refused_and_the_seat_is_kept() {
    let mut t = T::new(Config::default());
    let ben = serve(&mut t, "Ben", 2, 1, "82.0.0.2", true);
    t.tick(1_000);
    let ann = serve(&mut t, "Ann", 1, 1, "82.0.0.1", true);
    let q = t.player_at("Quinn", "82.0.0.4"); // an older game: seated in the newest
    t.quick_match(&q);
    assert_eq!(seats(&ann.texts()), Some(vec!["Quinn"]));
    let p = t.chooser_at("Pia", "82.0.0.3");
    t.quick_match(&p);
    assert_eq!(ids(&games(&p)), [id(1)], "Ann's game is full");
    pick(&mut t, &p, id(1));
    let before = games(&p);
    assert_eq!(before["seat"], id(1));
    ben.take();
    pick(&mut t, &p, id(2));
    let (texts, m) = p.split();
    assert_eq!(all_games(&texts), [&before], "the list again, still in Ben's game");
    assert!(m.is_empty());
    assert!(ann.take().is_empty() && ben.take().is_empty());
    assert_eq!(t.relay.status()["queued"], 2);
}

/// Two players pick the last seat: the first gets it, the second the list again.
#[test]
fn two_players_race_for_the_last_seat() {
    let mut t = T::new(Config::default());
    let ann = serve(&mut t, "Ann", 1, 1, "82.0.0.1", true);
    let p = t.chooser_at("Pia", "82.0.0.3");
    let r = t.chooser_at("Rex", "82.0.0.5");
    t.quick_match(&p);
    t.quick_match(&r);
    assert_eq!(ids(&games(&r)), [id(1)]);
    pick(&mut t, &p, id(1));
    assert_eq!(games(&r)["games"], json!([]), "the full game leaves Rex's list");
    pick(&mut t, &r, id(1)); // Rex's pick was on its way
    assert_eq!(games(&r), json!({"t": "games", "on": true, "seat": 0, "games": []}));
    assert_eq!(seats(&ann.texts()), Some(vec!["Pia"]));
    assert_eq!(games(&p)["seat"], id(1));
}

/// A game that went away: the pick is refused; the host's next game has a new id.
#[test]
fn picking_a_game_that_just_went_away() {
    let mut t = T::new(Config::default());
    let ann = serve(&mut t, "Ann", 2, 1, "82.0.0.1", true);
    let p = t.chooser_at("Pia", "82.0.0.3");
    t.quick_match(&p);
    p.take();
    t.send(&ann, json!({"t": "unserve"}));
    assert_eq!(games(&p)["games"], json!([]));
    pick(&mut t, &p, id(1));
    assert_eq!(games(&p), json!({"t": "games", "on": true, "seat": 0, "games": []}));
    let next = json!({"t": "serve", "room": "HOST_Ann_2", "players": 2, "name": "Ann", "hero": 1, "approve": true});
    t.send(&ann, next);
    assert_eq!(ids(&games(&p)), [id(2)]);
    ann.take();
    pick(&mut t, &p, id(1)); // the old id does not lead into the new game
    assert_eq!(games(&p)["seat"], 0);
    assert!(ann.take().is_empty());
    pick(&mut t, &p, id(2));
    assert_eq!(seats(&ann.texts()), Some(vec!["Pia"]));
}

/// The host sent the player away: picking that game is refused; others are still offered it.
#[test]
fn picking_a_game_whose_host_sent_one_away_is_refused() {
    let mut t = T::new(Config::default());
    let ann = serve(&mut t, "Ann", 2, 1, "82.0.0.1", true);
    t.tick(1_000);
    serve(&mut t, "Ben", 2, 1, "82.0.0.2", true);
    let p = t.chooser_at("Pia", "82.0.0.3");
    t.quick_match(&p);
    pick(&mut t, &p, id(1));
    kick(&mut t, &ann, "Pia");
    assert_eq!(seats(&ann.texts()), no_seats());
    let g = games(&p);
    assert_eq!(g["seat"], 0);
    assert_eq!(ids(&g), [id(2)], "Ann's game is not offered to Pia");
    pick(&mut t, &p, id(1));
    assert_eq!(games(&p), g, "refused: the list again");
    assert!(ann.take().is_empty());
    let r = t.chooser_at("Rex", "82.0.0.5");
    t.quick_match(&r);
    assert_eq!(ids(&games(&r)), [id(2), id(1)]);
}

/// A pick from a player not in Quick Match (lobby only, after Back, not even in the lobby)
/// seats nobody.
#[test]
fn a_pick_outside_quick_match_does_nothing() {
    let mut t = T::new(Config::default());
    let ann = serve(&mut t, "Ann", 2, 1, "82.0.0.1", true);
    ann.take();
    let p = t.chooser_at("Pia", "82.0.0.3");
    pick(&mut t, &p, id(1));
    assert!(p.take().is_empty());
    t.quick_match(&p);
    t.cancel(&p);
    p.take();
    pick(&mut t, &p, id(1));
    assert!(p.take().is_empty());
    let c = t.conn_from(Some("82.0.0.7"));
    t.send(&c, json!({"t": "hello", "v": 1, "name": "Cy", "pick": true}));
    c.take();
    pick(&mut t, &c, id(1));
    assert!(c.take().is_empty());
    assert!(ann.take().is_empty(), "nobody sat down");
    assert_eq!(t.relay.status()["queued"], 0);
}

// ------------------------------------------------------------------ moving between games

/// Moving: the old seat is freed and the new one taken; both hosts hear it, the lists of the
/// player and of those seated with them follow.
#[test]
fn moving_frees_the_old_seat_and_tells_both_hosts() {
    let mut t = T::new(Config::default());
    let ann = serve(&mut t, "Ann", 2, 1, "82.0.0.1", true);
    t.tick(1_000);
    let ben = serve(&mut t, "Ben", 2, 1, "82.0.0.2", true);
    let q = t.player_at("Quinn", "82.0.0.1"); // an older game at Ann's home: seated there
    t.quick_match(&q);
    let mut qlist = Vec::new();
    waiting_list(&q.msgs(), &mut qlist);
    assert_eq!(names(&qlist), ["Ann"]);
    let p = t.chooser_at("Pia", "82.0.0.3");
    t.quick_match(&p);
    let (texts, m) = p.split();
    assert_eq!(last_games(&texts).unwrap()["seat"], 0);
    let mut plist = Vec::new();
    waiting_list(&m, &mut plist);
    assert!(plist.is_empty());
    ann.take();
    ben.take();
    pick(&mut t, &p, id(1));
    assert_eq!(seats(&ann.texts()), Some(vec!["Quinn", "Pia"]));
    assert!(ben.texts().is_empty());
    waiting_list(&p.msgs(), &mut plist);
    assert_eq!(names(&plist), ["Ann", "Quinn"]);
    waiting_list(&q.msgs(), &mut qlist);
    assert_eq!(names(&qlist), ["Ann", "Pia"]);
    // over to Ben's game
    pick(&mut t, &p, id(2));
    assert_eq!(seats(&ann.texts()), Some(vec!["Quinn"]));
    assert_eq!(seats(&ben.texts()), Some(vec!["Pia"]));
    let (texts, m) = p.split();
    let g = last_games(&texts).unwrap();
    assert_eq!(g["seat"], id(2));
    assert_eq!(g["games"], json!([[id(2), "Ben", 1, 1, 2, 0, 1], [id(1), "Ann", 1, 1, 2, 0, 1]]));
    waiting_list(&m, &mut plist);
    assert_eq!(names(&plist), ["Ben"]);
    waiting_list(&q.msgs(), &mut qlist);
    assert_eq!(names(&qlist), ["Ann"]);
    // and back again
    pick(&mut t, &p, id(1));
    assert_eq!(seats(&ann.texts()), Some(vec!["Quinn", "Pia"]));
    assert_eq!(seats(&ben.texts()), no_seats());
    waiting_list(&p.msgs(), &mut plist);
    assert_eq!(names(&plist), ["Ann", "Quinn"]);
    assert_eq!(t.relay.status()["queued"], 2);
    assert_eq!(t.relay.status()["hosted"], 2);
}

/// A full game the player moves out of is offered to the others again.
#[test]
fn moving_out_of_a_full_game_offers_it_again() {
    let mut t = T::new(Config::default());
    let ann = serve(&mut t, "Ann", 1, 1, "82.0.0.1", true);
    t.tick(1_000);
    serve(&mut t, "Ben", 2, 1, "82.0.0.2", true);
    let p = t.chooser_at("Pia", "82.0.0.3");
    t.quick_match(&p);
    pick(&mut t, &p, id(1));
    let r = t.chooser_at("Rex", "82.0.0.5");
    t.quick_match(&r);
    assert_eq!(ids(&games(&r)), [id(2)]);
    pick(&mut t, &p, id(2));
    assert_eq!(games(&r)["games"], json!([[id(2), "Ben", 1, 1, 2, 0, 1], [id(1), "Ann", 1, 0, 1, 0, 1]]));
    assert_eq!(seats(&ann.texts()), no_seats());
}

/// Moving into the last seat of an older host's game starts it; the old host hears the seat free.
#[test]
fn moving_into_the_last_seat_of_an_older_hosts_game_starts_it() {
    let mut t = T::new(Config::default());
    let ann = serve(&mut t, "Ann", 2, 1, "82.0.0.1", true);
    serve(&mut t, "Old", 1, 1, "82.0.0.2", false);
    let p = t.chooser_at("Pia", "82.0.0.3");
    t.quick_match(&p);
    pick(&mut t, &p, id(1));
    ann.take();
    pick(&mut t, &p, id(2));
    let (texts, m) = p.split();
    assert!(got_lock(&m));
    assert_eq!(last_games(&texts).unwrap(), &json!({"t": "games", "on": false}));
    assert_eq!(seats(&ann.texts()), no_seats());
    t.lock_ok(&p);
    assert_eq!(matched_room(&p.msgs()).as_deref(), Some("HOST_Old_1"));
    assert_eq!(t.relay.status()["hosted"], 1, "Ann's game still waits");
}

/// A chooser seated at home who moves to a game elsewhere stays there, also when more games
/// (at home too) appear.
#[test]
fn a_chooser_who_moves_away_from_home_stays_where_they_went() {
    let mut t = T::new(Config::default());
    let sis = serve(&mut t, "Sis", 2, 1, "82.0.0.3", true);
    t.tick(1_000);
    let ben = serve(&mut t, "Ben", 2, 1, "82.0.0.2", true);
    let p = t.chooser_at("Pia", "82.0.0.3");
    t.quick_match(&p);
    assert_eq!(games(&p)["seat"], id(1), "seated at home");
    pick(&mut t, &p, id(2));
    assert_eq!(seats(&sis.texts()), no_seats());
    t.tick(1_000);
    serve(&mut t, "Cid", 2, 1, "82.0.0.5", true);
    t.tick(1_000);
    serve(&mut t, "Bro", 2, 1, "82.0.0.3", true); // another game at home
    let g = games(&p);
    assert_eq!(g["seat"], id(2), "still in Ben's game");
    assert_eq!(ids(&g), [id(4), id(1), id(3), id(2)]);
    assert_eq!(seats(&ben.texts()), Some(vec!["Pia"]));
}

/// A chooser without a seat is seated in a game hosted at home as soon as it appears.
#[test]
fn a_chooser_without_a_seat_is_seated_when_a_game_at_home_appears() {
    let mut t = T::new(Config::default());
    serve(&mut t, "Ben", 2, 1, "82.0.0.2", true);
    let p = t.chooser_at("Pia", "82.0.0.3");
    t.quick_match(&p);
    let (texts, m) = p.split();
    assert_eq!(last_games(&texts).unwrap()["seat"], 0);
    let mut list = Vec::new();
    waiting_list(&m, &mut list);
    let sis = serve(&mut t, "Sis", 2, 1, "82.0.0.3", true);
    assert_eq!(seats(&sis.texts()), Some(vec!["Pia"]));
    let (texts, m) = p.split();
    assert_eq!(last_games(&texts).unwrap()["seat"], id(2));
    waiting_list(&m, &mut list);
    assert_eq!(names(&list), ["Sis"]);
}

// ------------------------------------------------------------------ Back

/// Back while seated: the seat is freed (the host and the others see it), the list ends.
#[test]
fn back_while_seated_frees_the_seat_and_ends_the_list() {
    let mut t = T::new(Config::default());
    let ann = serve(&mut t, "Ann", 1, 1, "82.0.0.1", true);
    let p = t.chooser_at("Pia", "82.0.0.3");
    t.quick_match(&p);
    pick(&mut t, &p, id(1));
    let r = t.chooser_at("Rex", "82.0.0.5");
    t.quick_match(&r);
    assert_eq!(games(&r)["games"], json!([]));
    ann.take();
    p.take();
    t.cancel(&p);
    let (texts, m) = p.split();
    assert_eq!(all_games(&texts), [&json!({"t": "games", "on": false})]);
    assert!(back_answered(&m));
    assert_eq!(seats(&ann.texts()), no_seats());
    assert_eq!(ids(&games(&r)), [id(1)]);
    start(&mut t, &ann);
    assert!(!got_lock(&p.msgs()) && !got_lock(&r.msgs()), "nobody seated: no start");
}

/// Back while choosing ends the list; Quick Match again brings it back.
#[test]
fn back_while_choosing_ends_the_list_and_quick_match_brings_it_back() {
    let mut t = T::new(Config::default());
    serve(&mut t, "Ann", 2, 1, "82.0.0.1", true);
    let p = t.chooser_at("Pia", "82.0.0.3");
    t.quick_match(&p);
    let first = games(&p);
    t.cancel(&p);
    let (texts, m) = p.split();
    assert_eq!(all_games(&texts), [&json!({"t": "games", "on": false})]);
    assert!(back_answered(&m));
    t.quick_match(&p);
    assert_eq!(games(&p), first);
}

/// Back outside Quick Match (and Back twice) is answered but sends no list.
#[test]
fn back_outside_quick_match_sends_no_list() {
    let mut t = T::new(Config::default());
    serve(&mut t, "Ann", 2, 1, "82.0.0.1", true);
    let p = t.chooser_at("Pia", "82.0.0.3");
    t.cancel(&p);
    let (texts, m) = p.split();
    assert!(all_games(&texts).is_empty());
    assert!(back_answered(&m));
    t.quick_match(&p);
    t.cancel(&p);
    p.take();
    t.cancel(&p);
    let (texts, m) = p.split();
    assert!(all_games(&texts).is_empty(), "the list ended once");
    assert!(back_answered(&m));
}

/// After Back a chooser is seated at home again (they choose themselves); an older game goes on
/// to the next game instead.
#[test]
fn a_choosers_back_does_not_skip_the_game_at_home_as_an_older_games_does() {
    let mut t = T::new(Config::default());
    serve(&mut t, "Sis", 2, 1, "82.0.0.3", true);
    t.tick(1_000);
    serve(&mut t, "Ben", 2, 1, "82.0.0.2", true);
    let p = t.chooser_at("Pia", "82.0.0.3");
    t.quick_match(&p);
    assert_eq!(games(&p)["seat"], id(1));
    t.cancel(&p);
    t.quick_match(&p);
    assert_eq!(games(&p)["seat"], id(1), "at home again");
    let o = t.player_at("Ola", "82.0.0.3");
    t.quick_match(&o);
    let mut olist = Vec::new();
    waiting_list(&o.msgs(), &mut olist);
    assert_eq!(names(&olist), ["Sis", "Pia"]);
    t.cancel(&o);
    t.quick_match(&o);
    waiting_list(&o.msgs(), &mut olist);
    assert_eq!(names(&olist), ["Ben"], "an older game: the next game");
}

// ------------------------------------------------------------------ disconnecting, hello again

/// A seated chooser's connection drops: the seat is freed for everyone to see.
#[test]
fn a_seated_chooser_who_disconnects_frees_the_seat() {
    let mut t = T::new(Config::default());
    let ann = serve(&mut t, "Ann", 2, 1, "82.0.0.1", true);
    let q = t.player_at("Quinn", "82.0.0.1");
    t.quick_match(&q);
    let mut qlist = Vec::new();
    waiting_list(&q.msgs(), &mut qlist);
    let p = t.chooser_at("Pia", "82.0.0.3");
    t.quick_match(&p);
    pick(&mut t, &p, id(1));
    assert_eq!(seats(&ann.texts()), Some(vec!["Quinn", "Pia"]));
    waiting_list(&q.msgs(), &mut qlist);
    assert_eq!(names(&qlist), ["Ann", "Pia"]);
    let r = t.chooser_at("Rex", "82.0.0.5");
    t.quick_match(&r);
    assert_eq!(games(&r)["games"], json!([]));
    t.disconnect(&p);
    assert_eq!(seats(&ann.texts()), Some(vec!["Quinn"]));
    waiting_list(&q.msgs(), &mut qlist);
    assert_eq!(names(&qlist), ["Ann"]);
    assert_eq!(games(&r)["games"], json!([[id(1), "Ann", 1, 1, 2, 0, 1]]));
    start(&mut t, &ann);
    assert!(!got_lock(&q.msgs()), "a seat is free: no start");
    assert_eq!(t.relay.status()["queued"], 2);
}

/// A chooser without a seat disconnects: gone from the others' lists, the name is free again and
/// a new connection starts from scratch.
#[test]
fn a_chooser_who_disconnects_while_choosing_leaves_no_trace() {
    let mut t = T::new(Config::default());
    let ann = serve(&mut t, "Ann", 2, 1, "82.0.0.1", true);
    ann.take();
    let p = t.chooser_at("Pia", "82.0.0.3");
    t.quick_match(&p);
    let r = t.chooser_at("Rex", "82.0.0.5");
    t.quick_match(&r);
    let mut rlist = Vec::new();
    waiting_list(&r.msgs(), &mut rlist);
    assert_eq!(names(&rlist), ["Pia"], "waiting together");
    t.disconnect(&p);
    waiting_list(&r.msgs(), &mut rlist);
    assert!(rlist.is_empty());
    assert!(ann.take().is_empty());
    assert_eq!(t.relay.status()["queued"], 1);
    let p2 = t.conn_from(Some("82.0.0.3"));
    t.send(&p2, json!({"t": "hello", "v": 1, "name": "Pia", "pick": true}));
    assert_eq!(p2.texts()[0]["name"], "Pia");
    t.send(&p2, json!({"t": "join", "room": "CasinoSystem"}));
    t.quick_match(&p2);
    assert_eq!(games(&p2), json!({"t": "games", "on": true, "seat": 0, "games": [[id(1), "Ann", 1, 0, 2, 0, 1]]}));
}

/// A second hello on the same connection keeps the name and the seat.
#[test]
fn hello_again_on_the_same_connection_keeps_the_name_and_the_seat() {
    let mut t = T::new(Config::default());
    let ann = serve(&mut t, "Ann", 1, 1, "82.0.0.1", true);
    let p = t.chooser_at("Pia", "82.0.0.3");
    t.quick_match(&p);
    pick(&mut t, &p, id(1));
    assert_eq!(seats(&ann.texts()), Some(vec!["Pia"]));
    p.take();
    t.send(&p, json!({"t": "hello", "v": 1, "name": "Pia", "pick": true}));
    let texts = p.texts();
    assert_eq!(texts.len(), 1);
    assert_eq!(texts[0]["name"], "Pia");
    assert!(ann.take().is_empty(), "still seated");
    start(&mut t, &ann);
    assert!(got_lock(&p.msgs()));
}

/// A seated chooser's game froze; the player starts it again under the same name: the silent
/// connection is replaced, its seat freed, and the new one chooses again.
#[test]
fn a_silent_seated_chooser_is_replaced_and_chooses_again() {
    let mut t = T::new(Config::default());
    let ann = serve(&mut t, "Ann", 2, 1, "82.0.0.1", true);
    let p = t.chooser_at("Pia", "82.0.0.3");
    t.quick_match(&p);
    pick(&mut t, &p, id(1));
    assert_eq!(seats(&ann.texts()), Some(vec!["Pia"]));
    t.now += 25_000;
    let p2 = t.conn_from(Some("82.0.0.3"));
    t.send(&p2, json!({"t": "hello", "v": 1, "name": "Pia", "pick": true}));
    assert_eq!(p2.texts()[0]["name"], "Pia");
    assert_eq!(p.closed(), Some(4002));
    assert_eq!(seats(&ann.texts()), no_seats());
    t.send(&p2, json!({"t": "join", "room": "CasinoSystem"}));
    t.quick_match(&p2);
    let g = games(&p2);
    assert_eq!(g["seat"], 0);
    assert_eq!(g["games"], json!([[id(1), "Ann", 1, 0, 2, 0, 1]]));
    pick(&mut t, &p2, id(1));
    assert_eq!(seats(&ann.texts()), Some(vec!["Pia"]));
}

/// Sent away, then back under the same name (in any case): still kept out of that game.
#[test]
fn a_player_sent_away_stays_away_after_reconnecting() {
    let mut t = T::new(Config::default());
    let ann = serve(&mut t, "Ann", 2, 1, "82.0.0.1", true);
    t.tick(1_000);
    serve(&mut t, "Ben", 2, 1, "82.0.0.2", true);
    let p = t.chooser_at("Pia", "82.0.0.3");
    t.quick_match(&p);
    pick(&mut t, &p, id(1));
    kick(&mut t, &ann, "Pia");
    t.disconnect(&p);
    for name in ["Pia", "PIA"] {
        let c = t.chooser_at(name, "82.0.0.3");
        t.quick_match(&c);
        assert_eq!(ids(&games(&c)), [id(2)], "{name}");
        pick(&mut t, &c, id(1));
        assert_eq!(games(&c)["seat"], 0, "{name}");
        t.disconnect(&c);
    }
    assert_eq!(seats(&ann.texts()), no_seats());
}

// ------------------------------------------------------------------ leaving: on false

/// Leaving the casino lobby while waiting (seated or not) ends the list and frees the seat.
#[test]
fn leaving_the_lobby_while_waiting_ends_the_list() {
    let mut t = T::new(Config::default());
    let ann = serve(&mut t, "Ann", 1, 1, "82.0.0.1", true);
    let p = t.chooser_at("Pia", "82.0.0.3");
    t.quick_match(&p);
    pick(&mut t, &p, id(1));
    p.take();
    t.send(&p, json!({"t": "move", "room": "Elsewhere"}));
    assert_eq!(all_games(&p.texts()), [&json!({"t": "games", "on": false})]);
    assert_eq!(seats(&ann.texts()), no_seats());
    let r = t.chooser_at("Rex", "82.0.0.5");
    t.quick_match(&r);
    assert_eq!(ids(&games(&r)), [id(1)]);
    t.send(&r, json!({"t": "leave"}));
    assert_eq!(all_games(&r.texts()), [&json!({"t": "games", "on": false})]);
    assert_eq!(t.relay.status()["queued"], 0);
}

/// Once the list ended, no more lists come, whatever happens.
#[test]
fn after_the_list_ended_no_more_lists_come() {
    let mut t = T::new(Config::default());
    let p = t.chooser_at("Pia", "82.0.0.3");
    t.quick_match(&p);
    t.cancel(&p);
    p.take();
    let ann = serve(&mut t, "Ann", 2, 1, "82.0.0.1", true);
    serve(&mut t, "Sis", 2, 1, "82.0.0.3", true); // even at home
    let q = t.player_at("Quinn", "82.0.0.4");
    t.quick_match(&q);
    kick(&mut t, &ann, "Quinn");
    t.disconnect(&ann);
    assert!(all_games(&p.texts()).is_empty());
}

// ------------------------------------------------------------------ list updates

/// The list follows seats taken and freed in a game the player looks at; other players waiting
/// do not resend it.
#[test]
fn the_list_follows_seats_taken_and_freed_in_other_games() {
    let mut t = T::new(Config::default());
    serve(&mut t, "Ann", 2, 1, "82.0.0.1", true);
    let p = t.chooser_at("Pia", "82.0.0.3");
    t.quick_match(&p);
    assert_eq!(games(&p)["games"], json!([[id(1), "Ann", 1, 0, 2, 0, 1]]));
    let q = t.player_at("Quinn", "82.0.0.4"); // an older game: seated in Ann's
    t.quick_match(&q);
    assert_eq!(games(&p)["games"], json!([[id(1), "Ann", 1, 1, 2, 0, 1]]));
    let r = t.chooser_at("Rex", "82.0.0.5");
    t.quick_match(&r);
    assert!(all_games(&p.texts()).is_empty(), "nothing changed for Pia");
    pick(&mut t, &r, id(1));
    assert_eq!(games(&p)["games"], json!([]), "full");
    t.cancel(&r);
    assert_eq!(games(&p)["games"], json!([[id(1), "Ann", 1, 1, 2, 0, 1]]));
    t.disconnect(&q);
    assert_eq!(games(&p)["games"], json!([[id(1), "Ann", 1, 0, 2, 0, 1]]));
}

/// A chooser and an older game fill an older host's game: it starts; a third player's list
/// loses it once; the older host hears no `seats`.
#[test]
fn a_game_that_fills_and_starts_leaves_the_list_once() {
    let mut t = T::new(Config::default());
    let old = serve(&mut t, "Old", 2, 1, "82.0.0.1", false);
    let q = t.player_at("Quinn", "82.0.0.4");
    t.quick_match(&q);
    let r = t.chooser_at("Rex", "82.0.0.5");
    t.quick_match(&r);
    assert_eq!(games(&r)["games"], json!([[id(1), "Old", 1, 1, 2, 0, 0]]));
    let p = t.chooser_at("Pia", "82.0.0.3");
    t.quick_match(&p);
    q.take();
    pick(&mut t, &p, id(1));
    let texts = r.texts();
    assert_eq!(all_games(&texts), [&json!({"t": "games", "on": true, "seat": 0, "games": []})]);
    let (texts, m) = p.split();
    assert!(got_lock(&m));
    assert_eq!(last_games(&texts).unwrap(), &json!({"t": "games", "on": false}));
    assert!(got_lock(&q.msgs()));
    t.lock_ok(&p);
    t.lock_ok(&q);
    assert_eq!(matched_room(&p.msgs()).as_deref(), Some("HOST_Old_1"));
    assert_eq!(matched_room(&q.msgs()).as_deref(), Some("HOST_Old_1"));
    assert!(seats(&old.texts()).is_none());
    assert_eq!(t.relay.status()["hosted"], 0);
}

/// The host quits: its choosers choose again (not seated elsewhere by themselves), its older
/// games' players go on to the next game.
#[test]
fn when_a_host_quits_its_choosers_choose_again_and_older_games_move_on() {
    let mut t = T::new(Config::default());
    let ann = serve(&mut t, "Ann", 2, 1, "82.0.0.1", true);
    t.tick(1_000);
    let ben = serve(&mut t, "Ben", 2, 1, "82.0.0.2", true);
    let q = t.player_at("Quinn", "82.0.0.1"); // at Ann's home: seated there
    t.quick_match(&q);
    let p = t.chooser_at("Pia", "82.0.0.3");
    t.quick_match(&p);
    pick(&mut t, &p, id(1));
    assert_eq!(seats(&ann.texts()), Some(vec!["Quinn", "Pia"]));
    let mut qlist = Vec::new();
    waiting_list(&q.msgs(), &mut qlist);
    assert_eq!(names(&qlist), ["Ann", "Pia"]);
    ben.take();
    t.disconnect(&ann);
    let g = games(&p);
    assert_eq!(g["seat"], 0, "Pia chooses again");
    assert_eq!(g["games"], json!([[id(2), "Ben", 1, 1, 2, 0, 1]]));
    assert_eq!(seats(&ben.texts()), Some(vec!["Quinn"]));
    waiting_list(&q.msgs(), &mut qlist);
    assert_eq!(names(&qlist), ["Ben"]);
    pick(&mut t, &p, id(1)); // Ann's game is gone
    assert_eq!(games(&p)["seat"], 0);
    pick(&mut t, &p, id(2));
    assert_eq!(seats(&ben.texts()), Some(vec!["Quinn", "Pia"]));
}

/// An approving host hears its seats once per change, with the room and the number of seats.
#[test]
fn the_host_hears_each_change_of_its_seats_once() {
    let mut t = T::new(Config::default());
    let ann = serve(&mut t, "Ann", 2, 1, "82.0.0.1", true);
    assert_eq!(ann.texts(), [json!({"t": "seats", "room": "HOST_Ann_1", "names": [], "of": 2})]);
    let p = t.chooser_at("Pia", "82.0.0.3");
    t.quick_match(&p);
    assert!(ann.texts().is_empty(), "Pia only looks");
    pick(&mut t, &p, id(1));
    assert_eq!(ann.texts(), [json!({"t": "seats", "room": "HOST_Ann_1", "names": ["Pia"], "of": 2})]);
    let r = t.chooser_at("Rex", "82.0.0.5");
    t.quick_match(&r);
    serve(&mut t, "Ben", 2, 1, "82.0.0.2", true);
    assert!(ann.texts().is_empty());
    pick(&mut t, &r, id(1));
    assert_eq!(ann.texts(), [json!({"t": "seats", "room": "HOST_Ann_1", "names": ["Pia", "Rex"], "of": 2})]);
}

// ------------------------------------------------------------------ the host sends a player away

/// The player sent away goes to the front of the queue: the next game is theirs.
#[test]
fn a_removed_player_is_first_in_line_for_the_next_game() {
    let mut t = T::new(Config::default());
    let ann = serve(&mut t, "Ann", 1, 1, "82.0.0.1", true);
    let q = t.player_at("Quinn", "82.0.0.4");
    t.quick_match(&q); // seated in Ann's game
    let r = t.player_at("Rex", "82.0.0.5");
    t.quick_match(&r); // no free seat: waits
    let s = t.player_at("Sam", "82.0.0.6");
    t.quick_match(&s); // waits after Rex
    kick(&mut t, &ann, "Quinn");
    assert_eq!(seats(&ann.texts()), Some(vec!["Rex"]), "the free seat goes to the next in line");
    q.take();
    s.take();
    serve(&mut t, "Ben", 1, 1, "82.0.0.2", false); // an older host: starts when full
    assert!(got_lock(&q.msgs()), "Quinn, first in line, gets Ben's game");
    assert!(!got_lock(&s.msgs()));
}

/// A kick naming nobody seated: the host hears who is.
#[test]
fn removing_someone_not_seated_tells_the_host_who_is() {
    let mut t = T::new(Config::default());
    let ann = serve(&mut t, "Ann", 2, 1, "82.0.0.1", true);
    let p = t.chooser_at("Pia", "82.0.0.3");
    t.quick_match(&p);
    pick(&mut t, &p, id(1));
    ann.take();
    kick(&mut t, &ann, "Nobody");
    assert_eq!(seats(&ann.texts()), Some(vec!["Pia"]));
    kick(&mut t, &ann, "pia");
    assert_eq!(seats(&ann.texts()), no_seats());
    kick(&mut t, &ann, "Pia"); // pressed twice
    assert_eq!(seats(&ann.texts()), no_seats());
    assert_eq!(games(&p)["seat"], 0);
}

/// Only a game's own host starts it or sends its players away.
#[test]
fn only_a_games_own_host_starts_it_or_sends_players_away() {
    let mut t = T::new(Config::default());
    let ann = serve(&mut t, "Ann", 1, 1, "82.0.0.1", true);
    let ben = serve(&mut t, "Ben", 1, 1, "82.0.0.2", true);
    let p = t.chooser_at("Pia", "82.0.0.3");
    t.quick_match(&p);
    pick(&mut t, &p, id(1));
    let r = t.chooser_at("Rex", "82.0.0.5");
    t.quick_match(&r);
    pick(&mut t, &r, id(2));
    for c in [&ann, &ben, &p, &r] {
        c.take();
    }
    t.send(&p, json!({"t": "start"}));
    t.send(&p, json!({"t": "kick", "name": "Rex"}));
    for c in [&ann, &ben, &p, &r] {
        assert!(c.take().is_empty(), "a player is no host");
    }
    kick(&mut t, &ben, "Pia"); // not in Ben's game
    assert_eq!(seats(&ben.texts()), Some(vec!["Rex"]));
    assert!(ann.take().is_empty() && p.take().is_empty());
    start(&mut t, &ben);
    assert!(got_lock(&r.msgs()));
    assert!(!got_lock(&p.msgs()));
    assert_eq!(t.relay.status()["hosted"], 1);
}

/// A game at home that sent the player away does not seat them by itself again (a chooser or an
/// older game).
#[test]
fn a_player_sent_away_from_a_game_at_home_is_not_seated_there_again() {
    let mut t = T::new(Config::default());
    let sis = serve(&mut t, "Sis", 2, 1, "82.0.0.3", true);
    let p = t.chooser_at("Pia", "82.0.0.3");
    t.quick_match(&p);
    assert_eq!(games(&p)["seat"], id(1));
    kick(&mut t, &sis, "Pia");
    assert_eq!(games(&p), json!({"t": "games", "on": true, "seat": 0, "games": []}));
    t.cancel(&p);
    t.quick_match(&p);
    assert_eq!(games(&p), json!({"t": "games", "on": true, "seat": 0, "games": []}));
    let o = t.player_at("Ola", "82.0.0.3");
    t.quick_match(&o);
    assert_eq!(seats(&sis.texts()), Some(vec!["Ola"]));
    kick(&mut t, &sis, "Ola");
    t.quick_match(&o);
    let mut olist = Vec::new();
    waiting_list(&o.msgs(), &mut olist);
    assert_eq!(names(&olist), ["Pia"], "waiting, not in Sis's game");
    assert_eq!(seats(&sis.texts()), no_seats());
}

/// Ten minutes after being sent away the game is offered (and can be picked) again.
#[test]
fn ten_minutes_after_being_sent_away_the_game_is_offered_again() {
    let mut t = T::new(Config::default());
    let ann = serve(&mut t, "Ann", 2, 1, "82.0.0.1", true);
    let p = t.chooser_at("Pia", "82.0.0.3");
    t.quick_match(&p);
    pick(&mut t, &p, id(1));
    kick(&mut t, &ann, "Pia");
    assert_eq!(games(&p)["games"], json!([]));
    wait(&mut t, 9 * 60_000, &[&ann, &p]);
    let ben = serve(&mut t, "Ben", 2, 1, "82.0.0.2", true);
    assert_eq!(ids(&games(&p)), [id(2)], "not yet");
    wait(&mut t, 61_000, &[&ann, &p, &ben]);
    serve(&mut t, "Cid", 2, 1, "82.0.0.5", true); // something happens
    assert_eq!(ids(&games(&p)), [id(3), id(2), id(1)]);
    pick(&mut t, &p, id(1));
    assert_eq!(seats(&ann.texts()), Some(vec!["Pia"]));
}

/// The same when nothing else happens meanwhile: the waiting player's list should offer the game
/// again once the ten minutes are over.
#[test]
fn ten_minutes_after_being_sent_away_the_list_offers_the_game_by_itself() {
    let mut t = T::new(Config::default());
    let ann = serve(&mut t, "Ann", 2, 1, "82.0.0.1", true);
    let p = t.chooser_at("Pia", "82.0.0.3");
    t.quick_match(&p);
    pick(&mut t, &p, id(1));
    kick(&mut t, &ann, "Pia");
    assert_eq!(games(&p)["games"], json!([]));
    wait(&mut t, 10 * 60_000 + 10_000, &[&ann, &p]);
    let texts = p.texts();
    let g = last_games(&texts).expect("a new list once the ten minutes are over");
    assert_eq!(ids(g), [id(1)]);
}

// ------------------------------------------------------------------ choosers and older games

/// A chooser and an older game in one game its host starts: both are listed for each other,
/// the host hears both, nothing starts until the host says so.
#[test]
fn a_chooser_and_an_older_game_share_a_game_its_host_starts() {
    let mut t = T::new(Config::default());
    let ann = serve(&mut t, "Ann", 2, 1, "82.0.0.1", true);
    let q = t.player_at("Quinn", "82.0.0.4"); // seated by itself
    t.quick_match(&q);
    let mut qlist = Vec::new();
    waiting_list(&q.msgs(), &mut qlist);
    assert_eq!(names(&qlist), ["Ann"]);
    assert_eq!(seats(&ann.texts()), Some(vec!["Quinn"]));
    let p = t.chooser_at("Pia", "82.0.0.3");
    t.quick_match(&p);
    let (texts, m) = p.split();
    assert_eq!(last_games(&texts).unwrap()["games"], json!([[id(1), "Ann", 1, 1, 2, 0, 1]]));
    let mut plist = Vec::new();
    waiting_list(&m, &mut plist);
    pick(&mut t, &p, id(1));
    assert_eq!(seats(&ann.texts()), Some(vec!["Quinn", "Pia"]));
    let (texts, m) = p.split();
    assert_eq!(last_games(&texts).unwrap()["games"], json!([[id(1), "Ann", 1, 2, 2, 0, 1]]));
    assert!(!got_lock(&m), "the host starts it");
    waiting_list(&m, &mut plist);
    assert_eq!(names(&plist), ["Ann", "Quinn"]);
    let m = q.msgs();
    assert!(!got_lock(&m));
    waiting_list(&m, &mut qlist);
    assert_eq!(names(&qlist), ["Ann", "Pia"]);
    start(&mut t, &ann);
    let (texts, m) = p.split();
    assert!(got_lock(&m));
    assert_eq!(last_games(&texts).unwrap(), &json!({"t": "games", "on": false}));
    assert!(got_lock(&q.msgs()));
    t.lock_ok(&p);
    t.lock_ok(&q);
    assert_eq!(matched_room(&p.msgs()).as_deref(), Some("HOST_Ann_1"));
    assert_eq!(matched_room(&q.msgs()).as_deref(), Some("HOST_Ann_1"));
}

/// An older game takes the last seat of the game a chooser looks at, and gives it back with Back.
#[test]
fn an_older_game_taking_the_last_seat_takes_the_game_off_the_list() {
    let mut t = T::new(Config::default());
    serve(&mut t, "Ann", 1, 1, "82.0.0.1", true);
    let p = t.chooser_at("Pia", "82.0.0.3");
    t.quick_match(&p);
    assert_eq!(ids(&games(&p)), [id(1)]);
    let q = t.player_at("Quinn", "82.0.0.4");
    t.quick_match(&q);
    assert_eq!(games(&p)["games"], json!([]));
    t.cancel(&q);
    assert_eq!(games(&p)["games"], json!([[id(1), "Ann", 1, 0, 1, 0, 1]]));
}

/// An older game moves to a game hosted at its home: the seat it leaves is offered to choosers.
#[test]
fn an_older_game_moving_home_frees_its_seat_for_choosers() {
    let mut t = T::new(Config::default());
    let ann = serve(&mut t, "Ann", 1, 1, "82.0.0.1", true);
    let q = t.player_at("Quinn", "82.0.0.4");
    t.quick_match(&q);
    assert_eq!(seats(&ann.texts()), Some(vec!["Quinn"]));
    let r = t.chooser_at("Rex", "82.0.0.5");
    t.quick_match(&r);
    assert_eq!(games(&r)["games"], json!([]));
    let home = serve(&mut t, "Qho", 2, 1, "82.0.0.4", true);
    assert_eq!(seats(&home.texts()), Some(vec!["Quinn"]));
    assert_eq!(seats(&ann.texts()), no_seats());
    assert_eq!(games(&r)["games"], json!([[id(2), "Qho", 1, 1, 2, 0, 1], [id(1), "Ann", 1, 0, 1, 0, 1]]));
}

// ------------------------------------------------------------------ starting

/// The host's start, pressed again (or a kick) once the match is under way, does nothing.
#[test]
fn a_second_start_does_nothing() {
    let mut t = T::new(Config::default());
    let ann = serve(&mut t, "Ann", 1, 1, "82.0.0.1", true);
    let p = t.chooser_at("Pia", "82.0.0.3");
    t.quick_match(&p);
    pick(&mut t, &p, id(1));
    start(&mut t, &ann);
    assert!(got_lock(&p.msgs()));
    t.lock_ok(&p);
    assert_eq!(matched_room(&p.msgs()).as_deref(), Some("HOST_Ann_1"));
    ann.take();
    start(&mut t, &ann);
    kick(&mut t, &ann, "Pia");
    assert!(!got_lock(&p.msgs()));
    assert!(ann.texts().is_empty());
    assert_eq!(t.relay.status()["hosted"], 0);
}

/// The host presses START just as its last player moves to another game: nothing starts and the
/// host hears who is (still) seated.
#[test]
fn a_start_that_crosses_a_player_moving_away_does_nothing() {
    let mut t = T::new(Config::default());
    let ann = serve(&mut t, "Ann", 1, 1, "82.0.0.1", true);
    serve(&mut t, "Ben", 2, 1, "82.0.0.2", true);
    let p = t.chooser_at("Pia", "82.0.0.3");
    t.quick_match(&p);
    pick(&mut t, &p, id(1));
    pick(&mut t, &p, id(2));
    p.take();
    ann.take();
    start(&mut t, &ann);
    assert!(!got_lock(&p.msgs()));
    assert_eq!(seats(&ann.texts()), no_seats());
    assert_eq!(t.relay.status()["hosted"], 2);
}

/// The host removes a player who has just moved to another game: they stay there, the host hears
/// who is seated.
#[test]
fn removing_a_player_who_just_moved_away_leaves_them_in_their_new_game() {
    let mut t = T::new(Config::default());
    let ann = serve(&mut t, "Ann", 2, 1, "82.0.0.1", true);
    let ben = serve(&mut t, "Ben", 2, 1, "82.0.0.2", true);
    let p = t.chooser_at("Pia", "82.0.0.3");
    t.quick_match(&p);
    pick(&mut t, &p, id(1));
    pick(&mut t, &p, id(2));
    for c in [&ann, &ben, &p] {
        c.take();
    }
    kick(&mut t, &ann, "Pia");
    assert_eq!(seats(&ann.texts()), no_seats());
    assert!(ben.take().is_empty());
    assert!(all_games(&p.texts()).is_empty(), "still in Ben's game");
}

/// A pick that crosses the host's start: the player stays in the game that starts.
#[test]
fn a_pick_that_crosses_the_start_does_nothing() {
    let mut t = T::new(Config::default());
    let ann = serve(&mut t, "Ann", 1, 1, "82.0.0.1", true);
    let ben = serve(&mut t, "Ben", 2, 1, "82.0.0.2", true);
    let p = t.chooser_at("Pia", "82.0.0.3");
    t.quick_match(&p);
    pick(&mut t, &p, id(1));
    start(&mut t, &ann);
    ben.take();
    pick(&mut t, &p, id(2)); // sent before the list closed
    assert!(ben.take().is_empty());
    t.lock_ok(&p);
    assert_eq!(matched_room(&p.msgs()).as_deref(), Some("HOST_Ann_1"));
    assert_eq!(t.relay.status()["queued"], 0);
}

// ------------------------------------------------------------------ the same name again

/// A chooser whose game crashed mid-match starts it again under the same name: Quick Match goes
/// straight back into the match, without a list (even with a game waiting at home).
#[test]
fn a_dropped_chooser_goes_back_into_the_match_without_a_list() {
    let mut t = T::new(Config::default());
    let ann = serve(&mut t, "Ann", 1, 1, "82.0.0.1", true);
    let p = t.chooser_at("Pia", "82.0.0.3");
    t.quick_match(&p);
    pick(&mut t, &p, id(1));
    start(&mut t, &ann);
    t.lock_ok(&p);
    assert_eq!(matched_room(&p.msgs()).as_deref(), Some("HOST_Ann_1"));
    t.send(&p, json!({"t": "move", "room": "HOST_Ann_1"}));
    t.bin(&p, &frame(CH_TABLE, &Writer::new().u8(REQUEST).u8(13).u32(7).done()));
    assert!(ann.texts().contains(&json!({"t": "player_joined", "pid": 1, "name": "Pia"})));
    t.disconnect(&p);
    let sis = serve(&mut t, "Sis", 2, 1, "82.0.0.3", true); // a game waits at Pia's home
    sis.take();
    let p2 = t.chooser_at("Pia", "82.0.0.3");
    t.quick_match(&p2);
    let (texts, m) = p2.split();
    assert!(got_lock(&m), "straight back");
    assert!(all_games(&texts).is_empty(), "{texts:?}");
    assert!(sis.texts().is_empty(), "not seated at home");
    t.lock_ok(&p2);
    assert_eq!(matched_room(&p2.msgs()).as_deref(), Some("HOST_Ann_1"));
}

/// A second game under the same name while the first is alive is another player ("Pia2"); the
/// first keeps her seat.
#[test]
fn a_second_live_connection_with_the_same_name_is_another_player() {
    let mut t = T::new(Config::default());
    let ann = serve(&mut t, "Ann", 2, 1, "82.0.0.1", true);
    let p = t.chooser_at("Pia", "82.0.0.3");
    t.quick_match(&p);
    pick(&mut t, &p, id(1));
    assert_eq!(seats(&ann.texts()), Some(vec!["Pia"]));
    let p2 = t.conn_from(Some("82.0.0.3"));
    t.send(&p2, json!({"t": "hello", "v": 1, "name": "Pia", "pick": true}));
    assert_eq!(p2.texts()[0]["name"], "Pia2");
    assert_eq!(p.closed(), None);
    t.send(&p2, json!({"t": "join", "room": "CasinoSystem"}));
    t.quick_match(&p2);
    let g = games(&p2);
    assert_eq!(g["seat"], 0);
    assert_eq!(g["games"], json!([[id(1), "Ann", 1, 1, 2, 0, 1]]));
    pick(&mut t, &p2, id(1));
    assert_eq!(seats(&ann.texts()), Some(vec!["Pia", "Pia2"]));
}

/// Sent away from one game at home, a chooser is seated in another game at home by itself.
#[test]
fn sent_away_at_home_a_chooser_is_seated_in_another_game_at_home() {
    let mut t = T::new(Config::default());
    let bro = serve(&mut t, "Bro", 2, 1, "82.0.0.3", true);
    t.tick(1_000);
    let sis = serve(&mut t, "Sis", 2, 1, "82.0.0.3", true);
    let p = t.chooser_at("Pia", "82.0.0.3");
    t.quick_match(&p);
    assert_eq!(games(&p)["seat"], id(2), "the newest at home");
    bro.take();
    kick(&mut t, &sis, "Pia");
    let g = games(&p);
    assert_eq!(g["seat"], id(1));
    assert_eq!(ids(&g), [id(1)], "Sis's game is not offered");
    assert_eq!(seats(&bro.texts()), Some(vec!["Pia"]));
}

// ------------------------------------------------------------------ kicks are the host's

/// A kick holds for the host, not just for that game: the same host's next game (another room)
/// is not offered to the player either, while another host's is.
#[test]
fn a_kick_holds_for_the_host_s_next_game() {
    let mut t = T::new(Config::default());
    let ann = serve(&mut t, "Ann", 1, 1, "82.0.0.1", true);
    let p = t.chooser_at("Pia", "82.0.0.3");
    t.quick_match(&p);
    pick(&mut t, &p, id(1));
    kick(&mut t, &ann, "Pia");
    t.send(&ann, json!({"t": "unserve"}));
    t.send(&ann, json!({"t": "serve", "room": "HOST_Ann_2", "players": 1, "name": "Ann", "hero": 1, "approve": true}));
    let ben = serve(&mut t, "Ben", 1, 1, "82.0.0.2", true);
    let g = games(&p);
    assert_eq!(offered(&g), ["Ben"], "Ann's next game is not offered");
    pick(&mut t, &p, id(2));
    assert_eq!(games(&p)["seat"], 0, "nor taken");
    assert_eq!(seats(&ann.texts()), no_seats());
    pick(&mut t, &p, id(3));
    assert_eq!(seats(&ben.texts()), Some(vec!["Pia"]));
    // an older game's player sent away is not seated in the host's next game by itself
    let q = t.player_at("Quinn", "82.0.0.4");
    t.quick_match(&q); // Ben's game is full
    assert_eq!(seats(&ann.texts()), Some(vec!["Quinn"]));
    kick(&mut t, &ann, "Quinn");
    t.send(&ann, json!({"t": "serve", "room": "HOST_Ann_3", "players": 1, "name": "Ann", "hero": 1, "approve": true}));
    assert_eq!(seats(&ann.texts()), no_seats(), "Quinn waits");
}

/// A kick also holds for the player's address: a stranger who comes back under another name
/// from there is not offered that host's game (another host's is).
#[test]
fn a_kick_holds_for_the_player_s_address() {
    let mut t = T::new(Config::default());
    let ann = serve(&mut t, "Ann", 2, 1, "82.0.0.1", true);
    let _ben = serve(&mut t, "Ben", 1, 1, "82.0.0.2", true);
    let p = t.chooser_at("Troll", "82.0.0.66");
    t.quick_match(&p);
    pick(&mut t, &p, id(1));
    kick(&mut t, &ann, "Troll");
    t.disconnect(&p);
    ann.take();
    let p2 = t.chooser_at("Nice", "82.0.0.66");
    t.quick_match(&p2);
    assert_eq!(offered(&games(&p2)), ["Ben"]);
    pick(&mut t, &p2, id(1));
    assert_eq!(games(&p2)["seat"], 0);
    // an older game from there is not seated in it by itself either
    let old = t.player_at("Old", "82.0.0.66");
    t.quick_match(&old);
    assert_eq!(seats(&ann.texts()), None, "nobody new in Ann's game");
    // someone from elsewhere is offered it
    let r = t.chooser_at("Rex", "82.0.0.5");
    t.quick_match(&r);
    assert_eq!(offered(&games(&r)), ["Ann"]);
}

/// The address of a player at the host's own network does not count (a LAN relay, the host's
/// family): only the name is kept away.
#[test]
fn a_kick_at_the_host_s_own_network_holds_only_for_the_name() {
    for (host, player, sibling) in [("82.0.0.1", "82.0.0.1", "82.0.0.1"), ("127.0.0.1", "192.168.0.20", "192.168.0.21")] {
        let mut t = T::new(Config::default());
        let ann = serve(&mut t, "Ann", 2, 1, host, true);
        let p = t.chooser_at("Pia", player);
        t.quick_match(&p);
        assert_eq!(games(&p)["seat"], id(1), "{host}: at home");
        kick(&mut t, &ann, "Pia");
        ann.take();
        let s = t.chooser_at("Sis", sibling);
        t.quick_match(&s);
        assert_eq!(games(&s)["seat"], id(1), "{host}: Sis is seated at home");
        assert_eq!(seats(&ann.texts()), Some(vec!["Sis"]));
        assert_eq!(games(&p)["seat"], 0, "{host}: Pia stays away");
    }
}

/// The 10 minutes end by themselves: the tick at the end of them offers the game again and seats
/// the older game's player, nothing else needs to happen; a moment before, nothing changes.
#[test]
fn a_kick_ends_exactly_after_ten_minutes() {
    let mut t = T::new(Config::default());
    let ann = serve(&mut t, "Ann", 2, 1, "82.0.0.1", true);
    let p = t.chooser_at("Pia", "82.0.0.3");
    t.quick_match(&p);
    pick(&mut t, &p, id(1));
    let q = t.player_at("Quinn", "82.0.0.4");
    t.quick_match(&q);
    kick(&mut t, &ann, "Pia");
    kick(&mut t, &ann, "Quinn");
    for c in [&ann, &p, &q] {
        c.take();
    }
    wait(&mut t, 590_000, &[&ann, &p, &q]);
    t.tick(9_999);
    assert!(all_games(&p.texts()).is_empty(), "9:59.999: nothing yet");
    assert!(!got_lock(&q.msgs()));
    assert_eq!(seats(&ann.texts()), None);
    t.tick(1);
    assert_eq!(seats(&ann.texts()), Some(vec!["Quinn"]), "10:00: Quinn is seated again");
    assert_eq!(offered(&games(&p)), ["Ann"], "and Pia is offered the game again");
}

// ------------------------------------------------------------------ picks

/// Picks are rate limited per connection (each one can change every waiting chooser's list): a
/// few at once, then one per 500 ms; a pick beyond that is ignored, not queued.
#[test]
fn picks_are_rate_limited() {
    let mut t = T::new(Config::default());
    let ann = serve(&mut t, "Ann", 1, 1, "82.0.0.1", true);
    let ben = serve(&mut t, "Ben", 1, 1, "82.0.0.2", true);
    let p = t.chooser_at("Pia", "82.0.0.3");
    t.quick_match(&p);
    for i in 0..4 {
        pick(&mut t, &p, id(1 + i % 2));
    }
    assert_eq!(games(&p)["seat"], id(2), "the first four count");
    ann.take();
    pick(&mut t, &p, id(1));
    assert!(all_games(&p.texts()).is_empty(), "the fifth at once is ignored");
    assert_eq!(seats(&ann.texts()), None);
    t.tick(499);
    pick(&mut t, &p, id(1));
    assert!(all_games(&p.texts()).is_empty(), "still too soon");
    t.tick(1);
    pick(&mut t, &p, id(1));
    assert_eq!(games(&p)["seat"], id(1), "one per 500 ms");
    assert_eq!(seats(&ann.texts()), Some(vec!["Pia"]));
    // another player's picks count for them alone
    let r = t.chooser_at("Rex", "82.0.0.4");
    t.quick_match(&r);
    pick(&mut t, &r, id(2));
    assert_eq!(games(&r)["seat"], id(2));
    assert_eq!(seats(&ben.texts()), Some(vec!["Rex"]));
}

/// A game that does not choose (no `pick` in its hello) cannot pick: it was never offered the
/// games, and its picks would change every chooser's list.
#[test]
fn a_pick_from_an_older_game_is_ignored() {
    let mut t = T::new(Config::default());
    let _ann = serve(&mut t, "Ann", 2, 1, "82.0.0.1", true);
    t.tick(1_000);
    let ben = serve(&mut t, "Ben", 2, 1, "82.0.0.2", true);
    let q = t.player_at("Quinn", "82.0.0.4");
    t.quick_match(&q);
    assert_eq!(seats(&ben.texts()), Some(vec!["Quinn"]), "the newest game");
    q.take();
    pick(&mut t, &q, id(1));
    assert!(ben.texts().is_empty(), "Quinn stays put");
    assert!(q.take().is_empty());
}

// ------------------------------------------------------------------ the same name in a match

/// A player in a match whose game restarts after a network blip comes back from the same address
/// before the old connection is silent for long (match traffic): the new connection takes the
/// name over at once (the old one is closed as lost: the host holds the seat), and Quick Match
/// sends them back into the match. From another address it is someone else.
#[test]
fn a_player_back_from_the_same_address_takes_over_their_connection_in_a_match() {
    let mut t = T::new(Config::default());
    let ann = serve(&mut t, "Ann", 1, 1, "82.0.0.1", true);
    let p = t.chooser_at("Pia", "82.0.0.3");
    t.quick_match(&p);
    pick(&mut t, &p, id(1));
    start(&mut t, &ann);
    t.lock_ok(&p);
    assert_eq!(matched_room(&p.msgs()).as_deref(), Some("HOST_Ann_1"));
    t.send(&p, json!({"t": "move", "room": "HOST_Ann_1"}));
    seat_take(&mut t, &p);
    ann.take();
    t.tick(5_000);
    let p2 = t.conn_from(Some("82.0.0.3"));
    t.send(&p2, json!({"t": "hello", "v": 1, "name": "Pia", "pick": true}));
    assert_eq!(p2.texts()[0]["name"], "Pia", "not Pia2");
    assert_eq!(p.closed(), Some(4002));
    assert!(ann.texts().contains(&json!({"t": "player_left", "pid": 1, "lost": 1})), "the host holds the seat");
    t.send(&p2, json!({"t": "join", "room": "CasinoSystem"}));
    t.quick_match(&p2);
    assert!(got_lock(&p2.msgs()), "back into the match");
    let p3 = t.conn_from(Some("82.0.0.9"));
    t.send(&p3, json!({"t": "hello", "v": 1, "name": "Pia", "pick": true}));
    assert_eq!(p3.texts()[0]["name"], "Pia2");
    assert_eq!(p2.closed(), None);
}

/// On a LAN relay a friend over Tailscale (100.64.0.0/10) is at home like the LAN: seated in the
/// relay's own game by itself.
#[test]
fn a_tailscale_friend_is_at_home_on_a_lan_relay() {
    let mut t = T::new(Config::default());
    let lan = serve(&mut t, "Lan", 2, 1, "127.0.0.1", true);
    let p = t.chooser_at("Pia", "100.101.102.103");
    t.quick_match(&p);
    let g = games(&p);
    assert_eq!(g["seat"], id(1));
    assert_eq!(g["games"][0][5], 1, "home");
    assert_eq!(seats(&lan.texts()), Some(vec!["Pia"]));
}

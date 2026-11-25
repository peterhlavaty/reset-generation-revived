//! The relay core without sockets: a fake clock and sinks that collect what each client gets.

use rg_server::relay::{Config, Out, Relay};
use rg_server::wire::*;
use serde_json::{json, Value};
use std::cell::RefCell;
use std::rc::Rc;

/// A casino message: (channel, type, action, rest).
type Msg = (u8, u8, u8, Vec<u8>);

struct T {
    relay: Relay,
    now: u64,
}

#[derive(Clone)]
struct C {
    id: u64,
    inbox: Rc<RefCell<Vec<Out>>>,
}

impl C {
    fn take(&self) -> Vec<Out> {
        std::mem::take(&mut *self.inbox.borrow_mut())
    }
    fn texts(&self) -> Vec<Value> {
        self.take()
            .into_iter()
            .filter_map(|o| match o {
                Out::Text(s) => Some(serde_json::from_str(&s).unwrap()),
                _ => None,
            })
            .collect()
    }
    /// Casino / table messages received: (channel, type, action, rest).
    fn msgs(&self) -> Vec<(u8, u8, u8, Vec<u8>)> {
        let mut out = Vec::new();
        for o in self.take() {
            if let Out::Binary(b) = o {
                for (ch, data) in parse_frames(&b) {
                    if ch >= 0x11 && data.len() >= 2 {
                        out.push((ch, data[0], data[1], data[2..].to_vec()));
                    }
                }
            }
        }
        out
    }
    /// Text and casino messages received.
    fn split(&self) -> (Vec<Value>, Vec<Msg>) {
        let all = self.take();
        let (texts, bins): (Vec<Out>, Vec<Out>) = all.into_iter().partition(|o| matches!(o, Out::Text(_)));
        self.inbox.borrow_mut().extend(bins);
        let msgs = self.msgs();
        let texts = texts
            .into_iter()
            .filter_map(|o| match o {
                Out::Text(s) => Some(serde_json::from_str(&s).unwrap()),
                _ => None,
            })
            .collect();
        (texts, msgs)
    }
    fn closed(&self) -> Option<u16> {
        self.inbox.borrow().iter().find_map(|o| match o {
            Out::Close(code, _) => Some(*code),
            _ => None,
        })
    }
}

impl T {
    fn new(cfg: Config) -> Self {
        T { relay: Relay::new(cfg, Box::new(|_| {})), now: 1_000_000 }
    }
    fn conn(&mut self) -> C {
        self.conn_from(None)
    }
    fn conn_from(&mut self, addr: Option<&str>) -> C {
        let inbox = Rc::new(RefCell::new(Vec::new()));
        let i2 = inbox.clone();
        let addr = addr.map(|a| a.parse().unwrap());
        let id = self.relay.connect_from(self.now, Box::new(move |o| i2.borrow_mut().push(o)), addr);
        C { id, inbox }
    }
    fn send(&mut self, c: &C, m: Value) {
        self.relay.on_text(self.now, c.id, &m.to_string());
    }
    fn bin(&mut self, c: &C, b: &[u8]) {
        self.relay.on_binary(self.now, c.id, b);
    }
    fn hello(&mut self, name: &str) -> C {
        let c = self.conn();
        self.send(&c, json!({"t": "hello", "v": 1, "name": name}));
        assert_eq!(c.texts()[0]["t"], "welcome");
        c
    }
    /// Logged in and in the casino lobby.
    fn player(&mut self, name: &str) -> C {
        let c = self.hello(name);
        self.send(&c, json!({"t": "join", "room": "CasinoSystem"}));
        c.take();
        c
    }
    /// In the casino lobby, connected from this address.
    fn player_at(&mut self, name: &str, addr: &str) -> C {
        let c = self.conn_from(Some(addr));
        self.send(&c, json!({"t": "hello", "v": 1, "name": name}));
        self.send(&c, json!({"t": "join", "room": "CasinoSystem"}));
        c.take();
        c
    }
    /// A host's game as the engine offers it since 2026-11-25 (its name and hero, an address).
    fn host_at(&mut self, nick: &str, players: u32, hero: i32, addr: &str) -> C {
        let h = self.conn_from(Some(addr));
        self.send(&h, json!({"t": "hello", "v": 1, "name": "RGHost"}));
        let room = format!("HOST_{nick}_1");
        self.send(&h, json!({"t": "serve", "room": room, "players": players, "name": nick, "hero": hero}));
        h.take();
        h
    }
    /// A player whose game lets them choose a hosted game (since 2026-11-25), in the casino lobby.
    fn chooser_at(&mut self, name: &str, addr: &str) -> C {
        let c = self.conn_from(Some(addr));
        self.send(&c, json!({"t": "hello", "v": 1, "name": name, "pick": true}));
        self.send(&c, json!({"t": "join", "room": "CasinoSystem"}));
        c.take();
        c
    }
    /// A host's game that starts when the host says so (since 2026-11-25).
    fn host_approving(&mut self, nick: &str, players: u32, addr: &str) -> C {
        let h = self.conn_from(Some(addr));
        self.send(&h, json!({"t": "hello", "v": 1, "name": "RGHost"}));
        let room = format!("HOST_{nick}_1");
        self.send(&h, json!({"t": "serve", "room": room, "players": players, "name": nick, "hero": 7, "approve": true}));
        h.take();
        h
    }
    fn cancel(&mut self, c: &C) {
        self.bin(c, &frame(CH_MATCHMAKER, &Writer::new().u8(REQUEST).u8(10).done()));
    }
    fn quick_match(&mut self, c: &C) {
        let req = frame(CH_MATCHMAKER, &Writer::new().u8(REQUEST).u8(9).str("PLUMBER").str("FALSE").done());
        self.bin(c, &req);
    }
    fn lock_ok(&mut self, c: &C) {
        self.bin(c, &frame(CH_CASINO, &Writer::new().u8(RESPONSE).u8(29).u8(0).u32(0).done()));
    }
    /// A host player's game serving `room` for `players` players.
    fn host(&mut self, name: &str, room: &str, players: u32) -> C {
        let h = self.hello(name);
        self.send(&h, json!({"t": "serve", "room": room, "players": players}));
        assert_eq!(h.texts(), vec![json!({"t": "serving", "room": room})]);
        h
    }
    fn tick(&mut self, ms: u64) {
        self.now += ms;
        self.relay.tick(self.now);
    }
    fn disconnect(&mut self, c: &C) {
        self.relay.disconnect(self.now, c.id);
    }
}

/// The room named in an EVENT CASINO_MATCHED_UP, if this player got one.
fn matched_room(msgs: &[(u8, u8, u8, Vec<u8>)]) -> Option<String> {
    msgs.iter().find(|m| m.1 == EVENT && m.2 == 31).map(|m| {
        let mut r = Reader::new(&m.3);
        r.str().unwrap();
        r.str().unwrap()
    })
}

/// The "Players in Match" list after these messages: the Quick Match answer, then events 9
/// (add) and 10 (remove); (id, name, hero) in order.
fn waiting_list(msgs: &[(u8, u8, u8, Vec<u8>)], list: &mut Vec<(String, String, String)>) {
    let get = |p: &[(String, String)], k: &str| p.iter().find(|(key, _)| key == k).map(|(_, v)| v.clone()).unwrap();
    for (_, t, action, rest) in msgs {
        let mut r = Reader::new(rest);
        match (*t, *action) {
            (RESPONSE, 9) => {
                r.u8().unwrap();
                r.u32().unwrap();
                list.clear();
                for _ in 0..r.u8().unwrap() {
                    let p = r.kv().unwrap();
                    list.push((get(&p, "id"), get(&p, "name"), get(&p, "heroType")));
                }
            }
            (EVENT, 9) => {
                let id = r.str().unwrap();
                let p = r.kv().unwrap();
                assert!(!list.iter().any(|e| e.0 == id), "{id} listed twice");
                list.push((id, get(&p, "name"), get(&p, "heroType")));
            }
            (EVENT, 10) => {
                let id = r.str().unwrap();
                list.retain(|e| e.0 != id);
            }
            _ => {}
        }
    }
}

/// The last `games` message among these.
fn last_games(texts: &[Value]) -> Option<&Value> {
    texts.iter().rev().find(|t| t["t"] == "games")
}

/// The hosts of the games offered in a `games` message.
fn offered(g: &Value) -> Vec<&str> {
    g["games"].as_array().unwrap().iter().map(|e| e[1].as_str().unwrap()).collect()
}

/// The seated players a host last heard of.
fn seats(texts: &[Value]) -> Option<Vec<&str>> {
    let s = texts.iter().rev().find(|t| t["t"] == "seats")?;
    Some(s["names"].as_array().unwrap().iter().map(|n| n.as_str().unwrap()).collect())
}

fn names(list: &[(String, String, String)]) -> Vec<&str> {
    list.iter().map(|e| e.1.as_str()).collect()
}

fn got_lock(msgs: &[(u8, u8, u8, Vec<u8>)]) -> bool {
    msgs.iter().any(|m| m.0 == CH_CASINO && m.1 == REQUEST && m.2 == 29)
}

#[test]
fn hello_checks_the_version_and_the_name() {
    let mut t = T::new(Config::default());
    let c = t.conn();
    t.send(&c, json!({"t": "join", "room": "x"}));
    assert_eq!(c.texts()[0]["code"], "not_logged_in");
    t.send(&c, json!({"t": "hello", "v": 1, "name": "bad name!"}));
    assert_eq!(c.texts()[0]["code"], "bad_name");
    t.relay.on_text(t.now, c.id, "{not json");
    assert_eq!(c.texts()[0]["code"], "bad_json");
    t.send(&c, json!({"t": "hello", "v": 99, "name": "ok"}));
    assert_eq!(c.closed(), Some(4000));
    assert_eq!(c.texts()[0]["code"], "version");
}

#[test]
fn duplicate_names_get_a_suffix_that_fits_fifteen_characters() {
    let mut t = T::new(Config::default());
    t.hello("Twin");
    let b = t.conn();
    t.send(&b, json!({"t": "hello", "v": 1, "name": "twin"}));
    assert_eq!(b.texts()[0]["name"], "twin2");
    t.hello("ABCDEFGHIJKLMNO");
    let d = t.conn();
    t.send(&d, json!({"t": "hello", "v": 1, "name": "ABCDEFGHIJKLMNO"}));
    assert_eq!(d.texts()[0]["name"], "ABCDEFGHIJKLMN2");
}

#[test]
fn rooms_relay_in_order_to_the_others_and_hold_four() {
    let mut t = T::new(Config::default());
    let ps: Vec<C> = (0..5).map(|i| t.hello(&format!("P{i}"))).collect();
    t.send(&ps[1], json!({"t": "join", "room": "Room1"}));
    assert_eq!(ps[1].texts()[0]["code"], "not_found");
    t.send(&ps[0], json!({"t": "create", "room": "Room1"}));
    assert_eq!(ps[0].texts()[0], json!({"t": "joined", "op": "create", "room": "Room1", "members": ["P0"]}));
    t.send(&ps[1], json!({"t": "join", "room": "room1"})); // case-insensitive
    assert_eq!(ps[1].texts()[0]["members"], json!(["P0", "P1"]));
    assert_eq!(ps[0].texts()[0], json!({"t": "peer_joined", "room": "Room1", "name": "P1"}));
    for i in 0..20u8 {
        t.bin(&ps[0], &[i, 7]);
    }
    let got: Vec<Out> = ps[1].take().into_iter().filter(|o| matches!(o, Out::Binary(b) if b.len() == 2)).collect();
    assert_eq!(got, (0..20u8).map(|i| Out::Binary(vec![i, 7])).collect::<Vec<_>>());
    assert!(ps[0].take().iter().all(|o| !matches!(o, Out::Binary(b) if b.len() == 2)), "no echo");
    for p in &ps[2..4] {
        t.send(p, json!({"t": "join", "room": "Room1"}));
    }
    t.send(&ps[4], json!({"t": "join", "room": "Room1"}));
    assert_eq!(ps[4].texts()[0]["code"], "full");
    t.disconnect(&ps[3]);
    assert!(ps[0].texts().contains(&json!({"t": "peer_left", "room": "Room1", "name": "P3", "reason": "disconnected"})));
    t.send(&ps[4], json!({"t": "join", "room": "Room1"}));
    assert_eq!(ps[4].texts()[0]["members"].as_array().unwrap().len(), 4);
    t.bin(&ps[4], &[0u8; 2049]);
    assert_eq!(ps[4].texts()[0]["code"], "bad_size");
}

/// Node relay bug: a truncated casino request threw out of the message handler, which takes the
/// whole Node process down (every player's connection with it). Malformed input must only be
/// ignored.
#[test]
fn truncated_casino_and_table_messages_are_ignored() {
    let mut t = T::new(Config::default());
    let a = t.player("Ann");
    for bad in [
        frame(CH_CASINO, &[REQUEST]),                                  // no action
        frame(CH_CASINO, &[REQUEST, 0, 1, 0]),                         // login: short version
        frame(CH_MATCHMAKER, &Writer::new().u8(REQUEST).u8(9).u32(1).u32(500).done()), // short hero
        frame(CH_MATCHMAKER, &[REQUEST, 8, 1]),                        // enter: short casino id
        frame(CH_CASINO, &Writer::new().u8(REQUEST).u8(0).u32(1).u32(0x2000_0000).u32(u32::MAX).done()),
    ] {
        t.bin(&a, &bad);
    }
    t.quick_match(&a); // still works afterwards
    let m = a.msgs();
    assert!(m.iter().any(|m| m.1 == RESPONSE && m.2 == 9), "{m:?}");
    // the same in a game room
    let h = t.host("RGHost", "HOST_Ann", 1);
    t.lock_ok(&a);
    t.send(&a, json!({"t": "move", "room": "HOST_Ann"}));
    t.bin(&a, &frame(CH_TABLE, &[REQUEST]));
    t.bin(&a, &frame(CH_COMMAND, &[5, 1, 0]));
    t.bin(&a, &frame(CH_TABLE, &Writer::new().u8(REQUEST).u8(13).u32(7).done()));
    assert_eq!(h.texts(), vec![json!({"t": "player_joined", "pid": 1, "name": "Ann"})]);
}

#[test]
fn quick_match_waits_for_a_hosted_game_and_fills_it() {
    let mut t = T::new(Config::default());
    let bob = t.player("Bob");
    t.quick_match(&bob);
    let m = bob.msgs();
    assert!(!got_lock(&m), "no host yet: Bob waits");
    let cid = t.player("Cid");
    t.quick_match(&cid);
    // Bob sees Cid on the "Players in Match" screen (EVENT 9)
    assert!(bob.msgs().iter().any(|m| m.1 == EVENT && m.2 == 9));
    cid.take();
    let _h = t.host("RGHost", "HOST_Ann_1", 2);
    let (mb, mc) = (bob.msgs(), cid.msgs());
    assert!(got_lock(&mb) && got_lock(&mc));
    // introductions: each gets EVENT CASINO_ENTER (8) for the other
    assert!(mb.iter().any(|m| m.1 == EVENT && m.2 == 8));
    t.lock_ok(&bob);
    assert_eq!(matched_room(&bob.msgs()), None, "MATCHED_UP waits for every lock answer");
    t.lock_ok(&cid);
    assert_eq!(matched_room(&bob.msgs()).as_deref(), Some("HOST_Ann_1"));
    assert_eq!(matched_room(&cid.msgs()).as_deref(), Some("HOST_Ann_1"));
}

#[test]
fn matched_up_is_sent_after_the_lock_timeout() {
    let mut t = T::new(Config::default());
    let (a, b) = (t.player("Gus"), t.player("Hal"));
    t.host("RGHost", "HOST_X", 2);
    t.quick_match(&a);
    t.quick_match(&b);
    t.lock_ok(&a);
    t.tick(2_000);
    assert_eq!(matched_room(&a.msgs()), None);
    t.tick(1_500);
    assert_eq!(matched_room(&a.msgs()).as_deref(), Some("HOST_X"));
    assert_eq!(matched_room(&b.msgs()).as_deref(), Some("HOST_X"));
}

#[test]
fn match_size_forms_rooms_without_a_host() {
    let mut t = T::new(Config { match_size: 2, ..Config::default() });
    let (a, b) = (t.player("A1"), t.player("B1"));
    t.quick_match(&a);
    t.quick_match(&b);
    t.lock_ok(&a);
    t.lock_ok(&b);
    assert_eq!(matched_room(&a.msgs()).as_deref(), Some("QM_1"));
}

/// Into a hosted match: hosted game, one player, matched and seated.
fn hosted_match(t: &mut T, player: &str) -> (C, C) {
    let h = t.host("RGHost", "HOST_Ann", 1);
    let p = t.player(player);
    t.quick_match(&p);
    t.lock_ok(&p);
    assert_eq!(matched_room(&p.msgs()).as_deref(), Some("HOST_Ann"));
    t.send(&p, json!({"t": "move", "room": "HOST_Ann"}));
    t.bin(&p, &frame(CH_TABLE, &Writer::new().u8(REQUEST).u8(13).u32(7).done()));
    assert_eq!(h.texts(), vec![json!({"t": "player_joined", "pid": 1, "name": player})]);
    p.take();
    (h, p)
}

/// Going back into a match is no longer limited to 3 minutes: a player whose connection dropped
/// goes straight back with Quick Match for as long as the host serves the match.
#[test]
fn a_dropped_player_goes_back_into_the_match_however_late() {
    let mut t = T::new(Config::default());
    let (h, bob) = hosted_match(&mut t, "Bob");
    t.disconnect(&bob); // Bob's game crashes mid-match
    assert_eq!(h.texts(), vec![json!({"t": "player_left", "pid": 1, "lost": 1})]);
    let cid = t.player("Cid");
    t.quick_match(&cid); // someone else waits for a new game
    assert!(!got_lock(&cid.msgs()));
    for _ in 0..120 {
        t.tick(60_000); // two hours later, the host still serves the match
        t.send(&h, json!({"t": "ping"}));
        t.send(&cid, json!({"t": "ping"}));
    }
    let bob2 = t.player("Bob");
    t.quick_match(&bob2);
    let m = bob2.msgs();
    assert!(got_lock(&m), "Bob goes straight back");
    t.lock_ok(&bob2);
    assert_eq!(matched_room(&bob2.msgs()).as_deref(), Some("HOST_Ann"));
    t.send(&bob2, json!({"t": "move", "room": "HOST_Ann"}));
    t.bin(&bob2, &frame(CH_TABLE, &Writer::new().u8(REQUEST).u8(13).u32(7).done()));
    let ctl = h.texts();
    assert!(ctl.contains(&json!({"t": "player_joined", "pid": 2, "name": "Bob"})), "{ctl:?}");
    // only once per drop
    t.quick_match(&bob2);
    assert!(!got_lock(&bob2.msgs()));
}

#[test]
fn no_way_back_after_a_normal_leave_after_the_match_or_past_a_set_limit() {
    // left the normal way (surrendered)
    let mut t = T::new(Config::default());
    let (_h, dan) = hosted_match(&mut t, "Dan");
    t.send(&dan, json!({"t": "move", "room": "CasinoSystem"}));
    dan.take();
    t.quick_match(&dan);
    assert!(!got_lock(&dan.msgs()));
    // the match ended (the host's listener closed)
    let mut t = T::new(Config::default());
    let (h, eve) = hosted_match(&mut t, "Eve");
    t.disconnect(&eve);
    t.send(&h, json!({"t": "unserve"}));
    let eve2 = t.player("Eve");
    t.quick_match(&eve2);
    assert!(!got_lock(&eve2.msgs()));
    // a server started with --rejoin-secs
    let mut t = T::new(Config { rejoin_ms: Some(60_000), ..Config::default() });
    let (h, fay) = hosted_match(&mut t, "Fay");
    t.disconnect(&fay);
    t.tick(30_000);
    t.send(&h, json!({"t": "ping"}));
    t.tick(31_000);
    let fay2 = t.player("Fay");
    t.quick_match(&fay2);
    assert!(!got_lock(&fay2.msgs()));
}

#[test]
fn a_hosted_room_stays_while_served_so_its_last_player_can_come_back() {
    let mut t = T::new(Config::default());
    let (h, bob) = hosted_match(&mut t, "Bob");
    t.disconnect(&bob); // a 2-player match: the only client drops
    let lister = t.hello("Lister");
    t.send(&lister, json!({"t": "rooms"}));
    let rooms = lister.texts()[0]["rooms"].clone();
    assert!(rooms.as_array().unwrap().iter().any(|r| r["name"] == "HOST_Ann"), "{rooms}");
    t.send(&h, json!({"t": "unserve"}));
    t.send(&lister, json!({"t": "rooms"}));
    let rooms = lister.texts()[0]["rooms"].clone();
    assert!(!rooms.as_array().unwrap().iter().any(|r| r["name"] == "HOST_Ann"), "{rooms}");
}

#[test]
fn a_new_connection_takes_over_the_name_of_a_silent_one() {
    let mut t = T::new(Config::default());
    let a = t.hello("Bob");
    t.now += 5_000;
    let b = t.conn();
    t.send(&b, json!({"t": "hello", "v": 1, "name": "Bob"}));
    assert_eq!(b.texts()[0]["name"], "Bob2", "the first one is alive");
    t.disconnect(&b);
    t.now += 30_000; // a has been silent for 35 s
    let c = t.conn();
    t.send(&c, json!({"t": "hello", "v": 1, "name": "Bob"}));
    assert_eq!(c.texts()[0]["name"], "Bob");
    assert_eq!(a.closed(), Some(4002));
}

#[test]
fn a_casino_player_keeps_their_id_between_visits() {
    let mut t = T::new(Config::default());
    let ann = t.player("Ann");
    let login = frame(CH_CASINO, &Writer::new().u8(REQUEST).u8(0).u32(1).str("Reset Generation v. 1.1.3").done());
    let id_of = |t: &mut T, c: &C| {
        t.bin(c, &login);
        let m = c.msgs().into_iter().find(|m| m.1 == RESPONSE && m.2 == 0).unwrap();
        let mut r = Reader::new(&m.3);
        r.u8().unwrap();
        r.u32().unwrap();
        r.kv().unwrap().into_iter().find(|(k, _)| k == "id").unwrap().1
    };
    let first = id_of(&mut t, &ann);
    t.send(&ann, json!({"t": "move", "room": "Elsewhere"}));
    t.send(&ann, json!({"t": "move", "room": "CasinoSystem"}));
    ann.take();
    assert_eq!(id_of(&mut t, &ann), first);
}

#[test]
fn idle_connections_are_closed() {
    let mut t = T::new(Config::default());
    let a = t.hello("Idle");
    let b = t.hello("Busy");
    for _ in 0..7 {
        t.tick(10_000);
        t.send(&b, json!({"t": "ping"}));
    }
    assert_eq!(a.closed(), Some(4001));
    assert_eq!(b.closed(), None);
}

#[test]
fn game_room_greets_once_everyone_is_in_and_routes_the_match() {
    let mut t = T::new(Config::default());
    let h = t.host("RGHost", "HOST_G", 2);
    let (a, b) = (t.player("A"), t.player("B"));
    t.quick_match(&a);
    t.quick_match(&b);
    t.lock_ok(&a);
    t.lock_ok(&b);
    a.take();
    b.take();
    t.send(&a, json!({"t": "move", "room": "HOST_G"}));
    assert!(a.take().iter().all(|o| !matches!(o, Out::Binary(_))), "silent until both are in");
    t.send(&b, json!({"t": "move", "room": "HOST_G"}));
    let greeting = rg_server::gameroom::greeting();
    assert!(a.take().contains(&Out::Binary(greeting.clone())));
    assert!(b.take().contains(&Out::Binary(greeting)));
    for (c, pid) in [(&a, 7u32), (&b, 8)] {
        t.bin(c, &frame(CH_TABLE, &Writer::new().u8(REQUEST).u8(13).u32(pid).done()));
    }
    h.take();
    // player -> server: [u16 session id][message]; server -> player: MT packets
    t.bin(&a, &mt_packets(CH_GAME, &[9, 8, 7])[0]);
    assert_eq!(h.take(), vec![Out::Binary(vec![1, 0, 9, 8, 7])]);
    let mut big = vec![2u8, 0];
    big.extend(std::iter::repeat_n(5u8, 2000));
    t.bin(&h, &big);
    let mut st = MtState::default();
    let got: Vec<_> = b
        .take()
        .into_iter()
        .filter_map(|o| if let Out::Binary(p) = o { Some(p) } else { None })
        .flat_map(|p| mt_messages(&mut st, &p))
        .filter(|m| m.0 == CH_GAME)
        .map(|m| m.1.len())
        .collect();
    assert_eq!(got, vec![2000], "one reassembled 2000-byte message");
    // other channels go to the other players unchanged
    a.take();
    t.bin(&a, &frame(0x30, &[1, 2]));
    assert!(b.take().contains(&Out::Binary(frame(0x30, &[1, 2]))));
    assert!(a.take().is_empty(), "no echo");
}

/// A 3-4 player game still waiting for players no longer holds up the others: every hosted game
/// has its own seats, and the newest takes a player first.
#[test]
fn hosted_games_fill_independently_newest_first() {
    let mut t = T::new(Config::default());
    let _old = t.host_at("Ann", 3, 1, "82.0.0.1"); // hosted long ago, waits for 3
    t.tick(60_000);
    let _new = t.host_at("Ben", 1, 7, "82.0.0.2"); // a friend just hosted a 2-player game
    let p = t.player_at("Pia", "82.0.0.3");
    t.quick_match(&p);
    let m = p.msgs();
    let mut list = Vec::new();
    waiting_list(&m, &mut list);
    assert_eq!(list, vec![("900000002".into(), "Ben".into(), "NINJA".into())], "the list names the host and hero");
    assert!(got_lock(&m), "Ben's game is full: it starts");
    t.lock_ok(&p);
    assert_eq!(matched_room(&p.msgs()).as_deref(), Some("HOST_Ben_1"));
    // the next player goes into Ann's game, the only one left
    let q = t.player_at("Quinn", "82.0.0.4");
    t.quick_match(&q);
    let m = q.msgs();
    assert!(!got_lock(&m));
    waiting_list(&m, &mut list);
    assert_eq!(names(&list), ["Ann"]);
    assert_eq!(t.relay.status()["hosted"], 1);
    assert_eq!(t.relay.status()["queued"], 1);
}

/// A game hosted from the player's own network (the same public address) comes first, even
/// when another is newer; a player already seated elsewhere moves to it.
#[test]
fn a_game_from_the_same_home_comes_first() {
    let mut t = T::new(Config::default());
    let _ann = t.host_at("Ann", 3, 1, "81.0.0.1");
    let p = t.player_at("Pia", "81.0.0.9");
    t.quick_match(&p);
    let mut list = Vec::new();
    waiting_list(&p.msgs(), &mut list);
    assert_eq!(names(&list), ["Ann"]);
    // someone at Pia's home hosts: Pia moves over (and that game is full)
    let _sis = t.host_at("Sis", 1, -1, "81.0.0.9");
    let m = p.msgs();
    waiting_list(&m, &mut list);
    assert_eq!(names(&list), ["Sis"]);
    assert!(got_lock(&m));
    // the same with IPv6: one /64 is one home
    let _hal = t.host_at("Hal", 1, 2, "2a02:1:2:3::10");
    let _ida = t.host_at("Ida", 3, 2, "2a02:9:9:9::10");
    let q = t.player_at("Quinn", "2a02:1:2:3:abcd::7");
    t.quick_match(&q);
    t.lock_ok(&q);
    assert_eq!(matched_room(&q.msgs()).as_deref(), Some("HOST_Hal_1"));
}

/// Back on the waiting screen and Quick Match again: the next game; once all were seen, round
/// again. The list follows (the host and the players seated with them).
#[test]
fn back_and_quick_match_again_goes_to_the_next_game() {
    let mut t = T::new(Config::default());
    t.host_at("Ann", 3, 1, "82.0.0.1");
    t.tick(1_000);
    t.host_at("Ben", 3, 1, "82.0.0.2");
    let p = t.player_at("Pia", "82.0.0.3");
    let mut list = Vec::new();
    t.quick_match(&p);
    waiting_list(&p.msgs(), &mut list);
    assert_eq!(names(&list), ["Ben"]);
    let q = t.player_at("Quinn", "82.0.0.4");
    t.quick_match(&q);
    let mut qlist = Vec::new();
    waiting_list(&q.msgs(), &mut qlist);
    assert_eq!(names(&qlist), ["Ben", "Pia"], "seated with Pia in Ben's game");
    waiting_list(&p.msgs(), &mut list);
    assert_eq!(names(&list), ["Ben", "Quinn"]);
    // Pia wants another game
    t.cancel(&p);
    waiting_list(&q.msgs(), &mut qlist);
    assert_eq!(names(&qlist), ["Ben"]);
    t.quick_match(&p);
    waiting_list(&p.msgs(), &mut list);
    assert_eq!(names(&list), ["Ann"]);
    t.cancel(&p);
    t.quick_match(&p);
    waiting_list(&p.msgs(), &mut list);
    assert_eq!(names(&list), ["Ben", "Quinn"], "seen them all: round again");
}

/// A host who stops waiting: the players seated in that game get another (or wait).
#[test]
fn players_of_a_game_whose_host_stops_get_another() {
    let mut t = T::new(Config::default());
    let ann = t.host_at("Ann", 2, 1, "82.0.0.1");
    t.tick(1_000);
    let ben = t.host_at("Ben", 2, 1, "82.0.0.2");
    let p = t.player_at("Pia", "82.0.0.3");
    let mut list = Vec::new();
    t.quick_match(&p);
    t.send(&ben, json!({"t": "unserve"}));
    waiting_list(&p.msgs(), &mut list);
    assert_eq!(names(&list), ["Ann"]);
    t.disconnect(&ann);
    waiting_list(&p.msgs(), &mut list);
    assert!(list.is_empty(), "no game left: Pia waits");
    // a host's game from before 2026-11-25 (no name or hero): the room names the host
    t.host("RGHost", "HOST_Old_Timer_3", 1);
    let m = p.msgs();
    waiting_list(&m, &mut list);
    assert_eq!(list, vec![("900000003".into(), "Old_Timer".into(), "PLUMBER".into())]);
    assert!(got_lock(&m));
}

/// A player whose game chooses sees the waiting games, picks one, and waits there until its host
/// starts it.
#[test]
fn a_player_picks_a_game_and_its_host_starts_it() {
    let mut t = T::new(Config::default());
    let ann = t.host_approving("Ann", 1, "82.0.0.1");
    t.tick(1_000);
    let _ben = t.host_approving("Ben", 2, "82.0.0.2");
    let p = t.chooser_at("Pia", "82.0.0.3");
    t.quick_match(&p);
    let (texts, m) = p.split();
    let g = last_games(&texts).expect("the games are offered");
    assert_eq!(g["on"], true);
    assert_eq!(g["seat"], 0, "not seated: Pia chooses");
    assert_eq!(offered(g), ["Ben", "Ann"], "newest first");
    assert_eq!(g["games"][1], json!([900000001, "Ann", 7, 0, 1, 0, 1]));
    let mut list = Vec::new();
    waiting_list(&m, &mut list);
    assert!(list.is_empty());
    // Pia picks Ann's game: it is full, but waits for Ann
    t.send(&p, json!({"t": "pick", "id": 900000001}));
    let (texts, m) = p.split();
    assert_eq!(last_games(&texts).unwrap()["seat"], 900000001);
    waiting_list(&m, &mut list);
    assert_eq!(names(&list), ["Ann"]);
    assert!(!got_lock(&m), "the host starts it");
    assert_eq!(seats(&ann.texts()), Some(vec!["Pia"]));
    t.send(&ann, json!({"t": "start"}));
    let (texts, m) = p.split();
    assert!(got_lock(&m));
    assert_eq!(last_games(&texts).unwrap()["on"], false, "no longer choosing");
    t.lock_ok(&p);
    assert_eq!(matched_room(&p.msgs()).as_deref(), Some("HOST_Ann_1"));
    assert_eq!(t.relay.status()["hosted"], 1, "Ben's game still waits");
}

/// The host sends a player away: that game is no longer offered to them, and an older game's
/// player is not put back into it. A host's start before the seats are taken does nothing.
#[test]
fn the_host_sends_a_player_away() {
    let mut t = T::new(Config::default());
    let ann = t.host_approving("Ann", 2, "82.0.0.1");
    let p = t.chooser_at("Pia", "82.0.0.3");
    t.quick_match(&p);
    t.send(&p, json!({"t": "pick", "id": 900000001}));
    let q = t.player_at("Quinn", "82.0.0.4"); // an older game: seated by itself
    t.quick_match(&q);
    let mut qlist = Vec::new();
    waiting_list(&q.msgs(), &mut qlist);
    assert_eq!(names(&qlist), ["Ann", "Pia"]);
    assert_eq!(seats(&ann.texts()), Some(vec!["Pia", "Quinn"]));
    t.send(&ann, json!({"t": "kick", "name": "quinn"}));
    assert_eq!(seats(&ann.texts()), Some(vec!["Pia"]));
    waiting_list(&q.msgs(), &mut qlist);
    assert!(qlist.is_empty(), "Quinn waits for another game");
    let (texts, m) = p.split();
    assert_eq!(last_games(&texts).unwrap()["games"][0][3], 1, "one seat taken");
    t.send(&ann, json!({"t": "start"}));
    assert!(!got_lock(&p.msgs()), "a seat is free: no start");
    assert_eq!(seats(&ann.texts()), Some(vec!["Pia"]), "the host hears who is there");
    // the next player who chooses is offered Ann's game; Quinn's next try does not get it
    let r = t.chooser_at("Rex", "82.0.0.5");
    t.quick_match(&r);
    assert_eq!(offered(last_games(&r.texts()).unwrap()), ["Ann"]);
    t.cancel(&q);
    t.quick_match(&q);
    waiting_list(&q.msgs(), &mut qlist);
    assert_eq!(names(&qlist), ["Rex"], "not in Ann's game: waiting, like Rex");
    drop(m);
}

/// A game hosted at home seats a player who chooses by itself; they can still pick another,
/// and stay there. Back ends the list; an older host's game starts once its seats are taken.
#[test]
fn choosing_with_a_game_at_home_and_an_older_host() {
    let mut t = T::new(Config::default());
    let _ann = t.host_approving("Ann", 2, "81.0.0.1");
    t.tick(1_000);
    let _old = t.host("RGHost", "HOST_Old_7", 1);
    let p = t.chooser_at("Pia", "81.0.0.1");
    t.quick_match(&p);
    let (texts, m) = p.split();
    let g = last_games(&texts).unwrap();
    assert_eq!(g["seat"], 900000001, "seated at home");
    assert_eq!(offered(g), ["Ann", "Old"], "home first");
    let mut list = Vec::new();
    waiting_list(&m, &mut list);
    assert_eq!(names(&list), ["Ann"]);
    t.cancel(&p);
    assert_eq!(last_games(&p.texts()).unwrap()["on"], false);
    t.quick_match(&p);
    p.take();
    t.send(&p, json!({"t": "pick", "id": 900000002}));
    let (_, m) = p.split();
    assert!(got_lock(&m), "the older host's game starts when full");
    t.lock_ok(&p);
    assert_eq!(matched_room(&p.msgs()).as_deref(), Some("HOST_Old_7"));
}

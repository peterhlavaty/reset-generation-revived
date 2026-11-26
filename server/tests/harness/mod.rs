// The relay test harness, shared by the files in server/tests: include!("harness/mod.rs").
// (A directory, so cargo does not build it as a test of its own.)

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
        T { relay: Relay::new(cfg, Box::new(|s| if std::env::var_os("RG_TEST_LOG").is_some() { eprintln!("{s}") })), now: 1_000_000 }
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

/// The MATCHED_UP room and its player count, if this player got one.
#[allow(dead_code)] // not every test file looks at the count
fn matched(msgs: &[Msg]) -> Option<(String, u32)> {
    msgs.iter().find(|m| m.1 == EVENT && m.2 == 31).map(|m| {
        let mut r = Reader::new(&m.3);
        r.str().unwrap();
        let room = r.str().unwrap();
        r.str().unwrap();
        r.u8().unwrap();
        r.u8().unwrap();
        (room, r.u32().unwrap())
    })
}

/// SEAT_TAKE in the game room: the player's session starts there (the host hears of it).
#[allow(dead_code)] // not every test file goes into game rooms
fn seat_take(t: &mut T, c: &C) {
    t.bin(c, &frame(CH_TABLE, &Writer::new().u8(REQUEST).u8(13).u32(7).done()));
}

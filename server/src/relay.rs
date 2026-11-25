//! The relay: named rooms + ordered delivery, standing in for N-Gage Arena's SNAP game rooms.
//!
//! * `CasinoSystem`: the permanent lobby; its "game server" is the matchmaker bot ([`Casino`]).
//! * Matchmade game rooms: served by a [`GameRoom`] bot (handshake, seating, routing the match
//!   between the players and the room's game server, a host player's engine).
//! * Other rooms (max 4 players): packets are relayed to the other members in order.
//!
//! The engine only starts talking in a room after the room's server has spoken, so every joiner
//! gets a greeting (matchmade rooms: once all matched players are present).
//!
//! Protocol (WebSocket at `/ws`, docs/MULTIPLAYER.md): text frames are JSON control messages
//! `{"t": <type>, …}`, binary frames are game packets (≤ [`MAX_PACKET`] bytes).
//!
//! The core is a single-threaded state machine without I/O: the transport ([`crate::net`])
//! feeds it events with the current time and it answers through each client's [`Sink`].

use crate::casino::{Casino, Offer};
use crate::gameroom::GameRoom;
use crate::wire::{frame, hex, CH_SYSTEM};
use indexmap::IndexMap;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::net::IpAddr;

pub const PROTOCOL_VERSION: u64 = 1;
pub const MAX_PACKET: usize = 2048; // the engine's shared net buffer
pub const MAX_ROOM_MEMBERS: usize = 4; // createGameRoom(name, null, 4, …)
pub const MAX_NAME: usize = 15; // SMArenaValidator
/// Clients ping every 15 s: a connection silent for longer is presumed dead (see `hello`).
pub const STALE_MS: u64 = 20_000;
/// How long a matchmade room waits for all its players before it greets the ones present.
pub const START_WAIT_MS: u64 = 10_000;
const LOBBY: &str = "CasinoSystem";
const LOBBY_KEY: &str = "casinosystem";

pub type ClientId = u64;

/// What the relay sends to one connection.
#[derive(Debug, Clone, PartialEq)]
pub enum Out {
    Text(String),
    Binary(Vec<u8>),
    Close(u16, &'static str),
}

pub type Sink = Box<dyn Fn(Out)>;

pub struct Client {
    pub id: ClientId,
    pub name: Option<String>,
    /// Where it connects from (the player's public address behind the proxy), if known.
    pub addr: Option<IpAddr>,
    /// Its game lets the player choose a hosted game (`hello` with `pick`, since 2026-11-25).
    pub picks: bool,
    room: Option<String>,
    serves: Option<String>,
    pub last_seen: u64,
    greeted: HashSet<String>,
    out: Sink,
}

struct Expected {
    count: usize,
    until: u64,
}

/// Shared state the bots use: the connections, the rooms the matchmaker announced, the game
/// servers, the clock and the log.
pub struct Net {
    pub clients: IndexMap<ClientId, Client>,
    expected: HashMap<String, Expected>,
    servers: HashMap<String, ClientId>,
    log: Box<dyn Fn(&str)>,
    pub now: u64,
}

impl Net {
    pub fn send(&self, c: ClientId, bytes: Vec<u8>) {
        if let Some(c) = self.clients.get(&c) {
            (c.out)(Out::Binary(bytes));
        }
    }
    pub fn ctl(&self, c: ClientId, msg: Value) {
        if let Some(c) = self.clients.get(&c) {
            (c.out)(Out::Text(msg.to_string()));
        }
    }
    pub fn addr(&self, c: ClientId) -> Option<IpAddr> {
        self.clients.get(&c).and_then(|c| c.addr)
    }
    pub fn picks(&self, c: ClientId) -> bool {
        self.clients.get(&c).is_some_and(|c| c.picks)
    }
    pub fn name(&self, c: ClientId) -> &str {
        self.clients.get(&c).and_then(|c| c.name.as_deref()).unwrap_or("?")
    }
    pub fn log(&self, s: &str) {
        (self.log)(s);
    }
    /// The matchmaker announces how many players a game room will have.
    pub fn expect_room(&mut self, name: &str, count: usize) {
        self.expected.insert(name.to_lowercase(), Expected { count, until: self.now + START_WAIT_MS });
    }
    pub fn is_served(&self, room: &str) -> bool {
        self.servers.contains_key(&room.to_lowercase())
    }
}

enum Bot {
    Plain,
    Casino,
    Game(Box<GameRoom>),
}

struct Room {
    name: String,
    members: Vec<ClientId>,
    max: usize,
    permanent: bool,
    bot: Bot,
    start_at: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct Config {
    /// Players per automatic `QM_n` room (0: Quick Match only fills games hosted by a player).
    pub match_size: usize,
    /// How long a player whose connection dropped may go back into their match (None: as long
    /// as the host serves it).
    pub rejoin_ms: Option<u64>,
    /// Connections silent for longer than this are closed (clients ping every 15 s).
    pub idle_ms: u64,
    pub log_packets: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config { match_size: 0, rejoin_ms: None, idle_ms: 60_000, log_packets: false }
    }
}

#[derive(Default, Clone, Copy)]
struct Stats {
    packets: u64,
    bytes: u64,
    connections: u64,
}

pub struct Relay {
    pub net: Net,
    rooms: IndexMap<String, Room>,
    casino: Casino,
    cfg: Config,
    next_id: u64,
    stats: Stats,
    next_reap: u64,
}

fn valid_name(s: &str) -> bool {
    (1..=MAX_NAME).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

fn valid_room(s: &str) -> bool {
    (1..=64).contains(&s.len()) && s.bytes().all(|b| (0x20..=0x7e).contains(&b))
}

impl Relay {
    pub fn new(cfg: Config, log: Box<dyn Fn(&str)>) -> Self {
        let mut casino = Casino::new();
        casino.match_size = cfg.match_size;
        casino.rejoin_ms = cfg.rejoin_ms;
        let mut rooms = IndexMap::new();
        rooms.insert(
            LOBBY_KEY.to_string(),
            Room { name: LOBBY.into(), members: Vec::new(), max: 1000, permanent: true, bot: Bot::Casino, start_at: None },
        );
        Relay {
            net: Net { clients: IndexMap::new(), expected: HashMap::new(), servers: HashMap::new(), log, now: 0 },
            rooms,
            casino,
            cfg,
            next_id: 1,
            stats: Stats::default(),
            next_reap: 0,
        }
    }

    /// Registers a new connection.
    pub fn connect(&mut self, now: u64, out: Sink) -> ClientId {
        self.connect_from(now, out, None)
    }

    /// Registers a new connection from this address (hosted games from a player's own network
    /// come first in Quick Match).
    pub fn connect_from(&mut self, now: u64, out: Sink, addr: Option<IpAddr>) -> ClientId {
        self.net.now = now;
        let id = self.next_id;
        self.next_id += 1;
        self.net.clients.insert(
            id,
            Client { id, name: None, addr, picks: false, room: None, serves: None, last_seen: now, greeted: HashSet::new(), out },
        );
        self.stats.connections += 1;
        id
    }

    fn ctl(&self, c: ClientId, msg: Value) {
        self.net.ctl(c, msg);
    }

    pub fn on_text(&mut self, now: u64, c: ClientId, text: &str) {
        self.net.now = now;
        let Some(client) = self.net.clients.get_mut(&c) else { return };
        client.last_seen = now;
        let logged_in = client.name.is_some();
        let Ok(m) = serde_json::from_str::<Value>(text) else {
            return self.ctl(c, json!({"t": "error", "code": "bad_json"}));
        };
        let Some(t) = m.get("t").and_then(Value::as_str) else {
            return self.ctl(c, json!({"t": "error", "code": "bad_message"}));
        };
        if t != "hello" && t != "ping" && !logged_in {
            return self.ctl(c, json!({"t": "error", "op": t, "code": "not_logged_in"}));
        }
        let room = m.get("room").and_then(Value::as_str);
        match t {
            "hello" => self.hello(c, &m),
            "ping" => self.ctl(c, json!({"t": "pong", "ts": m.get("ts").cloned().unwrap_or(Value::Null)})),
            "create" | "join" | "move" => self.enter(c, room, t),
            "leave" => self.leave(c, false, true),
            "serve" => self.serve(c, room, &m),
            "unserve" => {
                if self.net.clients[&c].serves.is_some() {
                    self.unserve(c);
                }
            }
            "rooms" => self.ctl(c, json!({"t": "rooms", "rooms": self.room_list()})),
            // Quick Match: a player picks a hosted game; its host starts it or sends a player away
            "pick" => {
                if let Some(id) = m.get("id").and_then(Value::as_u64).and_then(|id| u32::try_from(id).ok()) {
                    self.casino.choose(&mut self.net, c, id);
                }
            }
            "start" => self.casino.host_start(&mut self.net, c),
            "kick" => {
                if let Some(name) = m.get("name").and_then(Value::as_str) {
                    self.casino.host_kick(&mut self.net, c, name);
                }
            }
            _ => self.ctl(c, json!({"t": "error", "op": t, "code": "unknown_type"})),
        }
    }

    fn hello(&mut self, c: ClientId, m: &Value) {
        if m.get("v").and_then(Value::as_f64) != Some(PROTOCOL_VERSION as f64) {
            self.ctl(c, json!({"t": "error", "op": "hello", "code": "version", "server": PROTOCOL_VERSION}));
            return self.close(c, 4000, "protocol version");
        }
        let Some(base) = m.get("name").and_then(Value::as_str).filter(|n| valid_name(n)) else {
            return self.ctl(c, json!({"t": "error", "op": "hello", "code": "bad_name"}));
        };
        // Names are display names only (no accounts); make them unique per server. A connection
        // with this name that has been silent for longer than a ping interval is the same player
        // whose game crashed or lost the network: it is dropped, so they keep their name (and can
        // go back into their match).
        let now = self.net.now;
        let stale: Vec<ClientId> = self
            .net
            .clients
            .values()
            .filter(|o| {
                o.id != c
                    && o.serves.is_none()
                    && o.name.as_deref().is_some_and(|n| n.eq_ignore_ascii_case(base))
                    && o.last_seen + STALE_MS < now
            })
            .map(|o| o.id)
            .collect();
        for o in stale {
            self.net.log(&format!("replacing silent connection c{o} of {}", self.net.name(o)));
            self.close(o, 4002, "replaced");
        }
        let taken: HashSet<String> = self
            .net
            .clients
            .values()
            .filter(|o| o.id != c)
            .filter_map(|o| o.name.as_ref().map(|n| n.to_lowercase()))
            .collect();
        let mut name = base.to_string();
        let mut i = 2;
        while taken.contains(&name.to_lowercase()) {
            let suffix = i.to_string();
            name = format!("{}{suffix}", &base[..base.len().min(MAX_NAME - suffix.len())]);
            i += 1;
        }
        self.net.log(&format!("hello c{c} as {name}"));
        let cl = self.net.clients.get_mut(&c).unwrap();
        cl.name = Some(name.clone());
        cl.picks = m.get("pick").and_then(Value::as_bool) == Some(true);
        self.ctl(c, json!({"t": "welcome", "v": PROTOCOL_VERSION, "id": format!("c{c}"), "name": name}));
    }

    /// create: make the room (joins it if it exists and has space: two players creating the same
    /// private room should meet, not fail); join: only an existing room; move: join if it
    /// exists, else create (SNAPMobileClient.moveToRoom).
    fn enter(&mut self, c: ClientId, room_name: Option<&str>, op: &str) {
        let Some(room_name) = room_name.filter(|r| valid_room(r)) else {
            return self.ctl(c, json!({"t": "error", "op": op, "code": "bad_room"}));
        };
        let key = room_name.to_lowercase();
        if let Some(cur) = self.net.clients[&c].room.clone() {
            if cur == key {
                let r = &self.rooms[&key];
                return self.ctl(c, json!({"t": "joined", "op": op, "room": r.name, "members": self.member_names(&key)}));
            }
            self.leave(c, false, true);
        }
        if let Some(room) = self.rooms.get(&key) {
            if room.members.len() >= room.max {
                return self.ctl(c, json!({"t": "error", "op": op, "room": room_name, "code": "full"}));
            }
        } else {
            let exp = self.net.expected.remove(&key);
            if op == "join" && exp.is_none() {
                return self.ctl(c, json!({"t": "error", "op": op, "room": room_name, "code": "not_found"}));
            }
            let mut room = Room {
                name: room_name.to_string(),
                members: Vec::new(),
                max: MAX_ROOM_MEMBERS,
                permanent: false,
                bot: Bot::Plain,
                start_at: None,
            };
            if let Some(exp) = exp {
                let mut g = GameRoom::new(room_name, exp.count);
                if let Some(&server) = self.net.servers.get(&key) {
                    g.attach_server(&self.net, server);
                }
                room.bot = Bot::Game(Box::new(g));
                // start without the missing players after a while (they would never be greeted)
                room.start_at = Some(exp.until.max(self.net.now));
            }
            self.rooms.insert(key.clone(), room);
            self.net.log(&format!("room + {room_name}"));
        }
        let room = self.rooms.get_mut(&key).unwrap();
        room.members.push(c);
        self.net.clients.get_mut(&c).unwrap().room = Some(key.clone());
        let name = self.net.name(c).to_string();
        self.net.log(&format!("{name} -> {} ({})", room.name, room.members.len()));
        let room_name = room.name.clone();
        let members = self.member_names(&key);
        self.ctl(c, json!({"t": "joined", "op": op, "room": room_name, "members": members}));
        let room = self.rooms.get_mut(&key).unwrap();
        match &mut room.bot {
            Bot::Game(g) => g.join(&self.net, c),
            Bot::Casino => self.casino.join(&self.net, c),
            Bot::Plain => greet(&mut self.net, &room.members, &key),
        }
        for &o in room.members.iter().filter(|&&o| o != c) {
            self.net.ctl(o, json!({"t": "peer_joined", "room": room_name, "name": name}));
        }
    }

    /// A game server process (a host player's engine) offers to serve a matchmade room: it is not
    /// a player and is not seated. With `players`, Quick Match fills the room with that many;
    /// `name` and `hero` (the host's, the engine's hero index) show on the joiners' lists; with
    /// `approve` the game starts when the host says so (`start`).
    fn serve(&mut self, c: ClientId, room_name: Option<&str>, m: &Value) {
        let Some(room_name) = room_name.filter(|r| valid_room(r)) else {
            return self.ctl(c, json!({"t": "error", "op": "serve", "code": "bad_room"}));
        };
        let key = room_name.to_lowercase();
        if self.net.clients[&c].room.is_some() {
            self.leave(c, false, true);
        }
        if self.net.servers.get(&key).is_some_and(|&o| o != c) {
            return self.ctl(c, json!({"t": "error", "op": "serve", "room": room_name, "code": "taken"}));
        }
        if self.net.clients[&c].serves.as_ref().is_some_and(|s| *s != key) {
            self.unserve(c); // a host's next game
        }
        self.net.servers.insert(key.clone(), c);
        self.net.clients.get_mut(&c).unwrap().serves = Some(key.clone());
        self.net.log(&format!("{} serves {room_name}", self.net.name(c)));
        self.ctl(c, json!({"t": "serving", "room": room_name}));
        let exists = if let Some(room) = self.rooms.get_mut(&key) {
            if let Bot::Game(g) = &mut room.bot {
                g.attach_server(&self.net, c);
            }
            true
        } else {
            false
        };
        // A host player's game (rg.HostServer): Quick Match fills its room with `players` players.
        if let Some(n) = m.get("players").and_then(Value::as_f64) {
            if n.fract() == 0.0 && n > 0.0 && n < 100.0 && !exists {
                let offer = Offer {
                    room: room_name,
                    players: n as usize,
                    host: m.get("name").and_then(Value::as_str).filter(|n| valid_name(n)),
                    hero: m.get("hero").and_then(Value::as_i64),
                    addr: self.net.addr(c),
                    owner: Some(c),
                    approve: m.get("approve").and_then(Value::as_bool) == Some(true),
                };
                self.casino.add_hosted(&mut self.net, offer);
            }
        }
    }

    fn unserve(&mut self, c: ClientId) {
        let Some(key) = self.net.clients.get_mut(&c).and_then(|cl| cl.serves.take()) else { return };
        self.casino.game_over(&mut self.net, &key);
        self.net.servers.remove(&key);
        if let Some(room) = self.rooms.get_mut(&key) {
            if let Bot::Game(g) = &mut room.bot {
                g.detach_server();
            }
            if room.members.is_empty() && !room.permanent {
                self.remove_room(&key);
            }
        }
    }

    /// `lost`: the connection dropped. `reply`: tell the client it left (a `leave` request).
    fn leave(&mut self, c: ClientId, lost: bool, reply: bool) {
        let Some(key) = self.net.clients.get(&c).and_then(|cl| cl.room.clone()) else {
            if reply {
                self.net.ctl(c, json!({"t": "left", "room": null}));
            }
            return;
        };
        let room = self.rooms.get_mut(&key).expect("client in a missing room");
        room.members.retain(|&m| m != c);
        let name = self.net.clients[&c].name.clone();
        {
            let cl = self.net.clients.get_mut(&c).unwrap();
            cl.greeted.remove(&key);
            cl.room = None;
        }
        match &mut room.bot {
            Bot::Game(g) => {
                g.leave(&self.net, c, lost);
                if let Some(name) = &name {
                    // a game room: may the player come back?
                    if lost {
                        self.casino.dropped_out(&self.net, &key, name);
                    } else {
                        self.casino.left_game(&key, name);
                    }
                }
            }
            Bot::Casino => self.casino.leave(&mut self.net, c),
            Bot::Plain => {}
        }
        let room_name = room.name.clone();
        let reason = if lost { "disconnected" } else { "left" };
        if reply {
            self.net.ctl(c, json!({"t": "left", "room": room_name}));
        }
        for &o in &room.members {
            self.net.ctl(o, json!({"t": "peer_left", "room": room_name, "name": name, "reason": reason}));
        }
        // a hosted match stays while its host serves it: a player whose connection dropped may come back
        if room.members.is_empty() && !room.permanent && !self.net.servers.contains_key(&key) {
            self.remove_room(&key);
        }
    }

    fn remove_room(&mut self, key: &str) {
        if let Some(mut room) = self.rooms.shift_remove(key) {
            if let Bot::Game(g) = &mut room.bot {
                g.close();
            }
            self.net.log(&format!("room - {}", room.name));
        }
    }

    pub fn on_binary(&mut self, now: u64, c: ClientId, bytes: &[u8]) {
        self.net.now = now;
        let Some(client) = self.net.clients.get_mut(&c) else { return };
        client.last_seen = now;
        let (serves, room) = (client.serves.clone(), client.room.clone());
        if let Some(key) = serves {
            if let Some(Room { bot: Bot::Game(g), .. }) = self.rooms.get(&key) {
                g.from_server(&self.net, bytes);
            }
            return;
        }
        let Some(key) = room else {
            return self.ctl(c, json!({"t": "error", "op": "data", "code": "not_in_room"}));
        };
        if bytes.is_empty() || bytes.len() > MAX_PACKET {
            return self.ctl(c, json!({"t": "error", "op": "data", "code": "bad_size"}));
        }
        self.stats.packets += 1;
        self.stats.bytes += bytes.len() as u64;
        let room = self.rooms.get_mut(&key).unwrap();
        if self.cfg.log_packets {
            self.net.log(&format!("pkt {}@{} {}: {}", self.net.name(c), room.name, bytes.len(), hex(bytes)));
        }
        match &mut room.bot {
            Bot::Game(g) => g.packet(&self.net, c, bytes), // server-side logic, not broadcast
            Bot::Casino => self.casino.packet(&mut self.net, c, bytes),
            Bot::Plain => {
                for &o in room.members.iter().filter(|&&o| o != c) {
                    self.net.send(o, bytes.to_vec());
                }
            }
        }
    }

    /// The connection is gone (closed by the client, the network or the relay).
    pub fn disconnect(&mut self, now: u64, c: ClientId) {
        self.net.now = now;
        self.drop_client(c);
    }

    fn drop_client(&mut self, c: ClientId) {
        if !self.net.clients.contains_key(&c) {
            return;
        }
        self.unserve(c);
        self.leave(c, true, false);
        let name = self.net.clients.shift_remove(&c).and_then(|cl| cl.name).unwrap_or_default();
        self.net.log(&format!("bye c{c} {name}"));
    }

    /// Drops a connection and tells the transport to close it.
    fn close(&mut self, c: ClientId, code: u16, reason: &'static str) {
        if let Some(cl) = self.net.clients.get(&c) {
            (cl.out)(Out::Close(code, reason));
        }
        self.drop_client(c);
    }

    /// Timers: keep-alives, lock timeouts, waiting game rooms, idle connections. Call it often
    /// (every few hundred ms).
    pub fn tick(&mut self, now: u64) {
        self.net.now = now;
        self.casino.tick(&mut self.net);
        for room in self.rooms.values_mut() {
            if let Bot::Game(g) = &mut room.bot {
                if room.start_at.is_some_and(|t| now >= t) {
                    room.start_at = None;
                    g.start(&self.net);
                }
                g.tick(&self.net);
            }
        }
        if now >= self.next_reap {
            self.next_reap = now + 10_000;
            let idle: Vec<ClientId> = self
                .net
                .clients
                .values()
                .filter(|c| c.last_seen + self.cfg.idle_ms < now)
                .map(|c| c.id)
                .collect();
            for c in idle {
                self.close(c, 4001, "idle");
            }
            // rooms announced by the matchmaker that nobody entered
            self.net.expected.retain(|_, e| e.until + 60_000 > now);
        }
    }

    fn member_names(&self, key: &str) -> Vec<String> {
        self.rooms[key].members.iter().map(|&m| self.net.name(m).to_string()).collect()
    }

    fn room_list(&self) -> Vec<Value> {
        self.rooms
            .values()
            .map(|r| json!({"name": r.name, "players": r.members.len(), "max": r.max, "lobby": r.permanent}))
            .collect()
    }

    /// Hosted games waiting for players (LAN discovery answers).
    pub fn hosted_waiting(&self) -> usize {
        self.casino.hosted_waiting()
    }

    pub fn status(&self) -> Value {
        json!({
            "players": self.net.clients.len(),
            "rooms": self.rooms.values().filter(|r| !r.permanent).count(),
            "hosted": self.casino.hosted_waiting(),
            "queued": self.casino.queued(),
            "packets": self.stats.packets,
            "bytes": self.stats.bytes,
            "connections": self.stats.connections,
        })
    }
}

/// Rooms without a bot greet each joiner at once (the engine waits for the server to speak).
fn greet(net: &mut Net, members: &[ClientId], key: &str) {
    let hello = frame(CH_SYSTEM, &20000u32.to_le_bytes());
    for &m in members {
        if let Some(cl) = net.clients.get_mut(&m) {
            if cl.greeted.insert(key.to_string()) {
                (cl.out)(Out::Binary(hello.clone()));
            }
        }
    }
}

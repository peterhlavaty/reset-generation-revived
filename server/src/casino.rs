//! The "CasinoSystem": the server-side matchmaker of the original N-Gage Arena, rebuilt from the
//! engine's message classes (docs/MULTIPLAYER.md, "Casino protocol").
//!
//! Quick Match fills games hosted by a player (F5 in the Windows build: the host's engine is the
//! game server). With `match_size` > 0 it also forms `QM_n` rooms of that many players, which need
//! a separate game server.
//!
//! A player whose connection drops in a running hosted match is remembered; their next Quick
//! Match request sends them straight back into it, for as long as the host serves that match
//! (no time limit by default, `rejoin_ms` sets one).

use crate::relay::{ClientId, Net};
use crate::wire::*;
use std::collections::{HashMap, HashSet};

const KEEPALIVE_MS: u64 = 10_000; // clients drop a peer that is silent for 20 s
const LOCK_TIMEOUT_MS: u64 = 3_000; // wait this long for LOCK_PLAYER_IN_GAME answers
pub const MATCHMAKER_NAME: &str = "QuickMatch";

#[derive(Default)]
struct Player {
    id: u32,
    hero: Option<String>,
    random: Option<String>,
}

#[derive(Clone, Copy)]
struct Waiting {
    client: ClientId,
    ch: u8,
}

struct Hosted {
    room: String,
    players: usize,
}

struct Match {
    group: Vec<Waiting>,
    room: String,
    locked: HashSet<ClientId>,
    deadline: u64,
    done: bool,
}

#[derive(Default)]
pub struct Casino {
    players: HashMap<ClientId, Player>,
    queue: Vec<Waiting>,
    matches: Vec<Match>,
    hosted: Vec<Hosted>,
    /// Hosted matches being played: room key -> room name.
    running: HashMap<String, String>,
    /// Room key -> (lower-case) names that lost the connection in it -> when.
    dropped: HashMap<String, HashMap<String, u64>>,
    /// Nickname -> player id (a player keeps their id between casino visits).
    ids: HashMap<String, u32>,
    next_player_id: u32,
    next_table: u32,
    pub match_size: usize,
    /// How long a dropped player may come back (None: as long as the host serves the match).
    pub rejoin_ms: Option<u64>,
    ping_seq: u8,
    next_keepalive: Option<u64>,
}

/// The matchmaker casino descriptor: `[u8 kind][u32 id][u32 a][u32 b][u32 c][string name]`
/// (kind 1 = matchmaker, 2 = friendly; a..c unknown).
fn write_casino(w: &mut Writer) {
    w.u8(1).u32(1).u32(0).u32(0).u32(0).str(MATCHMAKER_NAME);
}

impl Casino {
    pub fn new() -> Self {
        Casino { next_table: 1, ping_seq: 1, ..Default::default() }
    }

    /// Hosted games waiting for players (LAN discovery answers, /status).
    pub fn hosted_waiting(&self) -> usize {
        self.hosted.len()
    }

    pub fn queued(&self) -> usize {
        self.queue.len()
    }

    /// The profile key/value map the client reads (it crashes on a missing key).
    fn profile(&self, net: &Net, c: ClientId) -> Vec<(String, String)> {
        let p = self.players.get(&c);
        vec![
            ("id".into(), p.map_or(0, |p| p.id).to_string()),
            ("name".into(), net.name(c).to_string()),
            ("fameLevel".into(), "0".into()),
            ("heroType".into(), p.and_then(|p| p.hero.clone()).unwrap_or_else(|| "PLUMBER".into())),
            ("random".into(), p.and_then(|p| p.random.clone()).unwrap_or_else(|| "FALSE".into())),
        ]
    }

    pub fn join(&mut self, net: &Net, c: ClientId) {
        let id = self.player_id(net.name(c));
        self.players.insert(c, Player { id, ..Default::default() });
        // The client starts its casino session once the room's server has spoken. Like a peer,
        // the server opens with a transport hello (ch 2: 00) and its keep-alive timeout.
        net.send(c, frame(CH_CONTROL, &[0]));
        net.send(c, frame(CH_SYSTEM, &20000u32.to_le_bytes()));
        if self.next_keepalive.is_none() {
            self.next_keepalive = Some(net.now + KEEPALIVE_MS);
        }
    }

    /// A player keeps their id when they come back to the casino (after a match). The engine
    /// keys the casino's member map by it and only removes the current id on leave: a changed id
    /// leaves a stale entry holding the engine's own user object, which it deletes when the
    /// connection drops, and then crashes.
    fn player_id(&mut self, name: &str) -> u32 {
        if let Some(&id) = self.ids.get(name) {
            if !self.players.values().any(|p| p.id == id) {
                return id;
            }
        }
        self.next_player_id += 1;
        if self.ids.len() >= 100_000 {
            self.ids.clear(); // a long-running public server: forget names not seen for a while
        }
        self.ids.insert(name.to_string(), self.next_player_id);
        self.next_player_id
    }

    pub fn leave(&mut self, net: &Net, c: ClientId) {
        if self.queue.iter().any(|q| q.client == c) {
            self.notify_queue(net, c, 10);
        }
        self.players.remove(&c);
        self.queue.retain(|q| q.client != c);
    }

    /// EVENT CASINO_MATCHMAKING (9, `[string id][profile]`) / CASINO_CANCEL_MATCHMAKING (10,
    /// `[string id]`): updates the other waiting players' "Players in Match" list.
    fn notify_queue(&self, net: &Net, c: ClientId, action: u8) {
        let p = self.profile(net, c);
        for q in self.queue.iter().filter(|q| q.client != c) {
            reply(net, q.client, EVENT, action, q.ch, |w| {
                w.str(&p[0].1);
                if action == 9 {
                    w.kv(&p);
                }
            });
        }
    }

    /// A host player's game (its engine is the game server) takes the next `players` players.
    pub fn add_hosted(&mut self, net: &mut Net, room: &str, players: usize) {
        self.remove_hosted(&room.to_lowercase());
        self.hosted.push(Hosted { room: room.to_string(), players });
        net.log(&format!("casino: hosted game {room} for {players} player(s)"));
        self.matchmake(net);
    }

    fn remove_hosted(&mut self, key: &str) {
        self.hosted.retain(|h| h.room.to_lowercase() != key);
    }

    /// The host's game server is gone (match over): no one can come back into it.
    pub fn game_over(&mut self, key: &str) {
        self.remove_hosted(key);
        self.running.remove(key);
        self.dropped.remove(key);
    }

    /// A player's connection dropped while in a hosted match (not a normal leave): the next time
    /// they ask for Quick Match they go back into it, the host's engine takes them in again.
    pub fn dropped_out(&mut self, net: &Net, key: &str, name: &str) {
        let Some(room) = self.running.get(key) else { return };
        net.log(&format!("casino: {name} dropped out of {room} (may rejoin)"));
        self.dropped.entry(key.to_string()).or_default().insert(name.to_lowercase(), net.now);
    }

    /// A player left a hosted match the normal way (surrendered, match over): no rejoin.
    pub fn left_game(&mut self, key: &str, name: &str) {
        if let Some(m) = self.dropped.get_mut(key) {
            m.remove(&name.to_lowercase());
        }
    }

    /// The running hosted match this player dropped out of, if they may still go back.
    fn rejoin_room(&mut self, now: u64, name: &str) -> Option<String> {
        let name = name.to_lowercase();
        for (key, names) in self.dropped.iter_mut() {
            let Some(t) = names.remove(&name) else { continue };
            let in_time = self.rejoin_ms.is_none_or(|ms| now.saturating_sub(t) <= ms);
            if let (true, Some(room)) = (in_time, self.running.get(key)) {
                return Some(room.clone());
            }
        }
        None
    }

    /// Forms matches: hosted games first (once enough players are queued); without a hosted game
    /// waiting, `QM_n` rooms of `match_size` players (if enabled).
    fn matchmake(&mut self, net: &mut Net) {
        while !self.hosted.is_empty() && self.queue.len() >= self.hosted[0].players {
            let h = self.hosted.remove(0);
            let group: Vec<Waiting> = self.queue.drain(..h.players).collect();
            self.start_match(net, group, h.room);
        }
        if !self.hosted.is_empty() {
            return; // players wait for the hosted game
        }
        while self.match_size > 0 && self.queue.len() >= self.match_size {
            let group: Vec<Waiting> = self.queue.drain(..self.match_size).collect();
            let room = format!("QM_{}", self.next_table);
            self.next_table += 1;
            self.start_match(net, group, room);
        }
    }

    /// A full group is announced in three steps:
    /// 1. EVENT CASINO_ENTER `[string id][profile]` for each other member (the game room's
    ///    seating events name players by id, resolved among the players the client's casino knows);
    /// 2. REQUEST LOCK_PLAYER_IN_GAME (casino-system channel) -> "Game starting"; the client
    ///    answers with a RESPONSE;
    /// 3. once all have answered (or after a timeout), EVENT CASINO_MATCHED_UP with the room.
    fn start_match(&mut self, net: &Net, group: Vec<Waiting>, room: String) {
        let names: Vec<&str> = group.iter().map(|q| net.name(q.client)).collect();
        net.log(&format!("casino: matched {} -> {room}", names.join(", ")));
        for q in &group {
            for o in group.iter().filter(|o| o.client != q.client) {
                let p = self.profile(net, o.client);
                reply(net, q.client, EVENT, 8, q.ch, |w| {
                    w.str(&p[0].1).kv(&p);
                });
            }
        }
        for q in &group {
            reply(net, q.client, REQUEST, 29, CH_CASINO, |w| {
                w.u32(0);
            });
        }
        self.matches.push(Match {
            group,
            room,
            locked: HashSet::new(),
            deadline: net.now + LOCK_TIMEOUT_MS,
            done: false,
        });
    }

    /// A player acknowledged LOCK_PLAYER_IN_GAME.
    fn locked(&mut self, net: &mut Net, c: ClientId) {
        let Some(i) = self.matches.iter().rposition(|m| !m.done && m.group.iter().any(|q| q.client == c)) else {
            return;
        };
        let m = &mut self.matches[i];
        m.locked.insert(c);
        if m.group.iter().all(|q| m.locked.contains(&q.client)) {
            self.matched_up(net, i);
        }
    }

    /// Tells each player of a locked group the game room to move to.
    fn matched_up(&mut self, net: &mut Net, i: usize) {
        let m = self.matches.remove(i);
        let group: Vec<Waiting> = m.group.into_iter().filter(|q| self.players.contains_key(&q.client)).collect();
        let room = m.room;
        net.expect_room(&room, group.len());
        if net.is_served(&room) {
            self.running.insert(room.to_lowercase(), room.clone());
        }
        for (i, q) in group.iter().enumerate() {
            let b = format!("naf://rg.com:80/{room}?hosted={}", if i == 0 { "yes" } else { "no" });
            reply(net, q.client, EVENT, 31, q.ch, |w| {
                // casino name, table: [string room][string address][u8][u8][u32 players][settings]
                // settings: [u8 kind=0][u8][string address][u8]
                w.str(MATCHMAKER_NAME).str(&room).str(&b).u8(0).u8(1).u32(group.len() as u32);
                w.u8(0).u8(0).str(&b).u8(0);
            });
        }
    }

    pub fn tick(&mut self, net: &mut Net) {
        while let Some(i) = self.matches.iter().position(|m| net.now >= m.deadline) {
            self.matched_up(net, i);
        }
        if let Some(t) = self.next_keepalive {
            if net.now >= t {
                if self.players.is_empty() {
                    self.next_keepalive = None;
                } else {
                    self.ping_seq = self.ping_seq % 255 + 1;
                    for &c in self.players.keys() {
                        net.send(c, frame(CH_SYSTEM, &[self.ping_seq]));
                    }
                    self.next_keepalive = Some(net.now + KEEPALIVE_MS);
                }
            }
        }
    }

    pub fn packet(&mut self, net: &mut Net, c: ClientId, bytes: &[u8]) {
        for (ch, data) in parse_frames(bytes) {
            match ch {
                CH_CASINO | CH_MATCHMAKER | CH_FRIENDLY => self.message(net, c, data, ch),
                CH_COMMAND => net.log(&format!("casino: {} command {}", net.name(c), hex(data))),
                CH_SYSTEM if data.len() == 1 => net.send(c, frame(CH_CONTROL, data)), // pong
                CH_CONTROL if data == [0] => net.send(c, frame(CH_CONTROL, &[0x20])), // accept
                CH_CONTROL | CH_SYSTEM => {} // pong / keep-alive setting
                _ => net.log(&format!("casino: {} ch{} {}", net.name(c), ch, hex(data))),
            }
        }
    }

    fn message(&mut self, net: &mut Net, c: ClientId, data: &[u8], ch: u8) {
        let mut r = Reader::new(data);
        let (Ok(t), Ok(action)) = (r.u8(), r.u8()) else {
            net.log(&format!("casino: {} short message {}", net.name(c), hex(data)));
            return;
        };
        net.log(&format!(
            "casino: {} ch{ch} {} {} {}",
            net.name(c),
            type_name(t),
            action_name(action),
            hex(r.rest())
        ));
        if t == RESPONSE && action == 29 {
            return self.locked(net, c);
        }
        if t != REQUEST {
            return;
        }
        if self.request(net, c, &mut r, action, ch).is_err() {
            net.log(&format!("casino: {} sent a truncated {}", net.name(c), action_name(action)));
        }
    }

    fn request(&mut self, net: &mut Net, c: ClientId, r: &mut Reader, action: u8, ch: u8) -> Res<()> {
        match action {
            0 => {
                // CASINOSYSTEM_LOGIN: u32 version, string client version
                let version = r.u32()?;
                let client_version = r.str()?;
                net.log(&format!("casino: login {} protocol {version} ({client_version})", net.name(c)));
                // response: generic + profile key/value map + u8 has-casino + casino descriptor
                let p = self.profile(net, c);
                respond(net, c, 0, 0, CH_CASINO, |w| {
                    w.kv(&p).u8(1);
                    write_casino(w);
                });
            }
            5 => {
                // GET_STATISTICS -> generic + stats: string, 9 x u32, u32 count (+ items)
                let name = net.name(c).to_string();
                respond(net, c, 5, 0, ch, |w| {
                    w.str(&name);
                    for _ in 0..10 {
                        w.u32(0);
                    }
                });
            }
            6 => {
                // GET_ACHIEVEMENTS -> generic + 200-byte achievement block (none unlocked online)
                respond(net, c, 6, 0, ch, |w| {
                    w.bytes(&[0; 200]);
                });
            }
            8 => {
                // CASINO_ENTER: u32 casino id -> generic + key/value map + casino descriptor
                let id = r.u32()?;
                let p = self.profile(net, c);
                respond(net, c, 8, id, ch, |w| {
                    w.kv(&p);
                    write_casino(w);
                });
            }
            9 => {
                // CASINO_MATCHMAKING: string hero, string flag -> generic + u8 count + kv maps
                let hero = r.str()?;
                let flag = r.str()?;
                if let Some(p) = self.players.get_mut(&c) {
                    p.hero = Some(hero);
                    p.random = Some(flag);
                }
                self.queue.retain(|q| q.client != c);
                // the list is a u8 count: a busy server shows the first 255
                let waiting: Vec<_> = self.queue.iter().take(255).map(|q| self.profile(net, q.client)).collect();
                respond(net, c, 9, 0, ch, |w| {
                    w.u8(waiting.len() as u8);
                    for m in &waiting {
                        w.kv(m);
                    }
                });
                let name = net.name(c).to_string();
                if let Some(back) = self.rejoin_room(net.now, &name) {
                    net.log(&format!("casino: {name} rejoins {back}"));
                    self.start_match(net, vec![Waiting { client: c, ch }], back);
                    return Ok(());
                }
                self.queue.push(Waiting { client: c, ch });
                self.notify_queue(net, c, 9);
                self.matchmake(net);
            }
            10 => {
                // CASINO_CANCEL_MATCHMAKING
                self.notify_queue(net, c, 10);
                self.queue.retain(|q| q.client != c);
                respond(net, c, 10, 0, ch, |_| {});
            }
            _ => respond(net, c, action, 0, ch, |_| {}),
        }
        Ok(())
    }
}

fn reply(net: &Net, c: ClientId, t: u8, action: u8, ch: u8, body: impl FnOnce(&mut Writer)) {
    let mut w = Writer::new();
    w.u8(t).u8(action);
    body(&mut w);
    net.send(c, frame(ch, &w.done()));
}

/// Generic RESPONSE: `[u8 result (0 = ok)][u32 value]` + extra fields.
fn respond(net: &Net, c: ClientId, action: u8, value: u32, ch: u8, extra: impl FnOnce(&mut Writer)) {
    reply(net, c, RESPONSE, action, ch, |w| {
        w.u8(0).u32(value);
        extra(w);
    });
}

//! The "CasinoSystem": the server-side matchmaker of the original N-Gage Arena, rebuilt from the
//! engine's message classes (docs/MULTIPLAYER.md, "Casino protocol").
//!
//! Quick Match fills games hosted by a player (F5: the host's engine is the game server). Each
//! hosted game has its own seats and starts as soon as they are taken, whatever the other games
//! do. A player asking for Quick Match gets a seat right away when a game is waiting:
//!
//! 1. a game hosted from their own network (the same public address: the same home) first;
//! 2. otherwise the newest waiting game (a friend who just hosted, not one left waiting long ago);
//! 3. not one they left with Back: Back and Quick Match again goes on to the next game (and
//!    round again once they have seen them all).
//!
//! The "Players in Match" screen lists the host (by the name they play under) and the players
//! already seated in that game, so a player sees whose game they are in. Without a hosted game
//! players wait and are seated when one is hosted. With `match_size` > 0 (tests) players without a
//! hosted game also form `QM_n` rooms of that many, which need a separate game server.
//!
//! A player whose connection drops in a running hosted match is remembered; their next Quick
//! Match request sends them straight back into it, for as long as the host serves that match
//! (no time limit by default, `rejoin_ms` sets one).

use crate::relay::{ClientId, Net};
use crate::wire::*;
use std::collections::{HashMap, HashSet};
use std::net::IpAddr;

const KEEPALIVE_MS: u64 = 10_000; // clients drop a peer that is silent for 20 s
const LOCK_TIMEOUT_MS: u64 = 3_000; // wait this long for LOCK_PLAYER_IN_GAME answers
/// How long a game a player left with Back is skipped for them.
const DECLINE_MS: u64 = 10 * 60_000;
/// Waiting-list ids of the hosts (players' own ids count up from 1).
const HOST_ID_BASE: u32 = 900_000_000;
/// The waiting list's player count is a u8.
const LIST_MAX: usize = 255;
pub const MATCHMAKER_NAME: &str = "QuickMatch";

/// The heroes by the engine's index, as the profile's `heroType` names them (0x10038940).
pub const HEROES: [&str; 10] = [
    "HEDGEHOG", "PLUMBER", "CYBORG", "LEVEL_50_ELF", "BABE_GUNNER", "MONSTER_TRAINER", "SCIFI_KNIGHT", "NINJA",
    "DR_LOVEBOMBER", "AGGRESSOR",
];

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

/// A game hosted by a player, waiting for its players.
struct Hosted {
    room: String,
    key: String,
    /// Players it takes (not counting the host).
    players: usize,
    /// The host's name and hero for the waiting list.
    host: String,
    hero: Option<&'static str>,
    addr: Option<IpAddr>,
    list_id: u32,
    /// The players Quick Match gave it so far.
    seated: Vec<Waiting>,
}

struct Match {
    group: Vec<Waiting>,
    room: String,
    locked: HashSet<ClientId>,
    deadline: u64,
    done: bool,
}

/// A game being announced on a waiting list: (profile id, profile).
type Entry = (String, Vec<(String, String)>);

#[derive(Default)]
pub struct Casino {
    players: HashMap<ClientId, Player>,
    /// Players waiting for a hosted game (none has a free seat).
    queue: Vec<Waiting>,
    matches: Vec<Match>,
    /// Waiting hosted games, oldest first.
    hosted: Vec<Hosted>,
    /// Hosted matches being played: room key -> room name.
    running: HashMap<String, String>,
    /// Room key -> (lower-case) names that lost the connection in it -> when.
    dropped: HashMap<String, HashMap<String, u64>>,
    /// Lower-case name -> the hosted games (room keys) they left with Back, and when they last did.
    declined: HashMap<String, (Vec<String>, u64)>,
    /// The profile ids each waiting player's "Players in Match" list shows.
    shown: HashMap<ClientId, Vec<String>>,
    /// Nickname -> player id (a player keeps their id between casino visits).
    ids: HashMap<String, u32>,
    next_player_id: u32,
    next_table: u32,
    next_list_id: u32,
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

/// Two addresses of one network: the same IPv4 address (a home behind one router) or the same
/// IPv6 /64.
fn same_network(a: Option<IpAddr>, b: Option<IpAddr>) -> bool {
    let plain = |ip: IpAddr| match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
        v4 => v4,
    };
    match (a.map(plain), b.map(plain)) {
        (Some(IpAddr::V4(x)), Some(IpAddr::V4(y))) => x == y,
        (Some(IpAddr::V6(x)), Some(IpAddr::V6(y))) => x.segments()[..4] == y.segments()[..4],
        _ => false,
    }
}

/// The host's name from a room the engine named `HOST_<name>_<n>` (hosts before 2026-11-25 do
/// not send their name).
fn host_from_room(room: &str) -> String {
    let inner = room.strip_prefix("HOST_").and_then(|r| r.rsplit_once('_')).map(|(name, _)| name);
    inner.filter(|n| !n.is_empty()).unwrap_or(room).to_string()
}

impl Casino {
    pub fn new() -> Self {
        Casino { next_table: 1, ping_seq: 1, ..Default::default() }
    }

    /// Hosted games waiting for players (LAN discovery answers, /status).
    pub fn hosted_waiting(&self) -> usize {
        self.hosted.len()
    }

    /// Players waiting in Quick Match (seated in a hosted game or waiting for one).
    pub fn queued(&self) -> usize {
        self.queue.len() + self.hosted.iter().map(|h| h.seated.len()).sum::<usize>()
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

    pub fn leave(&mut self, net: &mut Net, c: ClientId) {
        let waited = self.withdraw(c).is_some();
        self.players.remove(&c);
        if waited {
            self.matchmake(net);
        }
    }

    /// Takes a player out of Quick Match; returns the hosted game (room key) they were seated in,
    /// or "" if they were waiting for one.
    fn withdraw(&mut self, c: ClientId) -> Option<String> {
        self.shown.remove(&c);
        if let Some(i) = self.queue.iter().position(|q| q.client == c) {
            self.queue.remove(i);
            return Some(String::new());
        }
        let h = self.hosted.iter_mut().find(|h| h.seated.iter().any(|q| q.client == c))?;
        h.seated.retain(|q| q.client != c);
        Some(h.key.clone())
    }

    /// A host player's game (its engine is the game server) takes the next `players` players.
    /// `host` and `hero` (the engine's index, other values: random) are for the waiting list.
    pub fn add_hosted(
        &mut self,
        net: &mut Net,
        room: &str,
        players: usize,
        host: Option<&str>,
        hero: Option<i64>,
        addr: Option<IpAddr>,
    ) {
        let key = room.to_lowercase();
        self.remove_hosted(&key);
        self.next_list_id += 1;
        self.hosted.push(Hosted {
            room: room.to_string(),
            key,
            players,
            host: host.map_or_else(|| host_from_room(room), str::to_string),
            hero: hero.and_then(|h| usize::try_from(h).ok()).and_then(|h| HEROES.get(h).copied()),
            addr,
            list_id: HOST_ID_BASE + self.next_list_id,
            seated: Vec::new(),
        });
        net.log(&format!("casino: hosted game {room} for {players} player(s)"));
        self.matchmake(net);
    }

    /// The game is no longer waiting: its seated players wait again (at the front).
    fn remove_hosted(&mut self, key: &str) {
        if let Some(i) = self.hosted.iter().position(|h| h.key == key) {
            let h = self.hosted.remove(i);
            self.queue.splice(0..0, h.seated);
        }
    }

    /// The host's game server is gone (match over, or they stopped hosting): no one can come
    /// back into it, and players seated in it while it waited get another game.
    pub fn game_over(&mut self, net: &mut Net, key: &str) {
        self.running.remove(key);
        self.dropped.remove(key);
        if self.hosted.iter().any(|h| h.key == key) {
            self.remove_hosted(key);
            self.matchmake(net);
        }
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

    /// Has the player left this hosted game with Back (lately)?
    fn declined(&self, net: &Net, c: ClientId, key: &str) -> bool {
        self.declined.get(&net.name(c).to_lowercase()).is_some_and(|(keys, _)| keys.iter().any(|k| k == key))
    }

    /// The hosted game a waiting player gets: their own network's, else the newest; not one they
    /// left with Back while there is another.
    fn pick(&mut self, net: &Net, c: ClientId) -> Option<usize> {
        let free: Vec<usize> = (0..self.hosted.len()).rev().filter(|&i| self.hosted[i].seated.len() < self.hosted[i].players).collect();
        if free.is_empty() {
            return None;
        }
        let mut open: Vec<usize> = free.iter().copied().filter(|&i| !self.declined(net, c, &self.hosted[i].key)).collect();
        if open.is_empty() {
            // seen them all: round again
            self.declined.remove(&net.name(c).to_lowercase());
            open = free;
        }
        let addr = net.addr(c);
        open.iter().copied().find(|&i| same_network(self.hosted[i].addr, addr)).or(open.first().copied())
    }

    /// Seats waiting players: anyone (also seated elsewhere) in a game from their own network,
    /// then the others in the game `pick` gives them.
    fn place(&mut self, net: &Net) {
        let now = net.now;
        self.declined.retain(|_, (_, t)| now.saturating_sub(*t) < DECLINE_MS);
        let waiting: Vec<(Waiting, Option<usize>)> = self
            .queue
            .iter()
            .map(|&q| (q, None))
            .chain(self.hosted.iter().enumerate().flat_map(|(i, h)| h.seated.iter().map(move |&q| (q, Some(i)))))
            .collect();
        for (q, at) in waiting {
            let addr = net.addr(q.client);
            if at.is_some_and(|i| same_network(self.hosted[i].addr, addr)) {
                continue;
            }
            let home = (0..self.hosted.len()).rev().find(|&i| {
                let h = &self.hosted[i];
                h.seated.len() < h.players && same_network(h.addr, addr) && !self.declined(net, q.client, &h.key)
            });
            if let Some(j) = home {
                match at {
                    Some(i) => self.hosted[i].seated.retain(|o| o.client != q.client),
                    None => self.queue.retain(|o| o.client != q.client),
                }
                self.hosted[j].seated.push(q);
            }
        }
        let mut i = 0;
        while i < self.queue.len() {
            match self.pick(net, self.queue[i].client) {
                Some(j) => {
                    let q = self.queue.remove(i);
                    self.hosted[j].seated.push(q);
                }
                None => i += 1,
            }
        }
    }

    /// Seats waiting players, starts the games whose seats are all taken and updates the
    /// waiting lists. Without a hosted game, `QM_n` rooms of `match_size` players (if enabled).
    fn matchmake(&mut self, net: &mut Net) {
        self.place(net);
        self.refresh(net); // also those whose game starts now see whose it is
        while let Some(i) = self.hosted.iter().position(|h| h.seated.len() >= h.players) {
            let h = self.hosted.remove(i);
            for q in &h.seated {
                self.shown.remove(&q.client);
                self.declined.remove(&net.name(q.client).to_lowercase());
            }
            self.start_match(net, h.seated, h.room);
        }
        if self.hosted.is_empty() {
            while self.match_size > 0 && self.queue.len() >= self.match_size {
                let group: Vec<Waiting> = self.queue.drain(..self.match_size).collect();
                for q in &group {
                    self.shown.remove(&q.client);
                }
                let room = format!("QM_{}", self.next_table);
                self.next_table += 1;
                self.start_match(net, group, room);
            }
        }
        self.refresh(net);
    }

    /// What a waiting player's "Players in Match" list should show: seated in a hosted game, its
    /// host and the other players seated in it; else the others waiting for a game.
    fn list_for(&self, net: &Net, c: ClientId) -> Vec<Entry> {
        let entry = |o: ClientId| {
            let p = self.profile(net, o);
            (p[0].1.clone(), p)
        };
        let Some(h) = self.hosted.iter().find(|h| h.seated.iter().any(|q| q.client == c)) else {
            return self.queue.iter().filter(|q| q.client != c).take(LIST_MAX).map(|q| entry(q.client)).collect();
        };
        let host = vec![
            ("id".to_string(), h.list_id.to_string()),
            ("name".to_string(), h.host.clone()),
            ("fameLevel".to_string(), "0".to_string()),
            ("heroType".to_string(), h.hero.unwrap_or("PLUMBER").to_string()),
            ("random".to_string(), if h.hero.is_some() { "FALSE" } else { "TRUE" }.to_string()),
        ];
        let mut list = vec![(h.list_id.to_string(), host)];
        list.extend(h.seated.iter().filter(|q| q.client != c).map(|q| entry(q.client)));
        list
    }

    /// Brings every waiting player's list up to date: EVENT CASINO_CANCEL_MATCHMAKING (10,
    /// `[string id]`) removes an entry, EVENT CASINO_MATCHMAKING (9, `[string id][profile]`) adds
    /// one.
    fn refresh(&mut self, net: &Net) {
        let waiting: Vec<Waiting> = self.queue.iter().chain(self.hosted.iter().flat_map(|h| h.seated.iter())).copied().collect();
        for q in waiting {
            let want = self.list_for(net, q.client);
            let had = self.shown.remove(&q.client).unwrap_or_default();
            for id in had.iter().filter(|id| !want.iter().any(|(w, _)| w == *id)) {
                reply(net, q.client, EVENT, 10, q.ch, |w| {
                    w.str(id);
                });
            }
            for (id, p) in want.iter().filter(|(id, _)| !had.contains(id)) {
                reply(net, q.client, EVENT, 9, q.ch, |w| {
                    w.str(id).kv(p);
                });
            }
            self.shown.insert(q.client, want.into_iter().map(|(id, _)| id).collect());
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
                self.withdraw(c);
                let name = net.name(c).to_string();
                if let Some(back) = self.rejoin_room(net.now, &name) {
                    respond(net, c, 9, 0, ch, |w| {
                        w.u8(0);
                    });
                    net.log(&format!("casino: {name} rejoins {back}"));
                    self.start_match(net, vec![Waiting { client: c, ch }], back);
                    self.refresh(net);
                    return Ok(());
                }
                // seated first, so the answer lists the game they are in (the screen it opens
                // takes later changes as events)
                self.queue.push(Waiting { client: c, ch });
                self.place(net);
                let list = self.list_for(net, c);
                respond(net, c, 9, 0, ch, |w| {
                    w.u8(list.len() as u8);
                    for (_, p) in &list {
                        w.kv(p);
                    }
                });
                self.shown.insert(c, list.into_iter().map(|(id, _)| id).collect());
                self.matchmake(net);
            }
            10 => {
                // CASINO_CANCEL_MATCHMAKING (Back on the "Players in Match" screen): next time
                // the player gets another game, if there is one
                if let Some(key) = self.withdraw(c).filter(|k| !k.is_empty()) {
                    let d = self.declined.entry(net.name(c).to_lowercase()).or_default();
                    if !d.0.contains(&key) {
                        d.0.push(key);
                    }
                    d.1 = net.now;
                }
                respond(net, c, 10, 0, ch, |_| {});
                self.matchmake(net);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn networks() {
        let ip = |s: &str| Some(s.parse::<IpAddr>().unwrap());
        assert!(same_network(ip("81.2.3.4"), ip("81.2.3.4")));
        assert!(same_network(ip("81.2.3.4"), ip("::ffff:81.2.3.4")));
        assert!(!same_network(ip("81.2.3.4"), ip("81.2.3.5")));
        assert!(same_network(ip("2a02:1:2:3:aaaa::1"), ip("2a02:1:2:3:bbbb::2")));
        assert!(!same_network(ip("2a02:1:2:3::1"), ip("2a02:1:2:4::1")));
        assert!(!same_network(None, None));
    }

    #[test]
    fn host_names_from_rooms() {
        assert_eq!(host_from_room("HOST_Peter_1"), "Peter");
        assert_eq!(host_from_room("HOST_my_name_12"), "my_name");
        assert_eq!(host_from_room("HOST_X"), "HOST_X");
        assert_eq!(host_from_room("QM_3"), "QM_3");
    }
}

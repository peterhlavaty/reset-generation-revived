//! The "CasinoSystem": the server-side matchmaker of the original N-Gage Arena, rebuilt from the
//! engine's message classes (docs/MULTIPLAYER.md, "Casino protocol").
//!
//! Quick Match fills games hosted by a player (F5: the host's engine is the game server). Each
//! hosted game has its own seats, whatever the other games do.
//!
//! Games since 2026-11-25 choose: a player's game shows the waiting games (`games` messages to
//! the client, drawn by native/rt/lobby.c over the "Players in Match" screen) and the player
//! picks one (`pick`); only a game hosted from their own network (the same public address: the
//! same home) seats them by itself. The host sees who sat down (`seats`) and starts the game once
//! its seats are taken (`start`), or sends a player away (`kick`).
//!
//! Older games (and older hosts, whose games start as soon as the seats are taken) get a seat
//! right away when a game is waiting:
//!
//! 1. a game hosted from their own network first;
//! 2. otherwise the newest waiting game (a friend who just hosted, not one left waiting long ago);
//! 3. not one they left with Back: Back and Quick Match again goes on to the next game (and
//!    round again once they have seen them all).
//!
//! The "Players in Match" screen lists the host (by the name they play under) and the players
//! already seated in that game, so a player sees whose game they are in. Without a hosted game
//! players wait and are seated when one is hosted. With `match_size` > 0 (tests) players without a
//! hosted game also form `QM_n` rooms of that many, which need a separate game server.
//!
//! A host sends a player away (`kick`) for 10 minutes: none of that host's games (by the host's
//! name, so also their next one) is offered to the player's name, nor to their address unless it
//! is the host's own network (a stranger who comes back under another name stays away). The lists
//! change by themselves when the 10 minutes are over.
//!
//! Once a hosted game starts (its host says so, or an older host's game fills), the players it
//! started with are that match's players: the host's engine waits for all of them, so MATCHED_UP
//! announces them all. One who does not get there (Back or Quick Match during the lock, the
//! connection gone before entering the room) is remembered like a player whose connection dropped
//! in the match. A player whose connection drops in a running hosted match is remembered; their
//! next Quick Match request sends them straight back into it, for as long as the host serves that
//! match (no time limit by default, `rejoin_ms` sets one). While the host's link is coming back
//! (crate::relay::HOST_BACK_MS) the way back is kept but not taken, so the player waits for a
//! game as usual meanwhile; going back later frees the seat they took. Of two ways back (they
//! dropped out of another match meanwhile) the one they dropped out of last goes first, the other
//! one stays (that host's engine still holds their seat). A host that does not come back ends the
//! match and its ways back (the relay disconnects its players). Players matched into a hosted room
//! whose host is gone (it left during the lock) are disconnected instead: no one would start it.

use crate::relay::{ClientId, Net};
use crate::wire::*;
use std::collections::{HashMap, HashSet};
use serde_json::{json, Value};
use std::net::IpAddr;

const KEEPALIVE_MS: u64 = 10_000; // clients drop a peer that is silent for 20 s
const LOCK_TIMEOUT_MS: u64 = 3_000; // wait this long for LOCK_PLAYER_IN_GAME answers
/// How long a game a player left with Back is skipped for them.
const DECLINE_MS: u64 = 10 * 60_000;
/// Waiting-list ids of the hosts (players' own ids count up from 1).
const HOST_ID_BASE: u32 = 900_000_000;
/// The waiting list's player count is a u8.
const LIST_MAX: usize = 255;
/// Games offered to a player who chooses.
const GAMES_MAX: usize = 20;
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
    /// The host's connection, and whether the game starts only when they say so.
    owner: Option<ClientId>,
    approve: bool,
    /// The seated players' names as the host last heard them.
    told: Option<Vec<String>>,
}

/// A hosted game as the host's engine offers it (`serve`).
pub struct Offer<'a> {
    pub room: &'a str,
    /// Players it takes (not counting the host).
    pub players: usize,
    /// The host's name and hero (the engine's index; other values: random) for the lists.
    pub host: Option<&'a str>,
    pub hero: Option<i64>,
    pub addr: Option<IpAddr>,
    pub owner: Option<ClientId>,
    /// Starts when the host says so (`start`), not as soon as the seats are taken.
    pub approve: bool,
}

struct Match {
    group: Vec<Waiting>,
    room: String,
    /// The players it started with: MATCHED_UP's count (a host's engine waits for all of them).
    size: usize,
    /// A game hosted by a player (also a way back into one): its host's engine must still serve
    /// the room when the players are sent there (`QM_n` rooms are not served).
    hosted: bool,
    locked: HashSet<ClientId>,
    deadline: u64,
}

/// A host sent a player away: their name, their address (unless it is the host's own network),
/// when.
struct Kick {
    name: String,
    addr: Option<IpAddr>,
    at: u64,
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
    /// Hosted matches started (from the start on, so a player lost before entering the room can
    /// come back) or being played: room key -> room name.
    running: HashMap<String, String>,
    /// Room key -> (lower-case) names that lost the connection in it -> when.
    dropped: HashMap<String, HashMap<String, u64>>,
    /// Players sent into a hosted room (MATCHED_UP) who have not entered it yet: client -> room key.
    arriving: HashMap<ClientId, String>,
    /// Lower-case name -> the hosted games (room keys) they left with Back, and when they last did.
    declined: HashMap<String, (Vec<String>, u64)>,
    /// Lower-case host name -> the players that host sent away.
    kicked: HashMap<String, Vec<Kick>>,
    /// When the next kick or Back runs out: the lists change by themselves then.
    next_expiry: Option<u64>,
    /// The game list each choosing player was last sent.
    games_shown: HashMap<ClientId, String>,
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

/// Two addresses of one network: the same IPv4 address (a home behind one router), the same
/// IPv6 /64, or two local addresses (a LAN relay: the host's own game comes from 127.0.0.1, the
/// others from 192.168.x.y).
fn same_network(a: Option<IpAddr>, b: Option<IpAddr>) -> bool {
    let plain = |ip: IpAddr| match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
        v4 => v4,
    };
    // 100.64.0.0/10 (carrier-grade NAT) is where Tailscale puts a friend reaching a LAN relay
    let local = |ip: IpAddr| match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            v4.is_private() || v4.is_loopback() || v4.is_link_local() || (o[0] == 100 && o[1] & 0xc0 == 64)
        }
        IpAddr::V6(v6) => v6.is_loopback() || v6.segments()[0] & 0xfe00 == 0xfc00 || v6.segments()[0] & 0xffc0 == 0xfe80,
    };
    match (a.map(plain), b.map(plain)) {
        (Some(x), Some(y)) if local(x) && local(y) => true,
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

/// Kicks are the host's (by their name, so they hold for the host's next game too); a host
/// without a usable name has them for this game only.
fn kick_key(h: &Hosted) -> String {
    if h.host.is_empty() { h.key.clone() } else { h.host.to_lowercase() }
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
        self.left_launch(net, c);
        let waited = self.withdraw(c).is_some();
        self.players.remove(&c);
        if waited {
            self.matchmake(net);
        }
    }

    /// A player of a started hosted game who will not get into its room from here (Back or Quick
    /// Match during the lock, gone from the casino before entering the room): the host's engine
    /// keeps their place, so they are remembered like a player whose connection dropped in the
    /// match and are not sent there now. The others go on without waiting for their lock answer.
    fn left_launch(&mut self, net: &mut Net, c: ClientId) -> bool {
        let name = net.name(c).to_string();
        if let Some(i) = self.matches.iter().position(|m| m.hosted && m.group.iter().any(|q| q.client == c)) {
            let m = &mut self.matches[i];
            m.group.retain(|q| q.client != c);
            m.locked.remove(&c);
            let key = m.room.to_lowercase();
            self.dropped_out(net, &key, &name);
            let m = &self.matches[i];
            if m.group.is_empty() {
                self.matches.remove(i);
            } else if m.group.iter().all(|q| m.locked.contains(&q.client)) {
                self.matched_up(net, i);
            }
            return true;
        }
        if let Some(key) = self.arriving.remove(&c) {
            // on the way into its room (the engine leaves the casino first): entering it forgets
            // this, so it is not logged as a drop
            if self.running.contains_key(&key) {
                self.dropped.entry(key).or_default().insert(name.to_lowercase(), net.now);
            }
            return true;
        }
        false
    }

    /// A player is on their way into this room (it takes them from the casino): no longer arriving.
    pub fn entering(&mut self, net: &Net, c: ClientId, key: &str) {
        if self.arriving.get(&c).is_some_and(|k| k == key) {
            self.arriving.remove(&c);
        }
        if let Some(m) = self.dropped.get_mut(key) {
            m.remove(&net.name(c).to_lowercase());
        }
    }

    /// Is this player in a started hosted game they have not reached yet?
    pub fn launched(&self, c: ClientId) -> bool {
        self.arriving.contains_key(&c) || self.matches.iter().any(|m| m.hosted && m.group.iter().any(|q| q.client == c))
    }

    /// Is this room's game offered (waiting for its players)?
    pub fn is_hosted(&self, key: &str) -> bool {
        self.hosted.iter().any(|h| h.key == key)
    }

    /// Has a match been started in this room (and its host not finished it)?
    pub fn is_running(&self, key: &str) -> bool {
        self.running.contains_key(key)
    }

    /// The host's game came back on a new link (`serve` with its key): that link owns the game
    /// now and hears its seats again.
    pub fn host_back(&mut self, net: &mut Net, key: &str, owner: ClientId) {
        if let Some(h) = self.hosted.iter_mut().find(|h| h.key == key) {
            h.owner = Some(owner);
            h.told = None;
        }
        self.send_back(net, key);
        self.refresh(net);
    }

    /// A running match the relay did not know (it restarted, or its host came back after its
    /// players were sent away): the players whose seats its host holds go back in with Quick Match.
    pub fn adopt(&mut self, net: &mut Net, key: &str, room: &str, held: &[&str]) {
        net.log(&format!("casino: {room} is a running match (held: {})", held.join(", ")));
        self.running.insert(key.to_string(), room.to_string());
        let d = self.dropped.entry(key.to_string()).or_default();
        for n in held {
            d.insert(n.to_lowercase(), net.now);
        }
        self.send_back(net, key);
        self.refresh(net);
    }

    /// Players who may go back into this match and wait in Quick Match (they came back while its
    /// host was away; their game asks only once): into the match now.
    fn send_back(&mut self, net: &mut Net, key: &str) {
        let Some(names) = self.dropped.get(key) else { return };
        let back: Vec<Waiting> = self
            .queue
            .iter()
            .chain(self.hosted.iter().flat_map(|h| h.seated.iter()))
            .filter(|q| names.contains_key(&net.name(q.client).to_lowercase()))
            .copied()
            .collect();
        if back.is_empty() {
            return;
        }
        for q in back {
            self.withdraw(q.client);
            let name = net.name(q.client).to_string();
            if let Some(room) = self.rejoin_room(net, &name) {
                net.log(&format!("casino: {name} rejoins {room}"));
                self.start_match(net, vec![q], room, true);
            }
        }
        self.matchmake(net);
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
    pub fn add_hosted(&mut self, net: &mut Net, o: Offer) {
        let key = o.room.to_lowercase();
        self.remove_hosted(&key);
        self.next_list_id += 1;
        let host = o.host.map_or_else(|| host_from_room(o.room), str::to_string);
        self.hosted.push(Hosted {
            room: o.room.to_string(),
            key,
            players: o.players,
            host: host.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_').collect(),
            hero: o.hero.and_then(|h| usize::try_from(h).ok()).and_then(|h| HEROES.get(h).copied()),
            addr: o.addr,
            list_id: HOST_ID_BASE + self.next_list_id,
            seated: Vec::new(),
            owner: o.owner,
            approve: o.approve,
            told: None,
        });
        let when = if o.approve { "when the host says so" } else { "when full" };
        net.log(&format!("casino: hosted game {} for {} player(s), starts {when}", o.room, o.players));
        self.matchmake(net);
    }

    /// A choosing player picks a waiting game (by its list id). Only games that choose may: they
    /// were offered the games (and others could make every chooser's list change at will).
    pub fn choose(&mut self, net: &mut Net, c: ClientId, id: u32) {
        if !net.picks(c) {
            return;
        }
        let Some(j) = self.hosted.iter().position(|h| h.list_id == id) else {
            return self.resend_games(net, c); // gone meanwhile
        };
        let h = &self.hosted[j];
        if h.seated.iter().any(|q| q.client == c) {
            return;
        }
        if h.seated.len() >= h.players || self.was_kicked(net, c, h) {
            return self.resend_games(net, c);
        }
        let q = if let Some(i) = self.queue.iter().position(|q| q.client == c) {
            self.queue.remove(i)
        } else if let Some(o) = self.hosted.iter_mut().find(|o| o.seated.iter().any(|q| q.client == c)) {
            let i = o.seated.iter().position(|q| q.client == c).unwrap();
            o.seated.remove(i)
        } else {
            return; // not in Quick Match
        };
        net.log(&format!("casino: {} picks {}", net.name(c), self.hosted[j].room));
        self.hosted[j].seated.push(q);
        self.matchmake(net);
    }

    fn resend_games(&mut self, net: &mut Net, c: ClientId) {
        self.games_shown.remove(&c);
        self.refresh(net);
    }

    /// The host starts their game (`start`): only when every seat is taken.
    pub fn host_start(&mut self, net: &mut Net, owner: ClientId) {
        let Some(i) = self.hosted.iter().position(|h| h.owner == Some(owner)) else { return };
        if self.hosted[i].seated.len() >= self.hosted[i].players {
            net.log(&format!("casino: the host starts {}", self.hosted[i].room));
            self.launch(net, i);
        } else {
            self.hosted[i].told = None; // someone left meanwhile: the host hears who is there
        }
        self.refresh(net);
    }

    /// The host sends a seated player away (`kick`): none of this host's games is offered to them
    /// for DECLINE_MS. The player's address counts too (a stranger comes back under another name
    /// at will), unless it is the host's own network (a LAN relay, the host's family).
    pub fn host_kick(&mut self, net: &mut Net, owner: ClientId, name: &str) {
        let Some(h) = self.hosted.iter_mut().find(|h| h.owner == Some(owner)) else { return };
        let Some(i) = h.seated.iter().position(|q| net.name(q.client).eq_ignore_ascii_case(name)) else {
            h.told = None;
            return self.refresh(net);
        };
        let q = h.seated.remove(i);
        net.log(&format!("casino: the host of {} sends {} away", h.room, net.name(q.client)));
        let addr = net.addr(q.client).filter(|&a| !same_network(h.addr, Some(a)));
        let kick = Kick { name: net.name(q.client).to_lowercase(), addr, at: net.now };
        self.kicked.entry(kick_key(h)).or_default().push(kick);
        self.queue.insert(0, q);
        self.matchmake(net);
    }

    /// Has this game's host sent the player away in the last DECLINE_MS?
    fn was_kicked(&self, net: &Net, c: ClientId, h: &Hosted) -> bool {
        let (name, addr) = (net.name(c), net.addr(c));
        self.kicked.get(&kick_key(h)).is_some_and(|ks| {
            ks.iter().any(|k| {
                net.now < k.at + DECLINE_MS && (k.name.eq_ignore_ascii_case(name) || (k.addr.is_some() && same_network(k.addr, addr)))
            })
        })
    }

    /// Starts a hosted game with the players seated in it. It counts as running from now on: a
    /// player lost before entering its room may come back into it.
    fn launch(&mut self, net: &mut Net, i: usize) {
        let h = self.hosted.remove(i);
        for q in &h.seated {
            self.shown.remove(&q.client);
            self.declined.remove(&net.name(q.client).to_lowercase());
        }
        if net.is_served(&h.room) {
            self.running.insert(h.key.clone(), h.room.clone());
        }
        self.start_match(net, h.seated, h.room, true);
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
        self.arriving.retain(|_, k| k != key);
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

    /// The running hosted match this player dropped out of, if they may still go back. While its
    /// host's link is coming back (the room unserved for a few seconds) the way back is kept, so
    /// a player may have two (they played another match meanwhile): the one they dropped out of
    /// last.
    fn rejoin_room(&mut self, net: &Net, name: &str) -> Option<String> {
        let name = name.to_lowercase();
        let mut last: Option<(u64, String)> = None;
        for (key, names) in self.dropped.iter_mut() {
            let Some(&t) = names.get(&name) else { continue };
            let over = !self.running.contains_key(key);
            if over || self.rejoin_ms.is_some_and(|ms| net.now.saturating_sub(t) > ms) {
                names.remove(&name);
                continue;
            }
            if net.is_served(&self.running[key]) && last.as_ref().is_none_or(|(lt, lk)| (t, key) > (*lt, lk)) {
                last = Some((t, key.clone()));
            }
        }
        let (_, key) = last?;
        self.dropped.get_mut(&key)?.remove(&name);
        self.running.get(&key).cloned()
    }

    /// Has the player left this hosted game with Back (lately)?
    fn declined(&self, net: &Net, c: ClientId, key: &str) -> bool {
        self.declined
            .get(&net.name(c).to_lowercase())
            .is_some_and(|(keys, t)| net.now < t + DECLINE_MS && keys.iter().any(|k| k == key))
    }

    /// The hosted game a waiting player gets: their own network's, else the newest; not one they
    /// left with Back while there is another.
    fn pick(&mut self, net: &Net, c: ClientId) -> Option<usize> {
        let free: Vec<usize> = (0..self.hosted.len()).rev().filter(|&i| self.hosted[i].seated.len() < self.hosted[i].players).collect();
        if free.is_empty() {
            return None;
        }
        let free: Vec<usize> = free.into_iter().filter(|&i| !self.was_kicked(net, c, &self.hosted[i])).collect();
        let mut open: Vec<usize> = free.iter().copied().filter(|&i| !self.declined(net, c, &self.hosted[i].key)).collect();
        if open.is_empty() {
            // seen them all: round again
            self.declined.remove(&net.name(c).to_lowercase());
            open = free;
        }
        let addr = net.addr(c);
        open.iter().copied().find(|&i| same_network(self.hosted[i].addr, addr)).or(open.first().copied())
    }

    /// Seats waiting players: anyone in a game from their own network (also a player of an
    /// older game seated elsewhere; one who chooses only while they have not), then the older
    /// games' players in the game `pick` gives them. Players who choose pick a game themselves.
    fn place(&mut self, net: &Net) {
        let now = net.now;
        self.declined.retain(|_, (_, t)| now < *t + DECLINE_MS);
        for k in self.kicked.values_mut() {
            k.retain(|k| now < k.at + DECLINE_MS);
        }
        self.kicked.retain(|_, k| !k.is_empty());
        let kicks = self.kicked.values().flatten().map(|k| k.at);
        self.next_expiry = kicks.chain(self.declined.values().map(|(_, t)| *t)).min().map(|t| t + DECLINE_MS);
        // until nothing moves: a player going home frees a seat that may be someone else's home
        // (each player moves home at most once)
        let mut moved = true;
        while moved {
            moved = false;
            let waiting: Vec<(Waiting, Option<usize>)> = self
                .queue
                .iter()
                .map(|&q| (q, None))
                .chain(self.hosted.iter().enumerate().flat_map(|(i, h)| h.seated.iter().map(move |&q| (q, Some(i)))))
                .collect();
            for (q, at) in waiting {
                let addr = net.addr(q.client);
                if at.is_some_and(|i| same_network(self.hosted[i].addr, addr)) || (at.is_some() && net.picks(q.client)) {
                    continue;
                }
                let home = (0..self.hosted.len()).rev().find(|&i| {
                    let h = &self.hosted[i];
                    h.seated.len() < h.players
                        && same_network(h.addr, addr)
                        && !self.declined(net, q.client, &h.key)
                        && !self.was_kicked(net, q.client, h)
                });
                if let Some(j) = home {
                    match at {
                        Some(i) => self.hosted[i].seated.retain(|o| o.client != q.client),
                        None => self.queue.retain(|o| o.client != q.client),
                    }
                    self.hosted[j].seated.push(q);
                    moved = true;
                }
            }
        }
        let mut i = 0;
        while i < self.queue.len() {
            if net.picks(self.queue[i].client) {
                i += 1;
                continue;
            }
            match self.pick(net, self.queue[i].client) {
                Some(j) => {
                    let q = self.queue.remove(i);
                    self.hosted[j].seated.push(q);
                }
                None => i += 1,
            }
        }
    }

    /// Seats waiting players, starts the games whose seats are all taken (unless their host
    /// starts them) and updates the lists. Without a hosted game, `QM_n` rooms of `match_size`
    /// players (if enabled).
    fn matchmake(&mut self, net: &mut Net) {
        self.place(net);
        self.refresh(net); // also those whose game starts now see whose it is
        while let Some(i) = self.hosted.iter().position(|h| !h.approve && h.seated.len() >= h.players) {
            self.launch(net, i);
        }
        if self.hosted.is_empty() {
            while self.match_size > 0 && self.queue.len() >= self.match_size {
                let group: Vec<Waiting> = self.queue.drain(..self.match_size).collect();
                for q in &group {
                    self.shown.remove(&q.client);
                }
                let room = format!("QM_{}", self.next_table);
                self.next_table += 1;
                self.start_match(net, group, room, false);
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

    /// The games a choosing player is offered: `[list id, host, hero index or -1, seated,
    /// seats, same network (0/1), the host starts it (0/1)]`, their own network's first, then the
    /// newest, and always the one they sit in (the client finds its seat among these); and the list
    /// id of the game they sit in (0: none).
    fn games_for(&self, net: &Net, c: ClientId) -> (Vec<Value>, u32) {
        let addr = net.addr(c);
        let seat = self.hosted.iter().find(|h| h.seated.iter().any(|q| q.client == c)).map_or(0, |h| h.list_id);
        let mut games: Vec<&Hosted> = self
            .hosted
            .iter()
            .rev()
            .filter(|h| h.list_id == seat || (h.seated.len() < h.players && !self.was_kicked(net, c, h)))
            .collect();
        games.sort_by_key(|h| !same_network(h.addr, addr)); // stable: newest first within
        if let Some(p) = games.iter().position(|h| h.list_id == seat).filter(|&p| p >= GAMES_MAX) {
            let own = games.remove(p);
            games.truncate(GAMES_MAX - 1);
            games.push(own); // it sorts after all the others shown
        }
        let games = games
            .into_iter()
            .take(GAMES_MAX)
            .map(|h| {
                let hero = h.hero.and_then(|n| HEROES.iter().position(|&x| x == n)).map_or(-1, |i| i as i64);
                json!([h.list_id, h.host, hero, h.seated.len(), h.players, same_network(h.addr, addr) as u8, h.approve as u8])
            })
            .collect();
        (games, seat)
    }

    /// Brings every waiting player's list up to date: EVENT CASINO_CANCEL_MATCHMAKING (10,
    /// `[string id]`) removes an entry, EVENT CASINO_MATCHMAKING (9, `[string id][profile]`) adds
    /// one. Players who choose also get the games (`games`, `on: false` once they no longer
    /// wait), hosts who start their game the players seated in it (`seats`).
    fn refresh(&mut self, net: &Net) {
        let waiting: Vec<Waiting> = self.queue.iter().chain(self.hosted.iter().flat_map(|h| h.seated.iter())).copied().collect();
        for q in waiting.iter().filter(|q| net.picks(q.client)) {
            let (games, seat) = self.games_for(net, q.client);
            let msg = json!({"t": "games", "on": true, "seat": seat, "games": games});
            let text = msg.to_string();
            if self.games_shown.get(&q.client) != Some(&text) {
                net.ctl(q.client, msg);
                self.games_shown.insert(q.client, text);
            }
        }
        let gone: Vec<ClientId> = self.games_shown.keys().copied().filter(|c| !waiting.iter().any(|q| q.client == *c)).collect();
        for c in gone {
            self.games_shown.remove(&c);
            net.ctl(c, json!({"t": "games", "on": false}));
        }
        for h in self.hosted.iter_mut().filter(|h| h.approve) {
            let names: Vec<String> = h.seated.iter().map(|q| net.name(q.client).to_string()).collect();
            if h.told.as_ref() != Some(&names) {
                if let Some(o) = h.owner {
                    net.ctl(o, json!({"t": "seats", "room": h.room, "names": names, "of": h.players}));
                }
                h.told = Some(names);
            }
        }
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
    ///
    /// `hosted`: a hosted game's room (its host's engine serves it).
    fn start_match(&mut self, net: &Net, group: Vec<Waiting>, room: String, hosted: bool) {
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
        let size = group.len();
        self.matches.push(Match { group, room, size, hosted, locked: HashSet::new(), deadline: net.now + LOCK_TIMEOUT_MS });
    }

    /// A player acknowledged LOCK_PLAYER_IN_GAME.
    fn locked(&mut self, net: &mut Net, c: ClientId) {
        let Some(i) = self.matches.iter().rposition(|m| m.group.iter().any(|q| q.client == c)) else {
            return;
        };
        let m = &mut self.matches[i];
        m.locked.insert(c);
        if m.group.iter().all(|q| m.locked.contains(&q.client)) {
            self.matched_up(net, i);
        }
    }

    /// Tells each player of a locked group the game room to move to. A hosted room whose host
    /// left meanwhile would never start: its players are disconnected instead (their game then
    /// says so), and remembered in case the host's link comes back.
    fn matched_up(&mut self, net: &mut Net, i: usize) {
        let m = self.matches.remove(i);
        let group: Vec<Waiting> = m.group.into_iter().filter(|q| self.players.contains_key(&q.client)).collect();
        let room = m.room;
        let key = room.to_lowercase();
        if m.hosted && !net.is_served(&room) {
            net.log(&format!("casino: the host of {room} is gone: its players are sent away"));
            for q in &group {
                let name = net.name(q.client).to_string();
                self.dropped_out(net, &key, &name);
                net.close_later(q.client, 4003, "host gone");
            }
            return;
        }
        net.expect_room(&room, m.size);
        if net.is_served(&room) {
            self.running.insert(key.clone(), room.clone());
        }
        for (i, q) in group.iter().enumerate() {
            if m.hosted {
                self.arriving.insert(q.client, key.clone());
            }
            let b = format!("naf://rg.com:80/{room}?hosted={}", if i == 0 { "yes" } else { "no" });
            reply(net, q.client, EVENT, 31, q.ch, |w| {
                // casino name, table: [string room][string address][u8][u8][u32 players][settings]
                // settings: [u8 kind=0][u8][string address][u8]
                w.str(MATCHMAKER_NAME).str(&room).str(&b).u8(0).u8(1).u32(m.size as u32);
                w.u8(0).u8(0).str(&b).u8(0);
            });
        }
    }

    pub fn tick(&mut self, net: &mut Net) {
        while let Some(i) = self.matches.iter().position(|m| net.now >= m.deadline) {
            self.matched_up(net, i);
        }
        if self.next_expiry.is_some_and(|t| net.now >= t) {
            self.matchmake(net); // a kick or Back ran out: that game is offered (seated) again
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
                self.left_launch(net, c); // asked again before getting there: the way back below
                let name = net.name(c).to_string();
                if let Some(back) = self.rejoin_room(net, &name) {
                    respond(net, c, 9, 0, ch, |w| {
                        w.u8(0);
                    });
                    net.log(&format!("casino: {name} rejoins {back}"));
                    self.start_match(net, vec![Waiting { client: c, ch }], back, true);
                    // the seat they leave (they waited there while their match's host was away)
                    // may be someone else's
                    self.matchmake(net);
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
                // the player gets another game, if there is one. Back that crosses the host's
                // start comes too late: that match is theirs (not sent there now, a way back).
                self.left_launch(net, c);
                if let Some(key) = self.withdraw(c).filter(|k| !k.is_empty() && !net.picks(c)) {
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
        assert!(same_network(ip("127.0.0.1"), ip("192.168.0.59")));
        assert!(same_network(ip("10.0.0.2"), ip("fe80::1")));
        assert!(!same_network(ip("127.0.0.1"), ip("81.2.3.4")));
        // Tailscale (100.64.0.0/10) on a LAN relay
        assert!(same_network(ip("127.0.0.1"), ip("100.101.102.103")));
        assert!(same_network(ip("100.64.0.1"), ip("100.127.255.254")));
        assert!(!same_network(ip("127.0.0.1"), ip("100.128.0.1")));
        assert!(!same_network(ip("127.0.0.1"), ip("100.63.255.255")));
    }

    #[test]
    fn host_names_from_rooms() {
        assert_eq!(host_from_room("HOST_Peter_1"), "Peter");
        assert_eq!(host_from_room("HOST_my_name_12"), "my_name");
        assert_eq!(host_from_room("HOST_X"), "HOST_X");
        assert_eq!(host_from_room("QM_3"), "QM_3");
    }
}

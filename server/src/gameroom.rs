//! Game-room server: the per-room "game server" of the original N-Gage Arena, rebuilt from the
//! engine's table client (docs/MULTIPLAYER.md, "The game room").
//!
//! Every matchmade room gets one. Like the casino bot it is an MT peer to each player: it opens
//! with the transport hello and keep-alive, answers pings, and serves the seating protocol on
//! channel 0x15 (same message framing as the casino: `[u8 type][u8 action]` + fields):
//!
//! * REQUEST  SEAT_RESERVE / SEAT_TAKE / SEAT_LEAVE  `u32` player id
//! * RESPONSE generic: `[u8 result (0 = ok)][u32 value]`
//! * EVENT    SEAT_RESERVE / SEAT_TAKE / SEAT_LEAVE  string player id (decimal); the client looks
//!   the id up among the players its casino knows (EVENT CASINO_ENTER), so the casino introduces
//!   matched players to each other before CASINO_MATCHED_UP.
//!
//! Channel 0x16 (the match) goes to the room's game server (a host player's engine) tagged with
//! the player's session id; frames on other channels are forwarded to the other players.

use crate::relay::{ClientId, Net};
use crate::wire::*;
use serde_json::json;

pub const SEAT_RESERVE: u8 = 12;
pub const SEAT_TAKE: u8 = 13;
pub const SEAT_LEAVE: u8 = 14;
const KEEPALIVE_MS: u64 = 10_000;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Seat {
    Reserved,
    Taken,
}

struct Player {
    id: ClientId,
    greeted: bool,
    pid: Option<String>,
    seat: Option<Seat>,
    sid: u32,
    frag: MtState,
    announced: bool,
}

pub struct GameRoom {
    pub name: String,
    expected: usize,
    players: Vec<Player>,
    started: bool,
    served: bool,
    server: Option<ClientId>,
    next_sid: u32,
    ping_seq: u8,
    next_keepalive: Option<u64>,
}

/// The transport hello (ch 2: 00) and the keep-alive timeout (ch 1: u32 20000 ms).
pub fn greeting() -> Vec<u8> {
    let mut g = frame(CH_CONTROL, &[0]);
    g.extend(frame(CH_SYSTEM, &20000u32.to_le_bytes()));
    g
}

impl GameRoom {
    /// `expected` is the number of matched players: the room stays silent until they are all
    /// present (or `start` is called), because the engine's handshake is lost on players who have
    /// not joined yet.
    pub fn new(name: &str, expected: usize) -> Self {
        GameRoom {
            name: name.to_string(),
            expected,
            players: Vec::new(),
            started: false,
            served: false,
            server: None,
            next_sid: 1,
            ping_seq: 1,
            next_keepalive: None,
        }
    }

    fn index(&self, c: ClientId) -> Option<usize> {
        self.players.iter().position(|p| p.id == c)
    }

    pub fn join(&mut self, net: &Net, c: ClientId) {
        self.players.push(Player {
            id: c,
            greeted: false,
            pid: None,
            seat: None,
            sid: self.next_sid,
            frag: MtState::default(),
            announced: false,
        });
        self.next_sid += 1;
        if self.started || self.players.len() >= self.expected {
            self.start(net);
        }
    }

    /// Greets everyone not yet greeted.
    pub fn start(&mut self, net: &Net) {
        self.started = true;
        for p in self.players.iter_mut().filter(|p| !p.greeted) {
            p.greeted = true;
            net.send(p.id, greeting());
        }
        if self.next_keepalive.is_none() {
            self.next_keepalive = Some(net.now + KEEPALIVE_MS);
        }
    }

    pub fn tick(&mut self, net: &Net) {
        if let Some(t) = self.next_keepalive {
            if net.now >= t {
                self.ping_seq = self.ping_seq % 255 + 1;
                for p in self.players.iter().filter(|p| p.greeted) {
                    net.send(p.id, frame(CH_SYSTEM, &[self.ping_seq]));
                }
                self.next_keepalive = Some(net.now + KEEPALIVE_MS);
            }
        }
    }

    /// `lost`: the connection dropped (not a normal leave); the host holds the seat.
    pub fn leave(&mut self, net: &Net, c: ClientId, lost: bool) {
        if let Some(i) = self.index(c) {
            let p = self.players.remove(i);
            if p.announced {
                if let Some(s) = self.server {
                    let m = if lost {
                        json!({"t": "player_left", "pid": p.sid, "lost": 1})
                    } else {
                        json!({"t": "player_left", "pid": p.sid})
                    };
                    net.ctl(s, m);
                }
            }
            if p.seat.is_some() {
                self.seat_left(net, p.pid.as_deref());
            }
        }
        if self.players.is_empty() {
            self.close();
        }
    }

    pub fn close(&mut self) {
        self.next_keepalive = None;
    }

    pub fn packet(&mut self, net: &Net, c: ClientId, bytes: &[u8]) {
        let Some(i) = self.index(c) else { return };
        let msgs = mt_messages(&mut self.players[i].frag, bytes);
        let mut forward = Vec::new();
        for (ch, data) in msgs {
            if ch == CH_GAME && self.server.is_some() {
                self.to_server(net, i, &data);
            } else if ch == CH_TABLE {
                self.table_message(net, i, &data);
            } else if ch == CH_SYSTEM && data.len() == 1 {
                net.send(c, frame(CH_CONTROL, &data)); // pong
            } else if ch == CH_CONTROL && data.len() == 1 && data[0] == 0 {
                net.send(c, frame(CH_CONTROL, &[0x20])); // accept the connection
            } else if ch == CH_CONTROL || ch == CH_SYSTEM {
                // pong / keep-alive setting
            } else if ch == CH_COMMAND {
                self.command(net, i, &data);
            } else {
                forward.push((ch, data));
            }
        }
        for (ch, data) in forward {
            net.log(&format!("room {}: {} -> others ch {} {} bytes", self.name, net.name(c), ch, data.len()));
            for o in self.players.iter().filter(|o| o.id != c) {
                for pkt in mt_packets(ch, &data) {
                    net.send(o.id, pkt);
                }
            }
        }
    }

    /// The game server process serves this room.
    pub fn attach_server(&mut self, net: &Net, server: ClientId) {
        self.served = true;
        self.server = Some(server);
        for i in 0..self.players.len() {
            self.announce(net, i);
        }
    }

    pub fn detach_server(&mut self) {
        self.server = None;
    }

    /// Tells the game server about a player once it has taken its seat (its session exists).
    fn announce(&mut self, net: &Net, i: usize) {
        let Some(s) = self.server else { return };
        let p = &mut self.players[i];
        if p.announced || p.seat != Some(Seat::Taken) {
            return;
        }
        p.announced = true;
        net.ctl(s, json!({"t": "player_joined", "pid": p.sid, "name": net.name(p.id)}));
    }

    /// Player -> server: `[u16 session id][message]`.
    fn to_server(&self, net: &Net, i: usize, data: &[u8]) {
        let Some(s) = self.server else { return };
        let sid = self.players[i].sid as u16;
        let mut out = Vec::with_capacity(data.len() + 2);
        out.extend_from_slice(&sid.to_le_bytes());
        out.extend_from_slice(data);
        net.send(s, out);
    }

    /// Server -> player: `[u16 session id][message]`, split into MT packets.
    pub fn from_server(&self, net: &Net, bytes: &[u8]) {
        if bytes.len() < 2 {
            return;
        }
        let sid = u16::from_le_bytes([bytes[0], bytes[1]]);
        if let Some(p) = self.players.iter().find(|p| p.sid as u16 == sid) {
            for pkt in mt_packets(CH_GAME, &bytes[2..]) {
                net.send(p.id, pkt);
            }
        }
    }

    /// Channel-3 session hello: `[u8 n]` n × (string key, string value), then typed extras.
    fn command(&mut self, net: &Net, i: usize, data: &[u8]) {
        let mut r = Reader::new(data);
        let parsed = (|| -> Res<Vec<(String, String)>> {
            let mut kv = Vec::new();
            for _ in 0..r.u8()? {
                let k = r.str()?;
                let v = r.str()?;
                kv.push((k, v));
            }
            Ok(kv)
        })();
        let name = net.name(self.players[i].id);
        match parsed {
            Ok(kv) => {
                if let Some((_, pid)) = kv.iter().find(|(k, _)| k == "pid") {
                    self.players[i].pid.get_or_insert_with(|| pid.clone());
                }
                net.log(&format!("room {}: {} hello {:?}", self.name, name, kv));
            }
            Err(_) => net.log(&format!("room {}: {} command {}", self.name, name, hex(data))),
        }
    }

    fn table_message(&mut self, net: &Net, i: usize, data: &[u8]) {
        let c = self.players[i].id;
        let mut r = Reader::new(data);
        let (Ok(t), Ok(action)) = (r.u8(), r.u8()) else {
            net.log(&format!("room {}: {} short table message {}", self.name, net.name(c), hex(data)));
            return;
        };
        net.log(&format!(
            "room {}: {} {} {} {}",
            self.name,
            net.name(c),
            type_name(t),
            action_name(action),
            hex(r.rest())
        ));
        if t != REQUEST {
            return;
        }
        let pid = match r.u32() {
            Ok(v) => Some(v.to_string()),
            Err(_) => self.players[i].pid.clone(),
        };
        self.players[i].pid = pid.clone();
        let value = pid.as_deref().and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
        match action {
            SEAT_RESERVE | SEAT_TAKE => {
                let first = self.players[i].seat.is_none();
                let seat = if action == SEAT_TAKE { Seat::Taken } else { self.players[i].seat.unwrap_or(Seat::Reserved) };
                self.players[i].seat = Some(seat);
                respond(net, c, action, 0, value);
                self.announce(net, i);
                // the newcomer learns about everyone already seated, then everyone learns about it
                if first {
                    for o in self.players.iter().filter(|o| o.id != c) {
                        if let Some(s) = o.seat {
                            let a = if s == Seat::Taken { SEAT_TAKE } else { SEAT_RESERVE };
                            send_event(net, c, a, o.pid.as_deref());
                        }
                    }
                }
                self.event(net, action, pid.as_deref());
            }
            SEAT_LEAVE => {
                self.players[i].seat = None;
                respond(net, c, action, 0, value);
                self.seat_left(net, pid.as_deref());
            }
            _ => respond(net, c, action, 0, 0),
        }
    }

    /// A player left its seat. With a game server the match reports leavers itself; an EVENT
    /// SEAT_LEAVE reaching a client that is still on its win screen leaves it on a black screen,
    /// so the event is only sent while no game server serves the room.
    fn seat_left(&self, net: &Net, pid: Option<&str>) {
        if !self.served {
            self.event(net, SEAT_LEAVE, pid);
        }
    }

    /// Tells every greeted player about a seat change.
    fn event(&self, net: &Net, action: u8, pid: Option<&str>) {
        for p in self.players.iter().filter(|p| p.greeted) {
            send_event(net, p.id, action, pid);
        }
    }
}

fn respond(net: &Net, c: ClientId, action: u8, result: u8, value: u32) {
    net.send(c, frame(CH_TABLE, &Writer::new().u8(RESPONSE).u8(action).u8(result).u32(value).done()));
}

fn send_event(net: &Net, c: ClientId, action: u8, pid: Option<&str>) {
    net.send(c, frame(CH_TABLE, &Writer::new().u8(EVENT).u8(action).str(pid.unwrap_or("0")).done()));
}

//! The engine's wire formats, as far as the relay needs them (docs/MULTIPLAYER.md).
//!
//! * A relay packet in the casino holds frames `[u16 length of the rest][u8 channel][payload]`.
//! * Game rooms use MT packet framing: `[u16 header][bytes]`, header = length | piece << MT_SHIFT;
//!   the pieces of a split message `[u8 channel][payload]` are numbered 1, 2, … and the last one
//!   is 0 (a message that fits one packet has piece 0, which is the frame format above).
//! * Strings are the engine's string object `[u32 refcount|flags][u32 length][u32 capacity][bytes]`
//!   (flags bits 28..30 = 2: UTF-16).
//!
//! All integers are little-endian.

pub const CH_SYSTEM: u8 = 1; // keep-alive: u32 timeout on join, then 1-byte sequence pings
pub const CH_CONTROL: u8 = 2; // transport control: 00 hello, 20 accept, 1-byte ping replies
pub const CH_COMMAND: u8 = 3; // key/value "command" (session hello on room join)
pub const CH_CASINO: u8 = 0x11; // the casino system
pub const CH_MATCHMAKER: u8 = 0x12; // a matchmaker casino (kind 1)
pub const CH_FRIENDLY: u8 = 0x13; // a friendly casino (kind 2)
pub const CH_TABLE: u8 = 0x15; // seating in a game room
pub const CH_GAME: u8 = 0x16; // the match: client <-> authoritative game server

pub const REQUEST: u8 = 0;
pub const RESPONSE: u8 = 1;
pub const EVENT: u8 = 2;

/// MT framing: the fragmenter cap is MTU 876 - 2 = 874 (initNative), so 1 << 10 >= cap.
pub const MT_SHIFT: u32 = 10;
pub const MT_LEN_MASK: u16 = (1 << MT_SHIFT) - 1;
pub const MT_PIECE: usize = 874;

/// Casino / table actions (the engine's message classes).
pub fn action_name(a: u8) -> &'static str {
    const NAMES: [&str; 43] = [
        "CASINOSYSTEM_LOGIN", "CASINOSYSTEM_LOGOUT", "CASINOSYSTEM_UPDATE", "CASINOSYSTEM_F_CASINO",
        "GET_RANKINGS", "GET_STATISTICS", "GET_ACHIEVEMENTS", "GET_FRIENDLY_CASINO_LIST",
        "CASINO_ENTER", "CASINO_MATCHMAKING", "CASINO_CANCEL_MATCHMAKING", "CASINO_EXIT",
        "SEAT_RESERVE", "SEAT_TAKE", "SEAT_LEAVE", "SEAT_RESERVATION_EXPIRED",
        "SEATING_FAILED", "GAME_START_FAILED", "TABLE_CLOSED", "TABLE_TERMINATED",
        "CASINO_CREATE", "FRIENDLY_CASINO_ENTERED", "FRIENDLY_CASINO_LEFT", "FRIENDLY_CASINO_PLAYING",
        "FRIENDLY_CASINO_UNAVAIL", "FRIENDLY_UPDATE", "CASINO_SHUTDOWN", "CASINO_ACTIVATE",
        "FRIENDLY_CASINO_START", "LOCK_PLAYER_IN_GAME", "UNLOCK_PLAYER_FROM_GAME", "CASINO_MATCHED_UP",
        "ACTION_32", "REGISTER_FRIENDS", "VERSION_NOT_SUPPORTED", "FRIEND_REMOVE", "FRIEND_ADD",
        "CASINO_NOT_FOUND", "CASINO_CHECK_STATE", "PRESENCE_SUBSCRIBE", "PRESENCE_UNSUBSCRIBE",
        "PRESENCE_ACCEPT", "PRESENCE_DECLINE",
    ];
    NAMES.get(a as usize).copied().unwrap_or("ACTION_?")
}

pub fn type_name(t: u8) -> &'static str {
    match t {
        REQUEST => "REQUEST",
        RESPONSE => "RESPONSE",
        EVENT => "EVENT",
        _ => "TYPE_?",
    }
}

/// A truncated or malformed message (the sender is buggy or hostile: log and ignore it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Short;

pub type Res<T> = Result<T, Short>;

/// Little-endian reader over a byte slice; every read is bounds-checked.
pub struct Reader<'a> {
    b: &'a [u8],
    pub p: usize,
}

impl<'a> Reader<'a> {
    pub fn new(b: &'a [u8]) -> Self {
        Reader { b, p: 0 }
    }
    pub fn left(&self) -> usize {
        self.b.len() - self.p
    }
    pub fn rest(&self) -> &'a [u8] {
        &self.b[self.p..]
    }
    pub fn take(&mut self, n: usize) -> Res<&'a [u8]> {
        if n > self.left() {
            return Err(Short);
        }
        let s = &self.b[self.p..self.p + n];
        self.p += n;
        Ok(s)
    }
    pub fn u8(&mut self) -> Res<u8> {
        Ok(self.take(1)?[0])
    }
    pub fn u16(&mut self) -> Res<u16> {
        let s = self.take(2)?;
        Ok(u16::from_le_bytes([s[0], s[1]]))
    }
    pub fn u32(&mut self) -> Res<u32> {
        let s = self.take(4)?;
        Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
    }
    /// The engine's string object.
    pub fn str(&mut self) -> Res<String> {
        let flags = self.u32()?;
        let len = self.u32()? as usize;
        self.u32()?;
        let wide = (flags >> 28) & 7 == 2;
        if wide {
            let raw = self.take(len.checked_mul(2).ok_or(Short)?)?;
            let units: Vec<u16> = raw.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
            Ok(String::from_utf16_lossy(&units))
        } else {
            let raw = self.take(len)?;
            Ok(raw.iter().map(|&c| c as char).collect())
        }
    }
    /// Key/value map: `u32 count` + key/value string pairs.
    pub fn kv(&mut self) -> Res<Vec<(String, String)>> {
        let n = self.u32()?;
        let mut m = Vec::new();
        for _ in 0..n {
            let k = self.str()?;
            let v = self.str()?;
            m.push((k, v));
        }
        Ok(m)
    }
}

/// Little-endian writer.
#[derive(Default)]
pub struct Writer {
    pub b: Vec<u8>,
}

impl Writer {
    pub fn new() -> Self {
        Writer { b: Vec::new() }
    }
    pub fn u8(&mut self, x: u8) -> &mut Self {
        self.b.push(x);
        self
    }
    pub fn u16(&mut self, x: u16) -> &mut Self {
        self.b.extend_from_slice(&x.to_le_bytes());
        self
    }
    pub fn u32(&mut self, x: u32) -> &mut Self {
        self.b.extend_from_slice(&x.to_le_bytes());
        self
    }
    pub fn bytes(&mut self, x: &[u8]) -> &mut Self {
        self.b.extend_from_slice(x);
        self
    }
    /// The engine's string object, 8-bit (characters above U+00FF are truncated like the original).
    pub fn str(&mut self, s: &str) -> &mut Self {
        let bytes: Vec<u8> = s.chars().map(|c| c as u32 as u8).collect();
        self.u32(1).u32(bytes.len() as u32).u32(bytes.len() as u32).bytes(&bytes)
    }
    pub fn kv(&mut self, m: &[(String, String)]) -> &mut Self {
        self.u32(m.len() as u32);
        for (k, v) in m {
            self.str(k).str(v);
        }
        self
    }
    pub fn done(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.b)
    }
}

/// One frame `[u16 length of the rest][u8 channel][payload]`.
pub fn frame(ch: u8, payload: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(payload.len() + 3);
    v.extend_from_slice(&((payload.len() + 1) as u16).to_le_bytes());
    v.push(ch);
    v.extend_from_slice(payload);
    v
}

/// Splits a relay packet into frames (stops at the first malformed one).
pub fn parse_frames(b: &[u8]) -> Vec<(u8, &[u8])> {
    let mut out = Vec::new();
    let mut p = 0;
    while b.len() - p >= 3 {
        let len = u16::from_le_bytes([b[p], b[p + 1]]) as usize;
        p += 2;
        if len < 1 || len > b.len() - p {
            break;
        }
        out.push((b[p], &b[p + 1..p + len]));
        p += len;
    }
    out
}

/// Splits a message into MT packets (one relay packet each). Messages larger than the framing
/// can number (63 pieces) are cut off: the engine would drop them anyway.
pub fn mt_packets(ch: u8, data: &[u8]) -> Vec<Vec<u8>> {
    let mut msg = Vec::with_capacity(data.len() + 1);
    msg.push(ch);
    msg.extend_from_slice(data);
    let max_pieces = (1usize << (16 - MT_SHIFT)) - 1;
    let n = msg.len().div_ceil(MT_PIECE).min(max_pieces);
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let piece = &msg[i * MT_PIECE..((i + 1) * MT_PIECE).min(msg.len())];
        let field = if i == n - 1 { 0 } else { i + 1 };
        let hdr = ((field as u16) << MT_SHIFT) | piece.len() as u16;
        let mut pkt = Vec::with_capacity(piece.len() + 2);
        pkt.extend_from_slice(&hdr.to_le_bytes());
        pkt.extend_from_slice(piece);
        out.push(pkt);
    }
    out
}

/// Reassembly state of one sender's MT messages.
#[derive(Default)]
pub struct MtState {
    parts: Vec<u8>,
    expect: u16,
}

/// Reassembles messages from MT packets: `(channel, payload)` for every completed message.
pub fn mt_messages(st: &mut MtState, b: &[u8]) -> Vec<(u8, Vec<u8>)> {
    let mut out = Vec::new();
    let mut o = 0;
    if st.expect == 0 {
        st.expect = 1;
    }
    while o + 3 <= b.len() {
        let hdr = u16::from_le_bytes([b[o], b[o + 1]]);
        let len = (hdr & MT_LEN_MASK) as usize;
        let field = hdr >> MT_SHIFT;
        o += 2;
        if len < 1 || o + len > b.len() {
            break;
        }
        let piece = &b[o..o + len];
        o += len;
        if field != 0 && field != st.expect {
            // out of order: drop what was collected
            st.parts.clear();
            st.expect = 1;
            continue;
        }
        st.parts.extend_from_slice(piece);
        if field != 0 {
            st.expect += 1;
            continue;
        }
        let msg = std::mem::take(&mut st.parts);
        st.expect = 1;
        out.push((msg[0], msg[1..].to_vec()));
    }
    out
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mt_round_trip_of_a_large_message() {
        let data: Vec<u8> = (0..5000).map(|i| (i & 255) as u8).collect();
        let pkts = mt_packets(CH_GAME, &data);
        assert_eq!(pkts.len(), 5001usize.div_ceil(MT_PIECE));
        let mut st = MtState::default();
        let mut got = Vec::new();
        for p in &pkts {
            got.extend(mt_messages(&mut st, p));
        }
        assert_eq!(got, vec![(CH_GAME, data)]);
        // a small message is one packet with piece 0, the same bytes as a plain frame
        assert_eq!(mt_packets(CH_GAME, &[1, 2, 3]), vec![frame(CH_GAME, &[1, 2, 3])]);
    }

    #[test]
    fn truncated_strings_and_maps_are_errors_not_panics() {
        let mut w = Writer::new();
        w.u32(1).u32(1000).u32(1000).bytes(b"abc");
        assert_eq!(Reader::new(&w.done()).str(), Err(Short));
        let mut w = Writer::new();
        w.u32(0x2000_0000).u32(u32::MAX).u32(0);
        assert_eq!(Reader::new(&w.done()).str(), Err(Short));
        let mut w = Writer::new();
        w.u32(u32::MAX); // a map that claims 4 billion entries
        assert_eq!(Reader::new(&w.done()).kv(), Err(Short));
    }

    #[test]
    fn strings_round_trip_and_utf16_is_read() {
        let mut w = Writer::new();
        w.str("PLUMBER");
        assert_eq!(Reader::new(&w.done()).str().unwrap(), "PLUMBER");
        let mut w = Writer::new();
        w.u32(0x2000_0001).u32(2).u32(2).bytes(&[b'h', 0, b'i', 0]);
        assert_eq!(Reader::new(&w.done()).str().unwrap(), "hi");
    }

    #[test]
    fn frames_stop_at_a_bad_length() {
        let mut b = frame(1, &[9]);
        b.extend_from_slice(&[0xff, 0xff, 2, 0]);
        assert_eq!(parse_frames(&b), vec![(1u8, &[9u8][..])]);
    }
}

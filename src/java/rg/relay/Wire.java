package rg.relay;

import java.io.ByteArrayOutputStream;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.List;
import java.util.Map;

/**
 * Wire formats of the original N-Gage Arena peers (port of server/src/casino.js and the MT
 * framing in gameroom.js; docs/MULTIPLAYER.md).
 *
 * A relay packet holds frames [u16 length of the rest][u8 channel][payload] (little-endian).
 * MT framing (game rooms): [u16 header][bytes], header = length | piece << MT_SHIFT, pieces of
 * a split message numbered 1, 2, ... and the last one 0.
 */
final class Wire {
    static final int CH_SYSTEM = 1;       // keep-alive: u32 timeout on join, then 1-byte pings
    static final int CH_CONTROL = 2;      // transport control: ping replies, 00 on join
    static final int CH_COMMAND = 3;      // key/value session hello
    static final int CH_CASINO = 0x11;    // the casino system
    static final int CH_MATCHMAKER = 0x12;
    static final int CH_FRIENDLY = 0x13;
    static final int CH_TABLE = 0x15;     // game room seating
    static final int CH_GAME = 0x16;      // the match
    static final int REQUEST = 0, RESPONSE = 1, EVENT = 2;
    static final int MT_SHIFT = 10;
    static final int MT_LEN_MASK = (1 << MT_SHIFT) - 1;
    static final int MT_PIECE = 874;

    private static final String[] ACTIONS = {
        "CASINOSYSTEM_LOGIN", "CASINOSYSTEM_LOGOUT", "CASINOSYSTEM_UPDATE", "CASINOSYSTEM_F_CASINO",
        "GET_RANKINGS", "GET_STATISTICS", "GET_ACHIEVEMENTS", "GET_FRIENDLY_CASINO_LIST",
        "CASINO_ENTER", "CASINO_MATCHMAKING", "CASINO_CANCEL_MATCHMAKING", "CASINO_EXIT",
        "SEAT_RESERVE", "SEAT_TAKE", "SEAT_LEAVE", "SEAT_RESERVATION_EXPIRED",
        "SEATING_FAILED", "GAME_START_FAILED", "TABLE_CLOSED", "TABLE_TERMINATED",
        "CASINO_CREATE", "FRIENDLY_CASINO_ENTERED", "FRIENDLY_CASINO_LEFT", "FRIENDLY_CASINO_PLAYING",
        "FRIENDLY_CASINO_UNAVAIL", "FRIENDLY_UPDATE", "CASINO_SHUTDOWN", "CASINO_ACTIVATE",
        "FRIENDLY_CASINO_START", "LOCK_PLAYER_IN_GAME", "UNLOCK_PLAYER_FROM_GAME", "CASINO_MATCHED_UP",
        null, "REGISTER_FRIENDS", "VERSION_NOT_SUPPORTED", "FRIEND_REMOVE", "FRIEND_ADD",
        "CASINO_NOT_FOUND", "CASINO_CHECK_STATE", "PRESENCE_SUBSCRIBE", "PRESENCE_UNSUBSCRIBE",
        "PRESENCE_ACCEPT", "PRESENCE_DECLINE",
    };

    private Wire() {
    }

    static String action(int a) {
        return a >= 0 && a < ACTIONS.length && ACTIONS[a] != null ? ACTIONS[a] : "ACTION_" + a;
    }

    static String type(int t) {
        return t == REQUEST ? "REQUEST" : t == RESPONSE ? "RESPONSE" : t == EVENT ? "EVENT" : String.valueOf(t);
    }

    /** One frame / MT message: channel + payload. */
    static final class Frame {
        final int ch;
        final byte[] data;

        Frame(int ch, byte[] data) {
            this.ch = ch;
            this.data = data;
        }
    }

    /** Little-endian reader. */
    static final class Reader {
        private final byte[] b;
        int p;

        Reader(byte[] b) {
            this.b = b;
        }

        int left() {
            return this.b.length - this.p;
        }

        private void need(int n) {
            if (this.p + n > this.b.length) {
                throw new IndexOutOfBoundsException("read past end");
            }
        }

        int u8() {
            this.need(1);
            return this.b[this.p++] & 0xff;
        }

        int u16() {
            this.need(2);
            int x = (this.b[this.p] & 0xff) | (this.b[this.p + 1] & 0xff) << 8;
            this.p += 2;
            return x;
        }

        long u32() {
            this.need(4);
            long x = (this.b[this.p] & 0xffL) | (this.b[this.p + 1] & 0xffL) << 8
                    | (this.b[this.p + 2] & 0xffL) << 16 | (this.b[this.p + 3] & 0xffL) << 24;
            this.p += 4;
            return x;
        }

        /** The engine's string object: [u32 refcount|flags][u32 length][u32 capacity][bytes]. */
        String str() {
            long flags = this.u32();
            int len = (int) this.u32();
            this.u32();
            boolean wide = ((flags >>> 28) & 7) == 2;
            int n = wide ? 2 * len : len;
            if (len < 0 || n < 0) {
                throw new IndexOutOfBoundsException("bad string");
            }
            this.need(n);
            String s;
            if (wide) {
                s = new String(this.b, this.p, n, StandardCharsets.UTF_16LE);
            } else {
                s = new String(this.b, this.p, n, StandardCharsets.ISO_8859_1);
            }
            this.p += n;
            return s;
        }
    }

    /** Little-endian writer. */
    static final class Writer {
        private final ByteArrayOutputStream out = new ByteArrayOutputStream();

        Writer push(byte[] a) {
            this.out.write(a, 0, a.length);
            return this;
        }

        Writer u8(long x) {
            this.out.write((int) x & 255);
            return this;
        }

        Writer u16(long x) {
            return this.u8(x).u8(x >>> 8);
        }

        Writer u32(long x) {
            return this.u8(x).u8(x >>> 8).u8(x >>> 16).u8(x >>> 24);
        }

        Writer str(String s) {
            byte[] bytes = new byte[s.length()];
            for (int i = 0; i < bytes.length; i++) {
                bytes[i] = (byte) s.charAt(i);
            }
            return this.u32(1).u32(bytes.length).u32(bytes.length).push(bytes);
        }

        Writer kv(Map<String, String> m) {
            this.u32(m.size());
            for (Map.Entry<String, String> e : m.entrySet()) {
                this.str(e.getKey()).str(e.getValue());
            }
            return this;
        }

        byte[] bytes() {
            return this.out.toByteArray();
        }
    }

    static byte[] frame(int ch, byte[] payload) {
        return new Writer().u16(payload.length + 1).u8(ch).push(payload).bytes();
    }

    static byte[] concat(byte[] a, byte[] b) {
        byte[] out = Arrays.copyOf(a, a.length + b.length);
        System.arraycopy(b, 0, out, a.length, b.length);
        return out;
    }

    /** Splits a relay packet into frames. */
    static List<Frame> parseFrames(byte[] bytes) {
        List<Frame> frames = new ArrayList<Frame>();
        Reader r = new Reader(bytes);
        while (r.left() >= 3) {
            int len = r.u16();
            if (len < 1 || len > r.left()) {
                break;
            }
            int ch = r.u8();
            frames.add(new Frame(ch, Arrays.copyOfRange(bytes, r.p, r.p + len - 1)));
            r.p += len - 1;
        }
        return frames;
    }

    /** Splits a message into MT packets (one relay packet each). */
    static List<byte[]> mtPackets(int ch, byte[] data) {
        byte[] msg = new byte[data.length + 1];
        msg[0] = (byte) ch;
        System.arraycopy(data, 0, msg, 1, data.length);
        int n = (msg.length + MT_PIECE - 1) / MT_PIECE;
        if (n > (1 << (16 - MT_SHIFT)) - 1) {
            throw new IllegalArgumentException("message of " + msg.length + " bytes does not fit MT framing");
        }
        List<byte[]> out = new ArrayList<byte[]>();
        for (int i = 0; i < n; i++) {
            int from = i * MT_PIECE, len = Math.min(MT_PIECE, msg.length - from);
            int field = i == n - 1 ? 0 : i + 1;
            int hdr = field << MT_SHIFT | len;
            byte[] pkt = new byte[2 + len];
            pkt[0] = (byte) hdr;
            pkt[1] = (byte) (hdr >> 8);
            System.arraycopy(msg, from, pkt, 2, len);
            out.add(pkt);
        }
        return out;
    }

    /** The pieces of an unfinished MT message. */
    static final class MtState {
        final ByteArrayOutputStream parts = new ByteArrayOutputStream();
        int expect = 1;
    }

    /** Reassembles messages from MT packets. */
    static List<Frame> mtMessages(MtState st, byte[] bytes) {
        List<Frame> out = new ArrayList<Frame>();
        int o = 0;
        while (o + 3 <= bytes.length) {
            int hdr = (bytes[o] & 0xff) | (bytes[o + 1] & 0xff) << 8;
            int len = hdr & MT_LEN_MASK, field = hdr >> MT_SHIFT;
            o += 2;
            if (len < 1 || o + len > bytes.length) {
                break;
            }
            int from = o;
            o += len;
            if (field != 0 && field != st.expect) {     // out of order
                st.parts.reset();
                st.expect = 1;
                continue;
            }
            st.parts.write(bytes, from, len);
            if (field != 0) {
                st.expect++;
                continue;
            }
            byte[] msg = st.parts.toByteArray();
            st.parts.reset();
            st.expect = 1;
            out.add(new Frame(msg[0] & 0xff, Arrays.copyOfRange(msg, 1, msg.length)));
        }
        return out;
    }

    static String hex(byte[] b) {
        StringBuilder s = new StringBuilder();
        for (byte x : b) {
            s.append(String.format("%02x", x & 0xff));
        }
        return s.toString();
    }

    static String hexFrom(byte[] b, int from) {
        return from >= b.length ? "" : hex(Arrays.copyOfRange(b, from, b.length));
    }
}

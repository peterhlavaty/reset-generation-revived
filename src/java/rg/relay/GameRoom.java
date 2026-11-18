package rg.relay;

import static rg.relay.Wire.CH_COMMAND;
import static rg.relay.Wire.CH_CONTROL;
import static rg.relay.Wire.CH_GAME;
import static rg.relay.Wire.CH_SYSTEM;
import static rg.relay.Wire.CH_TABLE;
import static rg.relay.Wire.EVENT;
import static rg.relay.Wire.REQUEST;
import static rg.relay.Wire.RESPONSE;
import static rg.relay.Wire.frame;

import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.concurrent.ScheduledFuture;
import java.util.concurrent.TimeUnit;

/**
 * Game-room server: the per-room "game server" of the original N-Gage Arena (port of
 * server/src/gameroom.js; docs/MULTIPLAYER.md, "The game room"). It greets the players, answers
 * pings, serves the seating protocol on channel 0x15 and routes the match (channel 0x16) between
 * the players and the hosting game ([u16 player id][message] on the server link).
 */
final class GameRoom implements Relay.Bot {
    static final int SEAT_RESERVE = 12, SEAT_TAKE = 13, SEAT_LEAVE = 14;
    private static final long KEEPALIVE_MS = 10000;

    /** The game server process serving the room (a host player's engine). */
    interface ServerLink {
        void ctl(Map<String, Object> msg);

        void send(byte[] bytes);
    }

    private static final class Player {
        boolean greeted, announced;
        String pid, seat;          // seat: null, "reserved" or "taken"
        final int sid;
        final Wire.MtState frag = new Wire.MtState();

        Player(int sid) {
            this.sid = sid;
        }
    }

    private final Relay relay;
    final String name;
    private final int expected;
    private final Map<Relay.Client, Player> players = new LinkedHashMap<Relay.Client, Player>();
    private boolean started, served;
    private ServerLink server;
    private int nextSid = 1, pingSeq = 1;
    private ScheduledFuture<?> timer;

    GameRoom(Relay relay, String name, int expected) {
        this.relay = relay;
        this.name = name;
        this.expected = expected;
    }

    @Override
    public void join(Relay.Client client) {
        this.players.put(client, new Player(this.nextSid++));
        if (this.started || this.players.size() >= this.expected) {
            this.start();
        }
    }

    /** Greets everyone not yet greeted: transport hello (ch 2: 00) and keep-alive timeout. */
    void start() {
        this.started = true;
        byte[] hello = Wire.concat(frame(CH_CONTROL, new byte[]{0}), frame(CH_SYSTEM, new Wire.Writer().u32(20000).bytes()));
        for (Map.Entry<Relay.Client, Player> e : this.players.entrySet()) {
            if (e.getValue().greeted) {
                continue;
            }
            e.getValue().greeted = true;
            e.getKey().send(hello);
        }
        if (this.timer == null) {
            this.timer = this.relay.sched.scheduleAtFixedRate(this::keepAlive, KEEPALIVE_MS, KEEPALIVE_MS, TimeUnit.MILLISECONDS);
        }
    }

    private void keepAlive() {
        this.pingSeq = this.pingSeq % 255 + 1;
        for (Map.Entry<Relay.Client, Player> e : new ArrayList<Map.Entry<Relay.Client, Player>>(this.players.entrySet())) {
            if (e.getValue().greeted) {
                e.getKey().send(frame(CH_SYSTEM, new byte[]{(byte) this.pingSeq}));
            }
        }
    }

    @Override
    public void leave(Relay.Client client) {
        Player p = this.players.remove(client);
        if (p != null && p.announced && this.server != null) {
            this.server.ctl(Relay.msg("t", "player_left", "pid", p.sid));
        }
        if (p != null && p.seat != null) {
            this.seatLeft(p.pid);
        }
        if (this.players.isEmpty()) {
            this.close();
        }
    }

    void close() {
        if (this.timer != null) {
            this.timer.cancel(false);
        }
        this.timer = null;
    }

    @Override
    public void packet(Relay.Client client, byte[] bytes) {
        Player p = this.players.get(client);
        if (p == null) {
            return;
        }
        List<Wire.Frame> forward = new ArrayList<Wire.Frame>();
        for (Wire.Frame f : Wire.mtMessages(p.frag, bytes)) {
            if (f.ch == CH_GAME && this.server != null) {
                this.toServer(p, f.data);
            } else if (f.ch == CH_TABLE) {
                this.tableMessage(client, p, f.data);
            } else if (f.ch == CH_SYSTEM && f.data.length == 1) {
                client.send(frame(CH_CONTROL, f.data));      // pong
            } else if (f.ch == CH_CONTROL && f.data.length == 1 && f.data[0] == 0) {
                client.send(frame(CH_CONTROL, new byte[]{0x20}));
            } else if (f.ch == CH_CONTROL || f.ch == CH_SYSTEM) {
                // pong / keep-alive setting
            } else if (f.ch == CH_COMMAND) {
                this.command(client, p, f.data);
            } else {
                forward.add(f);
            }
        }
        for (Wire.Frame f : forward) {
            this.relay.log("room " + this.name + ": " + client.name + " -> others ch " + f.ch + " " + f.data.length + " bytes");
            for (Relay.Client c : new ArrayList<Relay.Client>(this.players.keySet())) {
                if (c != client) {
                    for (byte[] pkt : Wire.mtPackets(f.ch, f.data)) {
                        c.send(pkt);
                    }
                }
            }
        }
    }

    /** The game server process serves this room (see Relay.serve). */
    void attachServer(ServerLink link) {
        this.served = true;
        this.server = link;
        for (Map.Entry<Relay.Client, Player> e : new ArrayList<Map.Entry<Relay.Client, Player>>(this.players.entrySet())) {
            this.announce(e.getKey(), e.getValue());
        }
    }

    /** Tells the game server about a player once it has taken its seat (its game session exists). */
    private void announce(Relay.Client client, Player p) {
        if (p.announced || this.server == null || !"taken".equals(p.seat)) {
            return;
        }
        p.announced = true;
        this.server.ctl(Relay.msg("t", "player_joined", "pid", p.sid, "name", client.name));
    }

    void detachServer() {
        this.server = null;
    }

    /** Player -> server: [u16 player id][message]. */
    private void toServer(Player p, byte[] data) {
        byte[] out = new byte[data.length + 2];
        out[0] = (byte) p.sid;
        out[1] = (byte) (p.sid >> 8);
        System.arraycopy(data, 0, out, 2, data.length);
        this.relay.log("room " + this.name + ": player " + p.sid + " -> server " + data.length + " bytes");
        this.server.send(out);
    }

    /** Server -> player: [u16 player id][message], split into MT packets. */
    void fromServer(byte[] bytes) {
        if (bytes.length < 2) {
            return;
        }
        int sid = (bytes[0] & 0xff) | (bytes[1] & 0xff) << 8;
        for (Map.Entry<Relay.Client, Player> e : this.players.entrySet()) {
            if (e.getValue().sid != sid) {
                continue;
            }
            this.relay.log("room " + this.name + ": server -> player " + sid + " " + (bytes.length - 2) + " bytes");
            byte[] msg = new byte[bytes.length - 2];
            System.arraycopy(bytes, 2, msg, 0, msg.length);
            for (byte[] pkt : Wire.mtPackets(CH_GAME, msg)) {
                e.getKey().send(pkt);
            }
            return;
        }
    }

    /** Channel-3 session hello: key/value pairs (name, pid, passwd, roomname, ...). */
    private void command(Relay.Client client, Player p, byte[] data) {
        try {
            Wire.Reader r = new Wire.Reader(data);
            Map<String, String> kv = new LinkedHashMap<String, String>();
            for (int n = r.u8(); n > 0; n--) {
                String k = r.str();
                kv.put(k, r.str());
            }
            if (kv.get("pid") != null && !kv.get("pid").isEmpty() && p.pid == null) {
                p.pid = kv.get("pid");
            }
            this.relay.log("room " + this.name + ": " + client.name + " hello " + kv);
        } catch (RuntimeException e) {
            this.relay.log("room " + this.name + ": " + client.name + " command " + Wire.hex(data));
        }
    }

    private static long num(String s) {
        try {
            return s == null ? 0 : Long.parseLong(s.trim()) & 0xffffffffL;
        } catch (NumberFormatException e) {
            return 0;
        }
    }

    private void tableMessage(Relay.Client client, Player p, byte[] data) {
        Wire.Reader r = new Wire.Reader(data);
        int type = r.u8(), action = r.u8();
        this.relay.log("room " + this.name + ": " + client.name + " " + Wire.type(type) + " " + Wire.action(action)
                + " " + Wire.hexFrom(data, 2));
        if (type != REQUEST) {
            return;
        }
        String pid = r.left() >= 4 ? String.valueOf(r.u32()) : p.pid;
        p.pid = pid;
        switch (action) {
            case SEAT_RESERVE:
            case SEAT_TAKE: {
                boolean first = p.seat == null;
                p.seat = action == SEAT_TAKE ? "taken" : (p.seat == null ? "reserved" : p.seat);
                this.respond(client, action, num(pid));
                this.announce(client, p);
                // the newcomer learns about everyone already seated, then everyone learns about it
                if (first) {
                    for (Map.Entry<Relay.Client, Player> e : this.players.entrySet()) {
                        Player o = e.getValue();
                        if (e.getKey() != client && o.seat != null) {
                            this.sendEvent(client, "taken".equals(o.seat) ? SEAT_TAKE : SEAT_RESERVE, o.pid);
                        }
                    }
                }
                this.event(action, pid);
                break;
            }
            case SEAT_LEAVE:
                p.seat = null;
                this.respond(client, action, num(pid));
                this.seatLeft(pid);
                break;
            default:
                this.respond(client, action, 0);
        }
    }

    private void respond(Relay.Client client, int action, long value) {
        client.send(frame(CH_TABLE, new Wire.Writer().u8(RESPONSE).u8(action).u8(0).u32(value).bytes()));
    }

    private void sendEvent(Relay.Client client, int action, String pid) {
        client.send(frame(CH_TABLE, new Wire.Writer().u8(EVENT).u8(action).str(String.valueOf(pid)).bytes()));
    }

    /**
     * A player left its seat. With a game server the match reports leavers itself (an EVENT
     * SEAT_LEAVE would leave a client on its win screen on a black screen), so the event is only
     * sent while no game server serves the room.
     */
    private void seatLeft(String pid) {
        if (this.served) {
            return;
        }
        this.event(SEAT_LEAVE, pid);
    }

    /** Tells every greeted player about a seat change. */
    private void event(int action, String pid) {
        for (Map.Entry<Relay.Client, Player> e : new ArrayList<Map.Entry<Relay.Client, Player>>(this.players.entrySet())) {
            if (e.getValue().greeted) {
                this.sendEvent(e.getKey(), action, pid);
            }
        }
    }
}

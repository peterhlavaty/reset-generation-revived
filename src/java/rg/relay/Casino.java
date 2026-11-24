package rg.relay;

import static rg.relay.Wire.CH_CASINO;
import static rg.relay.Wire.CH_COMMAND;
import static rg.relay.Wire.CH_CONTROL;
import static rg.relay.Wire.CH_FRIENDLY;
import static rg.relay.Wire.CH_MATCHMAKER;
import static rg.relay.Wire.CH_SYSTEM;
import static rg.relay.Wire.EVENT;
import static rg.relay.Wire.REQUEST;
import static rg.relay.Wire.RESPONSE;
import static rg.relay.Wire.frame;

import java.util.ArrayList;
import java.util.HashMap;
import java.util.HashSet;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.Set;
import java.util.concurrent.ScheduledFuture;
import java.util.concurrent.TimeUnit;
import java.util.function.Consumer;

/**
 * The "CasinoSystem": the server-side matchmaker of the original N-Gage Arena (port of
 * server/src/casino.js; docs/MULTIPLAYER.md, "Casino protocol"). Quick Match fills games hosted by
 * a player; with matchSize > 0 it also forms QM_n rooms (they need a separate game server).
 */
final class Casino implements Relay.Bot {
    private static final long KEEPALIVE_MS = 10000;   // clients drop a peer that is silent for 20 s
    private static final long LOCK_TIMEOUT_MS = 3000;  // wait this long for LOCK_PLAYER_IN_GAME answers
    static final String MATCHMAKER_NAME = "QuickMatch";

    private static final class Player {
        int id;
        String hero, random;
    }

    private static final class Waiting {
        final Relay.Client client;
        final int ch;

        Waiting(Relay.Client client, int ch) {
            this.client = client;
            this.ch = ch;
        }
    }

    private static final class Hosted {
        final String room;
        final int players;

        Hosted(String room, int players) {
            this.room = room;
            this.players = players;
        }
    }

    private static final class Match {
        List<Waiting> group;
        String room;
        final Set<Relay.Client> locked = new HashSet<Relay.Client>();
        ScheduledFuture<?> timer;
        boolean done;
    }

    private final Relay relay;
    private final Map<Relay.Client, Player> players = new LinkedHashMap<Relay.Client, Player>();
    private List<Waiting> queue = new ArrayList<Waiting>();       // clients in Quick Match
    private final Map<Relay.Client, Match> locking = new HashMap<Relay.Client, Match>();
    private final List<Hosted> hosted = new ArrayList<Hosted>();  // host players' games waiting
    private final Map<String, Integer> ids = new HashMap<String, Integer>();   // nickname -> player id
    /** Hosted matches being played (room key -> room name) and who lost the connection in them. */
    private final Map<String, String> running = new HashMap<String, String>();
    private final Map<String, Map<String, Long>> dropped = new HashMap<String, Map<String, Long>>();
    /** How long a dropped player may come back: the host holds the seat for 180 s (rghost HOLD_MS). */
    static final long REJOIN_MS = 170000;
    private int nextTable = 1, nextPlayerId, pingSeq = 1;
    private ScheduledFuture<?> timer;
    int matchSize;

    Casino(Relay relay) {
        this.relay = relay;
    }

    int hostedWaiting() {
        return this.hosted.size();
    }

    /** Player profile key/value map the client reads (id, name, fameLevel, heroType, random). */
    private Map<String, String> profile(Relay.Client client) {
        Player p = this.players.get(client);
        Map<String, String> m = new LinkedHashMap<String, String>();
        m.put("id", String.valueOf(p == null ? 0 : p.id));
        m.put("name", client.name);
        m.put("fameLevel", "0");
        m.put("heroType", p == null || p.hero == null ? "PLUMBER" : p.hero);
        m.put("random", p == null || p.random == null ? "FALSE" : p.random);
        return m;
    }

    private void send(Relay.Client c, byte[] bytes) {
        c.send(bytes);
    }

    @Override
    public void join(Relay.Client client) {
        Player p = new Player();
        p.id = this.playerId(client.name);
        this.players.put(client, p);
        // The client starts its casino session once the room's server has spoken. Like a peer,
        // the server opens with a transport hello (ch 2: 00) and its keep-alive timeout.
        this.send(client, frame(CH_CONTROL, new byte[]{0}));
        this.send(client, frame(CH_SYSTEM, new Wire.Writer().u32(20000).bytes()));
        if (this.timer == null) {
            this.timer = this.relay.sched.scheduleAtFixedRate(this::keepAlive, KEEPALIVE_MS, KEEPALIVE_MS, TimeUnit.MILLISECONDS);
        }
    }

    /**
     * A player keeps their id when they come back to the casino (after a match). The engine keys
     * the casino's member map by it and only removes the current id on leave: a changed id leaves
     * a stale entry holding the engine's own user object, which it deletes when the connection
     * drops, and then crashes.
     */
    private int playerId(String name) {
        Integer id = this.ids.get(name);
        if (id != null) {
            for (Player other : this.players.values()) {
                if (other.id == id) {          // the name is in use by another connection
                    id = null;
                    break;
                }
            }
        }
        if (id == null) {
            id = ++this.nextPlayerId;
            this.ids.put(name, id);
        }
        return id;
    }

    /** Pings every player so the engine does not time the casino out. */
    private void keepAlive() {
        if (this.players.isEmpty()) {
            this.timer.cancel(false);
            this.timer = null;
            return;
        }
        this.pingSeq = this.pingSeq % 255 + 1;
        for (Relay.Client c : new ArrayList<Relay.Client>(this.players.keySet())) {
            this.send(c, frame(CH_SYSTEM, new byte[]{(byte) this.pingSeq}));
        }
    }

    @Override
    public void leave(Relay.Client client) {
        if (this.inQueue(client)) {
            this.notifyQueue(client, 10);
        }
        this.players.remove(client);
        this.dequeue(client);
    }

    private boolean inQueue(Relay.Client client) {
        for (Waiting q : this.queue) {
            if (q.client == client) {
                return true;
            }
        }
        return false;
    }

    private void dequeue(Relay.Client client) {
        List<Waiting> rest = new ArrayList<Waiting>();
        for (Waiting q : this.queue) {
            if (q.client != client) {
                rest.add(q);
            }
        }
        this.queue = rest;
    }

    /** EVENT CASINO_MATCHMAKING (9, [string id][profile]) / CASINO_CANCEL_MATCHMAKING (10, [string
     *  id]): updates the other waiting players' "Players in Match" list. */
    private void notifyQueue(Relay.Client client, final int action) {
        final Map<String, String> p = this.profile(client);
        for (Waiting q : new ArrayList<Waiting>(this.queue)) {
            if (q.client == client) {
                continue;
            }
            this.reply(q.client, EVENT, action, w -> {
                if (action == 9) {
                    w.str(p.get("id")).kv(p);
                } else {
                    w.str(p.get("id"));
                }
            }, q.ch);
        }
    }

    /** A host player's game (its engine is the game server) takes the next `players` players. */
    void addHosted(String room, int players) {
        this.removeHosted(room.toLowerCase());
        this.hosted.add(new Hosted(room, players));
        this.relay.log("casino: hosted game " + room + " for " + players + " player(s)");
        this.matchmake();
    }

    void removeHosted(String key) {
        this.hosted.removeIf(h -> h.room.toLowerCase().equals(key));
    }

    /** The host's game server is gone (match over): no one can come back into it. */
    void gameOver(String key) {
        this.removeHosted(key);
        this.running.remove(key);
        this.dropped.remove(key);
    }

    /**
     * A player's connection dropped while in a hosted match (not a normal leave): the next time
     * they ask for Quick Match they go back into it, the host's engine takes them in again.
     */
    void droppedOut(String key, String name) {
        if (this.running.containsKey(key)) {
            this.dropped.computeIfAbsent(key, k -> new HashMap<String, Long>()).put(name.toLowerCase(), System.currentTimeMillis());
            this.relay.log("casino: " + name + " dropped out of " + this.running.get(key) + " (may rejoin)");
        }
    }

    /** A player left a hosted match the normal way (surrendered, match over): no rejoin. */
    void leftGame(String key, String name) {
        Map<String, Long> d = this.dropped.get(key);
        if (d != null) {
            d.remove(name.toLowerCase());
        }
    }

    /** The running hosted match this player dropped out of, or null. */
    private String rejoinRoom(Relay.Client client) {
        for (Map.Entry<String, Map<String, Long>> e : this.dropped.entrySet()) {
            Long t = e.getValue().remove(client.name.toLowerCase());
            if (t != null && System.currentTimeMillis() - t <= REJOIN_MS) {
                return this.running.get(e.getKey());
            }
        }
        return null;
    }

    /**
     * Forms a match once enough players are queued: EVENT CASINO_ENTER for each other member, then
     * REQUEST LOCK_PLAYER_IN_GAME ("Game starting"), and once all have answered (or after a
     * timeout) EVENT CASINO_MATCHED_UP with the room (see casino.js).
     */
    private void matchmake() {
        while (!this.hosted.isEmpty() && this.queue.size() >= this.hosted.get(0).players) {
            Hosted h = this.hosted.remove(0);
            this.startMatch(this.take(h.players), h.room);
        }
        if (!this.hosted.isEmpty()) {
            return;    // players wait for the hosted game
        }
        while (this.matchSize > 0 && this.queue.size() >= this.matchSize) {
            this.startMatch(this.take(this.matchSize), "QM_" + this.nextTable++);
        }
    }

    private List<Waiting> take(int n) {
        List<Waiting> group = new ArrayList<Waiting>(this.queue.subList(0, n));
        this.queue = new ArrayList<Waiting>(this.queue.subList(n, this.queue.size()));
        return group;
    }

    private void startMatch(List<Waiting> group, String room) {
        StringBuilder names = new StringBuilder();
        for (Waiting q : group) {
            names.append(names.length() > 0 ? ", " : "").append(q.client.name);
        }
        this.relay.log("casino: matched " + names + " -> " + room);
        for (Waiting q : group) {
            for (Waiting o : group) {
                if (o != q) {
                    final Map<String, String> po = this.profile(o.client);
                    this.reply(q.client, EVENT, 8, w -> w.str(po.get("id")).kv(po), q.ch);
                }
            }
        }
        final Match match = new Match();
        match.group = group;
        match.room = room;
        for (Waiting q : group) {
            this.locking.put(q.client, match);
            this.reply(q.client, REQUEST, 29, w -> w.u32(0), CH_CASINO);
        }
        match.timer = this.relay.sched.schedule(() -> this.matchedUp(match), LOCK_TIMEOUT_MS, TimeUnit.MILLISECONDS);
    }

    /** A player acknowledged LOCK_PLAYER_IN_GAME. */
    private void locked(Relay.Client client) {
        Match match = this.locking.get(client);
        if (match == null) {
            return;
        }
        match.locked.add(client);
        for (Waiting q : match.group) {
            if (!match.locked.contains(q.client)) {
                return;
            }
        }
        this.matchedUp(match);
    }

    /** Tells each player of a locked group the game room to move to. */
    private void matchedUp(Match match) {
        if (match.done) {
            return;
        }
        match.done = true;
        if (match.timer != null) {
            match.timer.cancel(false);
        }
        List<Waiting> group = new ArrayList<Waiting>();
        for (Waiting q : match.group) {
            if (this.players.containsKey(q.client)) {
                group.add(q);
            }
        }
        for (Waiting q : match.group) {
            this.locking.remove(q.client);
        }
        final String room = match.room;
        List<Map<String, String>> profiles = new ArrayList<Map<String, String>>();
        for (Waiting q : group) {
            profiles.add(this.profile(q.client));
        }
        this.relay.expectRoom(room, group.size(), profiles);
        if (this.relay.isServed(room)) {
            this.running.put(room.toLowerCase(), room);
        }
        final int n = group.size();
        for (int i = 0; i < n; i++) {
            Waiting q = group.get(i);
            final String b = "naf://rg.com:80/" + room + "?hosted=" + (i == 0 ? "yes" : "no");
            // table: [string room][string address][u8 c][u8 d][u32 e][settings: u8 0, u8 a, string s, u8 flag]
            this.reply(q.client, EVENT, 31, w -> w.str(MATCHMAKER_NAME).str(room).str(b).u8(0).u8(1).u32(n)
                    .u8(0).u8(0).str(b).u8(0), q.ch);
        }
    }

    @Override
    public void packet(Relay.Client client, byte[] bytes) {
        for (Wire.Frame f : Wire.parseFrames(bytes)) {
            if (f.ch == CH_CASINO || f.ch == CH_MATCHMAKER || f.ch == CH_FRIENDLY) {
                this.casinoMessage(client, f.data, f.ch);
            } else if (f.ch == CH_COMMAND) {
                this.relay.log("casino: " + client.name + " command " + Wire.hex(f.data));
            } else if (f.ch == CH_SYSTEM && f.data.length == 1) {
                this.send(client, frame(CH_CONTROL, f.data));     // pong
            } else if (f.ch == CH_CONTROL && f.data.length == 1 && f.data[0] == 0) {
                this.send(client, frame(CH_CONTROL, new byte[]{0x20}));   // accept the connection
            } else if (f.ch != CH_CONTROL && f.ch != CH_SYSTEM) {
                this.relay.log("casino: " + client.name + " ch" + f.ch + " " + Wire.hex(f.data));
            }
        }
    }

    private void reply(Relay.Client client, int type, int action, Consumer<Wire.Writer> body, int ch) {
        Wire.Writer w = new Wire.Writer().u8(type).u8(action);
        body.accept(w);
        this.send(client, frame(ch, w.bytes()));
    }

    /** Generic RESPONSE: [u8 result (0 = ok)][u32 value] + extra. */
    private void respond(Relay.Client client, int action, long value, Consumer<Wire.Writer> extra, int ch) {
        this.reply(client, RESPONSE, action, w -> extra.accept(w.u8(0).u32(value)), ch);
    }

    /** Casino descriptor: [u8 kind=1 matchmaker][u32 id][u32 a][u32 b][u32 c][string name]. */
    private static Wire.Writer writeCasino(Wire.Writer w) {
        return w.u8(1).u32(1).u32(0).u32(0).u32(0).str(MATCHMAKER_NAME);
    }

    private void casinoMessage(final Relay.Client client, byte[] data, int ch) {
        Wire.Reader r = new Wire.Reader(data);
        int type = r.u8(), action = r.u8();
        this.relay.log("casino: " + client.name + " ch" + ch + " " + Wire.type(type) + " " + Wire.action(action)
                + " " + Wire.hexFrom(data, 2));
        if (type == RESPONSE && action == 29) {
            this.locked(client);
            return;
        }
        if (type != REQUEST) {
            return;
        }
        final Map<String, String> profile = this.profile(client);
        switch (action) {
            case 0: {   // CASINOSYSTEM_LOGIN: u32 version, string client version
                long version = r.u32();
                String clientVersion = r.str();
                this.relay.log("casino: login " + client.name + " protocol " + version + " (" + clientVersion + ")");
                // generic + profile key/value map + u8 has-casino + casino descriptor
                this.respond(client, 0, 0, w -> writeCasino(w.kv(profile).u8(1)), CH_CASINO);
                break;
            }
            case 5:     // GET_STATISTICS -> generic + stats: string, 9 x u32, u32 count (+ items)
                this.respond(client, 5, 0, w -> {
                    w.str(client.name);
                    for (int i = 0; i < 10; i++) {
                        w.u32(0);
                    }
                }, ch);
                break;
            case 6:     // GET_ACHIEVEMENTS -> generic + 200-byte achievement block
                this.respond(client, 6, 0, w -> w.push(new byte[200]), ch);
                break;
            case 8: {   // CASINO_ENTER: u32 casino id -> generic + key/value map + casino descriptor
                long id = r.u32();
                this.respond(client, 8, id, w -> writeCasino(w.kv(profile)), ch);
                break;
            }
            case 9: {   // CASINO_MATCHMAKING: string hero, string flag -> generic + u8 count + kv maps
                String hero = r.str(), flag = r.str();
                Player p = this.players.get(client);
                if (p != null) {
                    p.hero = hero;
                    p.random = flag;
                }
                this.dequeue(client);
                final List<Map<String, String>> waiting = new ArrayList<Map<String, String>>();
                for (Waiting q : this.queue) {
                    waiting.add(this.profile(q.client));
                }
                this.respond(client, 9, 0, w -> {
                    w.u8(waiting.size());
                    for (Map<String, String> m : waiting) {
                        w.kv(m);
                    }
                }, ch);
                String back = this.rejoinRoom(client);
                if (back != null) {
                    this.relay.log("casino: " + client.name + " rejoins " + back);
                    List<Waiting> alone = new ArrayList<Waiting>();
                    alone.add(new Waiting(client, ch));
                    this.startMatch(alone, back);
                    break;
                }
                this.queue.add(new Waiting(client, ch));
                this.notifyQueue(client, 9);
                this.matchmake();
                break;
            }
            case 10:    // CASINO_CANCEL_MATCHMAKING
                this.notifyQueue(client, 10);
                this.dequeue(client);
                this.respond(client, 10, 0, w -> { }, ch);
                break;
            default:
                this.respond(client, action, 0, w -> { }, ch);
        }
    }
}

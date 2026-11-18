package rg.relay;

import com.redlynx.mt.snapmobile.Json;
import java.util.ArrayList;
import java.util.HashMap;
import java.util.HashSet;
import java.util.LinkedHashMap;
import java.util.LinkedHashSet;
import java.util.List;
import java.util.Map;
import java.util.Set;
import java.util.concurrent.ScheduledExecutorService;
import java.util.concurrent.ScheduledFuture;
import java.util.concurrent.TimeUnit;
import java.util.function.Consumer;
import java.util.regex.Pattern;

/**
 * Reset Generation Revived multiplayer relay: named rooms + ordered delivery (port of
 * server/src/relay.js, protocol in docs/MULTIPLAYER.md). Runs inside a hosting player's game
 * ({@link LanRelay}); the Node server in server/ is the same relay for online use.
 *
 * Not thread-safe: every call (transport callbacks and timers) must run on the one thread behind
 * {@link #sched}, like the JavaScript original on its event loop.
 */
final class Relay {
    static final int PROTOCOL_VERSION = 1;
    static final int MAX_PACKET = 2048;          // the engine's shared net buffer
    static final int MAX_ROOM_MEMBERS = 4;
    static final int MAX_NAME = 15;
    private static final Pattern NAME_RE = Pattern.compile("^[A-Za-z0-9_]{1,15}$");
    private static final Pattern ROOM_RE = Pattern.compile("^[\\x20-\\x7e]{1,64}$");

    /** A connection's transport (WebSocket). */
    interface Transport {
        void sendText(String s);

        void sendBinary(byte[] b);

        void close(int code, String reason);
    }

    /** Server-side logic of a room (the casino or a game room). */
    interface Bot {
        void join(Client c);

        void leave(Client c);

        void packet(Client c, byte[] bytes);
    }

    static final class Client {
        final String id;
        final Transport transport;
        String name;
        Room room;
        String serves;           // key of the room this client serves as game server
        long lastSeen;
        final Set<Room> greeted = new HashSet<Room>();

        Client(String id, Transport transport) {
            this.id = id;
            this.transport = transport;
        }

        void ctl(Map<String, Object> msg) {
            this.transport.sendText(Json.write(msg));
        }

        void send(byte[] bytes) {
            this.transport.sendBinary(bytes);
        }
    }

    static final class Room {
        final String key, name;
        final Set<Client> members = new LinkedHashSet<Client>();
        final int max;
        final boolean permanent;
        Bot bot;
        ScheduledFuture<?> startTimer;

        Room(String key, String name, int max, boolean permanent) {
            this.key = key;
            this.name = name;
            this.max = max;
            this.permanent = permanent;
        }
    }

    private static final class Expected {
        final int count;
        final long until;

        Expected(int count, long until) {
            this.count = count;
            this.until = until;
        }
    }

    /** Handlers the transport calls for one connection. */
    final class Handle {
        final Client client;

        Handle(Client client) {
            this.client = client;
        }

        void onText(String text) {
            Relay.this.guard(() -> Relay.this.onText(this.client, text));
        }

        void onBinary(byte[] bytes) {
            Relay.this.guard(() -> Relay.this.onBinary(this.client, bytes));
        }

        void onClose() {
            Relay.this.guard(() -> Relay.this.disconnect(this.client));
        }
    }

    final ScheduledExecutorService sched;
    private final Consumer<String> logger;
    private final Map<String, Client> servers = new HashMap<String, Client>();   // room key -> game server
    private final Map<String, Client> clients = new LinkedHashMap<String, Client>();
    private final Map<String, Room> rooms = new LinkedHashMap<String, Room>();
    private final Map<String, Expected> expected = new HashMap<String, Expected>();
    private final Casino casino;
    private int nextId = 1;
    private long packets, bytesIn, connections;

    Relay(ScheduledExecutorService sched, Consumer<String> logger) {
        this.sched = sched;
        this.logger = logger;
        this.casino = new Casino(this);
        // The permanent lobby: the engine joins "CasinoSystem" first and does its matchmaking
        // with the casino bot there, then moves to a game room.
        Room lobby = new Room("casinosystem", "CasinoSystem", 1000, true);
        lobby.bot = this.casino;
        this.rooms.put(lobby.key, lobby);
    }

    void log(String s) {
        this.logger.accept(s);
    }

    private void guard(Runnable r) {
        try {
            r.run();
        } catch (RuntimeException e) {
            this.log("relay error: " + e);
        }
    }

    static Map<String, Object> msg(Object... kv) {
        Map<String, Object> m = new LinkedHashMap<String, Object>();
        for (int i = 0; i + 1 < kv.length; i += 2) {
            m.put((String) kv[i], kv[i + 1]);
        }
        return m;
    }

    /** Number of hosted games waiting for players (LAN discovery answers). */
    int hostedWaiting() {
        return this.casino.hostedWaiting();
    }

    /** Registers a new connection; returns the handlers the transport must call. */
    Handle connect(Transport transport) {
        Client c = new Client("c" + this.nextId++, transport);
        c.lastSeen = System.currentTimeMillis();
        this.clients.put(c.id, c);
        this.connections++;
        return new Handle(c);
    }

    private void onText(Client c, String text) {
        c.lastSeen = System.currentTimeMillis();
        Map<String, Object> m = Json.parseObject(text);
        if (m == null) {
            c.ctl(msg("t", "error", "code", "bad_json"));
            return;
        }
        if (!(m.get("t") instanceof String)) {
            c.ctl(msg("t", "error", "code", "bad_message"));
            return;
        }
        String t = (String) m.get("t");
        if (!t.equals("hello") && !t.equals("ping") && c.name == null) {
            c.ctl(msg("t", "error", "op", t, "code", "not_logged_in"));
            return;
        }
        switch (t) {
            case "hello": this.hello(c, m); break;
            case "ping": c.ctl(msg("t", "pong", "ts", m.get("ts"))); break;
            case "create": this.enter(c, m.get("room"), "create"); break;
            case "join": this.enter(c, m.get("room"), "join"); break;
            case "move": this.enter(c, m.get("room"), "move"); break;
            case "leave": this.leave(c, "left"); break;
            case "serve": this.serve(c, m.get("room"), m.get("players")); break;
            case "unserve":
                if (c.serves != null) {
                    this.unserve(c);
                }
                break;
            case "rooms": c.ctl(msg("t", "rooms", "rooms", this.roomList())); break;
            default: c.ctl(msg("t", "error", "op", t, "code", "unknown_type"));
        }
    }

    private void hello(Client c, Map<String, Object> m) {
        Object v = m.get("v");
        if (!(v instanceof Number) || ((Number) v).doubleValue() != PROTOCOL_VERSION) {
            c.ctl(msg("t", "error", "op", "hello", "code", "version", "server", PROTOCOL_VERSION));
            c.transport.close(4000, "protocol version");
            return;
        }
        Object n = m.get("name");
        if (!(n instanceof String) || !NAME_RE.matcher((String) n).matches()) {
            c.ctl(msg("t", "error", "op", "hello", "code", "bad_name"));
            return;
        }
        // Names are display names only (no accounts); make them unique per server.
        String base = (String) n, name = base;
        Set<String> taken = new HashSet<String>();
        for (Client o : this.clients.values()) {
            if (o != c && o.name != null) {
                taken.add(o.name.toLowerCase());
            }
        }
        for (int i = 2; taken.contains(name.toLowerCase()); i++) {
            String suffix = String.valueOf(i);
            name = base.substring(0, Math.min(base.length(), MAX_NAME - suffix.length())) + suffix;
        }
        c.name = name;
        this.log("hello " + c.id + " as " + name);
        c.ctl(msg("t", "welcome", "v", PROTOCOL_VERSION, "id", c.id, "name", name));
    }

    private static boolean validRoom(Object roomName) {
        return roomName instanceof String && ROOM_RE.matcher((String) roomName).matches();
    }

    /**
     * create: make the room (joins it if it exists and has space); join: only an existing room;
     * move: join if it exists, else create.
     */
    private void enter(Client c, Object roomArg, String op) {
        if (!validRoom(roomArg)) {
            c.ctl(msg("t", "error", "op", op, "code", "bad_room"));
            return;
        }
        String roomName = (String) roomArg;
        String key = roomName.toLowerCase();
        if (c.room != null) {
            if (c.room.key.equals(key)) {
                c.ctl(msg("t", "joined", "op", op, "room", c.room.name, "members", this.memberNames(c.room)));
                return;
            }
            this.leave(c, "left");
        }
        Room room = this.rooms.get(key);
        if (room == null) {
            if (op.equals("join") && !this.expected.containsKey(key)) {
                c.ctl(msg("t", "error", "op", op, "room", roomName, "code", "not_found"));
                return;
            }
            room = new Room(key, roomName, MAX_ROOM_MEMBERS, false);
            Expected exp = this.expected.remove(key);
            if (exp != null) {
                final GameRoom bot = new GameRoom(this, roomName, exp.count);
                room.bot = bot;
                Client server = this.servers.get(key);
                if (server != null) {
                    bot.attachServer(this.serverLink(server));
                }
                // start without the missing players after a while (they would never be greeted)
                room.startTimer = this.sched.schedule(() -> this.guard(bot::start),
                        Math.max(0, exp.until - System.currentTimeMillis()), TimeUnit.MILLISECONDS);
            }
            this.rooms.put(key, room);
            this.log("room + " + roomName);
        } else if (room.members.size() >= room.max) {
            c.ctl(msg("t", "error", "op", op, "room", roomName, "code", "full"));
            return;
        }
        room.members.add(c);
        c.room = room;
        this.log(c.name + " -> " + room.name + " (" + room.members.size() + ")");
        c.ctl(msg("t", "joined", "op", op, "room", room.name, "members", this.memberNames(room)));
        if (room.bot != null) {
            room.bot.join(c);
        } else {
            this.greet(room);
        }
        for (Client o : new ArrayList<Client>(room.members)) {
            if (o != c) {
                o.ctl(msg("t", "peer_joined", "room", room.name, "name", c.name));
            }
        }
    }

    /**
     * A game server process (a host player's engine) offers to serve a matchmade room: it is not
     * a player and is not seated. With `players`, Quick Match fills the room with that many players.
     */
    private void serve(Client c, Object roomArg, Object players) {
        if (!validRoom(roomArg)) {
            c.ctl(msg("t", "error", "op", "serve", "code", "bad_room"));
            return;
        }
        String roomName = (String) roomArg;
        String key = roomName.toLowerCase();
        if (c.room != null) {
            this.leave(c, "left");
        }
        Client other = this.servers.get(key);
        if (other != null && other != c) {
            c.ctl(msg("t", "error", "op", "serve", "room", roomName, "code", "taken"));
            return;
        }
        if (c.serves != null && !c.serves.equals(key)) {
            this.unserve(c);      // a host's next game
        }
        this.servers.put(key, c);
        c.serves = key;
        this.log(c.name + " serves " + roomName);
        c.ctl(msg("t", "serving", "room", roomName));
        Room room = this.rooms.get(key);
        if (room != null && room.bot instanceof GameRoom) {
            ((GameRoom) room.bot).attachServer(this.serverLink(c));
        }
        if (players instanceof Number) {
            double d = ((Number) players).doubleValue();
            if (d == Math.rint(d) && d > 0 && d < 100 && room == null) {
                this.casino.addHosted(roomName, (int) d);
            }
        }
    }

    private GameRoom.ServerLink serverLink(final Client c) {
        return new GameRoom.ServerLink() {
            @Override
            public void ctl(Map<String, Object> m) {
                c.ctl(m);
            }

            @Override
            public void send(byte[] bytes) {
                c.send(bytes);
            }
        };
    }

    private void unserve(Client c) {
        this.casino.removeHosted(c.serves);
        this.servers.remove(c.serves);
        Room room = this.rooms.get(c.serves);
        if (room != null && room.bot instanceof GameRoom) {
            ((GameRoom) room.bot).detachServer();
        }
        c.serves = null;
    }

    /** The matchmaker announces how many players a game room will have. */
    void expectRoom(String name, int count, List<Map<String, String>> profiles) {
        this.expected.put(name.toLowerCase(), new Expected(count, System.currentTimeMillis() + 10000));
    }

    /** Rooms without a bot greet each joiner at once (the engine waits for the server to speak). */
    private void greet(Room room) {
        byte[] hello = Wire.frame(Wire.CH_SYSTEM, new Wire.Writer().u32(20000).bytes());
        for (Client m : room.members) {
            if (m.greeted.add(room)) {
                m.send(hello);
            }
        }
    }

    private void leave(Client c, String reason) {
        Room room = c.room;
        if (room == null) {
            if (reason.equals("left")) {
                c.ctl(msg("t", "left", "room", null));
            }
            return;
        }
        room.members.remove(c);
        c.greeted.remove(room);
        if (room.bot != null) {
            room.bot.leave(c);
        }
        c.room = null;
        if (reason.equals("left")) {
            c.ctl(msg("t", "left", "room", room.name));
        }
        for (Client o : new ArrayList<Client>(room.members)) {
            o.ctl(msg("t", "peer_left", "room", room.name, "name", c.name, "reason", reason));
        }
        if (room.members.isEmpty() && !room.permanent) {
            if (room.startTimer != null) {
                room.startTimer.cancel(false);
            }
            if (room.bot instanceof GameRoom) {
                ((GameRoom) room.bot).close();
            }
            this.rooms.remove(room.key);
            this.log("room - " + room.name);
        }
    }

    private void onBinary(Client c, byte[] bytes) {
        c.lastSeen = System.currentTimeMillis();
        if (c.serves != null) {
            Room room = this.rooms.get(c.serves);
            if (room != null && room.bot instanceof GameRoom) {
                ((GameRoom) room.bot).fromServer(bytes);
            }
            return;
        }
        if (c.room == null) {
            c.ctl(msg("t", "error", "op", "data", "code", "not_in_room"));
            return;
        }
        if (bytes.length == 0 || bytes.length > MAX_PACKET) {
            c.ctl(msg("t", "error", "op", "data", "code", "bad_size"));
            return;
        }
        this.packets++;
        this.bytesIn += bytes.length;
        if (c.room.bot != null) {
            c.room.bot.packet(c, bytes);     // server-side logic, not broadcast
            return;
        }
        for (Client o : new ArrayList<Client>(c.room.members)) {
            if (o != c) {
                o.send(bytes);
            }
        }
    }

    private void disconnect(Client c) {
        if (!this.clients.containsKey(c.id)) {
            return;
        }
        if (c.serves != null) {
            this.unserve(c);
        }
        this.leave(c, "disconnected");
        this.clients.remove(c.id);
        this.log("bye " + c.id + " " + (c.name == null ? "" : c.name));
    }

    /** Closes connections idle for longer than `ms` (no frames, including pings). */
    void reapIdle(long ms) {
        long cutoff = System.currentTimeMillis() - ms;
        for (Client c : new ArrayList<Client>(this.clients.values())) {
            if (c.lastSeen < cutoff) {
                this.disconnect(c);
                try {
                    c.transport.close(4001, "idle");
                } catch (RuntimeException e) {
                    // already gone
                }
            }
        }
    }

    private List<String> memberNames(Room room) {
        List<String> names = new ArrayList<String>();
        for (Client m : room.members) {
            names.add(m.name);
        }
        return names;
    }

    private List<Map<String, Object>> roomList() {
        List<Map<String, Object>> out = new ArrayList<Map<String, Object>>();
        for (Room r : this.rooms.values()) {
            out.add(msg("name", r.name, "players", r.members.size(), "max", r.max, "lobby", r.permanent));
        }
        return out;
    }

    Map<String, Object> status() {
        int open = 0;
        for (Room r : this.rooms.values()) {
            if (!r.permanent) {
                open++;
            }
        }
        return msg("players", this.clients.size(), "rooms", open, "packets", this.packets, "bytes", this.bytesIn,
                "connections", this.connections);
    }
}

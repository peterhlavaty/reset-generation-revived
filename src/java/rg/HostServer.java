package rg;

import java.awt.BasicStroke;
import java.awt.Color;
import java.awt.Font;
import java.awt.Graphics2D;
import java.awt.RenderingHints;
import java.awt.event.KeyEvent;
import java.io.File;
import java.util.ArrayList;
import java.util.List;

/**
 * "Host a game": this player's engine becomes the match's game server (the engine's own LAN
 * host mode) and plays as player 0; the other players join with Quick Match through the relay.
 * The engine-side work is in rghost.dll (native/win32/rghost.c); everything here runs on the
 * engine thread from {@link ResetGeneration#tickRG()}.
 *
 * F5 in the main menu shows the player-count picker (drawn over the menu, remembered as
 * host_players in revived.ini), then the engine's hero select (host_hero=NAME / RANDOM skips it).
 * With server=auto (default) the game starts its built-in relay ({@link rg.relay.LanRelay}) and
 * the others find it on the LAN by themselves. Or automatically with host_players=N plus
 * host=auto / --host=auto (hero: host_hero, default random). Test aid: --keys=T:KEY,... sends
 * key presses T milliseconds after start (KEY = ESC, RETURN, UP, DOWN, LEFT, RIGHT, SPACE,
 * F5 or a hex virtual-key code); they go to the picker while it is shown.
 */
final class HostServer {
    private static boolean loaded, installed, hosting;
    private static volatile boolean requested;
    /** Player-count picker: 0 = hidden, else the count shown (2..4). */
    private static volatile int picker;
    private static volatile boolean pickDone, pickCancelled;
    /** Key whose typed / released events must not reach the engine after the picker closed. */
    private static volatile int swallow;
    private static String lanAddress;
    private static long start, nextAuto, lastNudge;
    private static int games, wanted, shownPlayers = -1;
    /** All players had joined: the match is running (a player who drops does not reopen it). */
    private static boolean matchStarted;
    private static final List<long[]> keys = new ArrayList<long[]>();
    /** --perflog=N: every N seconds print the tick rate, the slowest tick and the Java heap. */
    private static long perfEvery = -1, perfStart, perfTicks, perfLast, perfWorst;

    static native boolean install();
    static native boolean chdir(String dir);
    static native boolean menuReady();
    static native int state();          // 0 idle, 1 choosing a hero, 2 serving
    static native int players();
    static native boolean host(String url, String room, int players, String name, int hero);

    /** Engine hero ids (0x100941c0). */
    private static final String[] HEROES = {"HEDGEHOG", "PLUMBER", "CYBORG", "LEVEL_50_ELF", "BABE_GUNNER",
            "MONSTER_TRAINER", "SCIFI_KNIGHT", "NINJA", "DR_LOVEBOMBER", "AGGRESSOR"};

    /** host_hero option: a hero name or id -> 0..9, RANDOM -> -1, anything else -> dflt. */
    static int heroOption(int dflt) {
        String v = Revived.opt("host_hero", "").trim().toUpperCase();
        if (v.equals("RANDOM")) return -1;
        for (int i = 0; i < HEROES.length; i++) {
            if (HEROES[i].equals(v) || String.valueOf(i).equals(v)) return i;
        }
        return dflt;
    }
    static native void pump();

    private HostServer() {
    }

    private static void perf() {
        if (perfEvery < 0) {
            perfEvery = Revived.optInt("perflog", 0) * 1000L;
        }
        if (perfEvery == 0) {
            return;
        }
        long now = System.currentTimeMillis();
        if (perfStart == 0) {
            perfStart = perfLast = now;
        }
        perfWorst = Math.max(perfWorst, now - perfLast);
        perfLast = now;
        perfTicks++;
        if (now - perfStart >= perfEvery) {
            Runtime r = Runtime.getRuntime();
            System.out.printf("perf: %.1f ticks/s, slowest %d ms, heap %d/%d MB, threads %d%n",
                    perfTicks * 1000.0 / (now - perfStart), perfWorst, (r.totalMemory() - r.freeMemory()) >> 20,
                    r.maxMemory() >> 20, Thread.activeCount());
            perfStart = now;
            perfTicks = 0;
            perfWorst = 0;
        }
    }

    /** F5 (AWT thread): host at the next engine tick. */
    static void request() {
        requested = true;
    }

    static boolean isHosting() {
        return hosting;
    }

    private static int dll;   // 0 not tried, 1 loaded, -1 missing

    private static boolean load() {
        if (dll == 0) {
            dll = loadDll() ? 1 : -1;
        }
        return dll > 0;
    }

    /**
     * The home directory to give the engine. Its file API is ANSI and gets the path as UTF-8, so a
     * game folder with non-ASCII characters (a user name like "Hlavatý") makes it crash: then that
     * folder becomes the process's current directory and the engine gets a relative path.
     */
    static String engineHome(String home) {
        boolean ascii = true;
        for (int i = 0; i < home.length(); i++) {
            ascii &= home.charAt(i) < 0x80;
        }
        if (ascii) {
            return home;
        }
        try {
            if (load() && chdir(new File(home).getAbsolutePath())) {
                System.out.println("HostServer: non-ASCII game folder, the engine uses it as the current directory");
                return home.endsWith("\\") || home.endsWith("/") ? ".\\" : ".";
            }
        } catch (UnsatisfiedLinkError e) {
            System.out.println("HostServer: rghost.dll is too old for a non-ASCII game folder");
        }
        return home;
    }

    private static boolean loadDll() {
        List<File> tries = new ArrayList<File>();
        String prop = System.getProperty("rg.hostdll");
        if (prop != null) tries.add(new File(prop));
        if (Revived.configFile != null) tries.add(new File(Revived.configFile.getParentFile(), "rghost.dll"));
        tries.add(new File("rghost.dll"));
        for (File f : tries) {
            if (f.isFile()) {
                try {
                    System.load(f.getAbsolutePath());
                    System.out.println("HostServer: loaded " + f);
                    return true;
                } catch (Throwable t) {
                    System.out.println("HostServer: cannot load " + f + ": " + t);
                }
            }
        }
        System.out.println("HostServer: rghost.dll not found, hosting disabled");
        return false;
    }

    private static void parseKeys(String spec) {
        if (spec == null || spec.isEmpty()) return;
        for (String part : spec.split(",")) {
            String[] tk = part.trim().split(":", 2);
            if (tk.length != 2) continue;
            int vk;
            String k = tk[1].trim().toUpperCase();
            switch (k) {
                case "ESC": vk = 0x1b; break;
                case "RETURN": case "ENTER": vk = 0x0a; break;
                case "SPACE": vk = 0x20; break;
                case "LEFT": vk = 0x25; break;
                case "UP": vk = 0x26; break;
                case "RIGHT": vk = 0x27; break;
                case "DOWN": vk = 0x28; break;
                case "F5": vk = -5; break;
                default: vk = Integer.parseInt(k, 16);
            }
            keys.add(new long[]{Long.parseLong(tk[0].trim()), vk});
        }
    }

    static int clampPlayers(int n) {
        return Math.max(2, Math.min(4, n));
    }

    /** Starts the built-in relay if needed (server=auto) and hosts; false if not possible now. */
    private static boolean startHosting(int players, int hero) {
        String nick = com.redlynx.mt.snapmobile.SNAPMobileClient.nickname;
        String url = com.redlynx.mt.snapmobile.SNAPMobileClient.relayUrl;
        if ("auto".equalsIgnoreCase(url)) {
            url = rg.relay.LanRelay.start(nick);
            if (url == null) {
                Revived.setStatus("cannot host: port " + rg.relay.LanRelay.PORT + " is used by another program");
                return false;
            }
            lanAddress = rg.relay.LanRelay.lanAddress();
        }
        String room = "HOST_" + (nick == null ? "game" : nick) + "_" + (++games);
        if (!host(url, room, players, nick == null ? "" : nick, hero)) {
            System.out.println("HostServer: cannot host now (already hosting, or a hosted match is running)");
            return false;
        }
        hosting = true;
        wanted = players - 1;
        shownPlayers = -1;
        matchStarted = false;
        System.out.println("HostServer: hosting " + room + " for " + players + " players"
                + (hero == -2 ? " (choosing a hero)" : hero >= 0 ? " as " + HEROES[hero] : ""));
        return true;
    }

    // ------------------------------------------------------------------ player-count picker

    /**
     * AWT thread (Revived's key dispatcher): true if the key event belongs to the picker and must
     * not reach the engine.
     */
    static boolean pickerEvent(KeyEvent e) {
        int code = e.getKeyCode();
        // The key that closed the picker stays ours until released: its auto-repeat presses and
        // typed events would otherwise reach the engine (Esc opens Quit, Enter picks a hero).
        if (swallow != 0 && (code == swallow || e.getID() == KeyEvent.KEY_TYPED)) {
            if (e.getID() == KeyEvent.KEY_RELEASED) {
                swallow = 0;
            }
            return true;
        }
        if (picker == 0) {
            return false;
        }
        if (e.getID() == KeyEvent.KEY_PRESSED) {
            pickerKey(code);
            if (picker == 0 || pickDone) {
                swallow = code;
            }
        }
        return true;
    }

    /** One key press in the picker: arrows change the count, Enter continues, Esc cancels. */
    static void pickerKey(int vk) {
        int n = picker;
        if (n == 0 || pickDone) {
            return;
        }
        switch (vk) {
            case KeyEvent.VK_LEFT: case KeyEvent.VK_DOWN: n--; break;
            case KeyEvent.VK_RIGHT: case KeyEvent.VK_UP: n++; break;
            case KeyEvent.VK_2: case KeyEvent.VK_3: case KeyEvent.VK_4: n = vk - KeyEvent.VK_0; break;
            case KeyEvent.VK_NUMPAD2: case KeyEvent.VK_NUMPAD3: case KeyEvent.VK_NUMPAD4: n = vk - KeyEvent.VK_NUMPAD0; break;
            case KeyEvent.VK_ENTER: case KeyEvent.VK_SPACE:
                pickDone = true;
                return;
            case KeyEvent.VK_ESCAPE: case KeyEvent.VK_BACK_SPACE:
                picker = 0;
                pickCancelled = true;
                return;
            default:
                return;
        }
        picker = clampPlayers(n);
    }

    /** Draws the picker over the engine frame (w x h engine pixels). */
    static void drawPicker(Graphics2D g, int w, int h) {
        int n = picker;
        if (n == 0 || g == null) {
            return;
        }
        Graphics2D g2 = (Graphics2D) g.create();
        try {
            g2.setRenderingHint(RenderingHints.KEY_TEXT_ANTIALIASING, RenderingHints.VALUE_TEXT_ANTIALIAS_ON);
            g2.setRenderingHint(RenderingHints.KEY_ANTIALIASING, RenderingHints.VALUE_ANTIALIAS_ON);
            int u = Math.max(2, h / 90);                 // layout unit: 4 px at 360 rows
            int bw = Math.min(w - 4 * u, 100 * u), bh = 58 * u;
            int x = (w - bw) / 2, y = (h - bh) / 2;
            Color gold = new Color(255, 200, 40);
            g2.setColor(new Color(0, 0, 0, 170));
            g2.fillRect(0, 0, w, h);
            g2.setColor(new Color(24, 32, 64));
            g2.fillRoundRect(x, y, bw, bh, 4 * u, 4 * u);
            g2.setColor(gold);
            g2.setStroke(new BasicStroke(Math.max(1f, u / 2f)));
            g2.drawRoundRect(x, y, bw, bh, 4 * u, 4 * u);
            centered(g2, "HOST A GAME", new Font("Arial", Font.BOLD, 5 * u), w / 2, y + 9 * u);
            g2.setColor(Color.WHITE);
            centered(g2, "Number of players", new Font("Arial", Font.PLAIN, 4 * u), w / 2, y + 17 * u);
            int cy = y + 29 * u;
            centered(g2, String.valueOf(n), new Font("Arial", Font.BOLD, 12 * u), w / 2, cy + 4 * u);
            g2.setColor(gold);
            if (n > 2) {
                triangle(g2, w / 2 - 14 * u, cy, -1, 3 * u);
            }
            if (n < 4) {
                triangle(g2, w / 2 + 14 * u, cy, 1, 3 * u);
            }
            g2.setColor(new Color(200, 210, 255));
            Font small = new Font("Arial", Font.PLAIN, 3 * u);
            centered(g2, "you + " + (n - 1) + (n == 2 ? " other player" : " other players") + " (Quick Match on their PCs)",
                    small, w / 2, y + 45 * u);
            centered(g2, "Left/Right: change    Enter: continue    Esc: cancel", small, w / 2, y + 52 * u);
        } finally {
            g2.dispose();
        }
    }

    private static void centered(Graphics2D g, String s, Font f, int cx, int baseline) {
        g.setFont(f);
        g.drawString(s, cx - g.getFontMetrics().stringWidth(s) / 2, baseline);
    }

    private static void triangle(Graphics2D g, int cx, int cy, int dir, int r) {
        g.fillPolygon(new int[]{cx + dir * r, cx - dir * r, cx - dir * r}, new int[]{cy, cy - r, cy + r}, 3);
    }


    /** Called once per engine tick, after the engine's own tick. */
    static void tick(ResetGeneration rg) {
        perf();
        if (!loaded) {
            loaded = true;
            start = System.currentTimeMillis();
            parseKeys(Revived.opt("keys", ""));
            installed = load() && install();
        }
        long t = System.currentTimeMillis() - start;
        while (!keys.isEmpty() && keys.get(0)[0] <= t) {
            int vk = (int) keys.remove(0)[1];
            System.out.println("HostServer: key " + vk + " at " + t + " ms");
            if (vk == -5) requested = true;
            else if (picker != 0) pickerKey(vk);
            else rg.injectKey(vk);
        }
        if (!installed) return;
        boolean auto = Revived.opt("host", "").equalsIgnoreCase("auto") && t > 15000;
        if (requested) {
            requested = false;
            if (!hosting && picker == 0 && menuReady()) {
                picker = clampPlayers(Revived.optInt("host_players", 2));
                Revived.setStatus("host a game: number of players (Enter = continue, Esc = cancel)");
            } else if (picker == 0) {
                System.out.println("HostServer: F5 ignored (not in the main menu, or already hosting)");
            }
        }
        if (pickCancelled) {
            pickCancelled = false;
            Revived.setStatus(null);
        }
        if (pickDone) {
            int players = picker;
            picker = 0;
            pickDone = false;
            Revived.config.setProperty("host_players", String.valueOf(players));
            System.clearProperty("rg.host_players");   // the choice beats a --host_players argument
            Revived.saveConfig();
            startHosting(players, heroOption(-2));
        } else if (auto && !hosting && picker == 0 && t >= nextAuto && menuReady()) {
            if (!startHosting(clampPlayers(Revived.optInt("host_players", 2)), heroOption(-1))) {
                nextAuto = t + 2000;
            }
        }
        pump();
        int st = hosting ? state() : 0;
        // The main menu replays the intro after ~35 s without input, and the picker keeps the keys
        // from the engine: tap Right (does nothing in the main menu; Shift would be Back) when the
        // picker opens and every 10 s.
        if (picker != 0 && t - lastNudge > 10000) {
            lastNudge = t;
            rg.injectKey(KeyEvent.VK_RIGHT);
        }
        if (hosting && st == 0) {      // match over, hero select or host screen left
            hosting = false;
            shownPlayers = -1;
            System.out.println("HostServer: hosted game over");
            Revived.setStatus("F5 = host a new game");
        }
        if (hosting && st == 1) {
            if (shownPlayers != -2) {
                shownPlayers = -2;
                Revived.setStatus("hosting: choose your hero (Esc = cancel)");
            }
        } else if (hosting) {
            int n = players();
            if (n >= wanted && !matchStarted) {
                matchStarted = true;
                shownPlayers = -1;
            }
            if (n != shownPlayers) {
                shownPlayers = n;
                if (matchStarted) {
                    Revived.setStatus(n < wanted ? "hosting: match running, " + n + "/" + wanted + " players still connected"
                            : "hosting: match running");
                } else {
                    Revived.setStatus("hosting" + (lanAddress == null ? "" : " on " + lanAddress)
                            + ", waiting for players " + n + "/" + wanted + " (others: Quick Match)");
                }
            }
        }
    }
}

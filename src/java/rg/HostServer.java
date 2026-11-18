package rg;

import java.io.File;
import java.util.ArrayList;
import java.util.List;

/**
 * "Host a game": this player's engine becomes the match's game server (the engine's own LAN
 * host mode) and plays as player 0; the other players join with Quick Match through the relay.
 * The engine-side work is in rghost.dll (native/win32/rghost.c); everything here runs on the
 * engine thread from {@link ResetGeneration#tickRG()}.
 *
 * Start hosting with F5 in the main menu (players = host_players in revived.ini, default 2): the
 * host first picks a hero in the engine's hero select (or host_hero=NAME / RANDOM skips it). Or
 * automatically with host_players=N plus host=auto / --host=auto (hero: host_hero, default
 * random). Test aid: --keys=T:KEY,... sends
 * key presses T milliseconds after start (KEY = ESC, RETURN, UP, DOWN, LEFT, RIGHT, SPACE,
 * F5 or a hex virtual-key code).
 */
final class HostServer {
    private static boolean loaded, installed, hosting;
    private static volatile boolean requested;
    private static long start, nextAuto;
    private static int games, wanted, shownPlayers = -1;
    private static final List<long[]> keys = new ArrayList<long[]>();

    static native boolean install();
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

    /** F5 (AWT thread): host at the next engine tick. */
    static void request() {
        requested = true;
    }

    static boolean isHosting() {
        return hosting;
    }

    private static boolean load() {
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

    /** Called once per engine tick, after the engine's own tick. */
    static void tick(ResetGeneration rg) {
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
            else rg.injectKey(vk);
        }
        if (!installed) return;
        boolean auto = Revived.opt("host", "").equalsIgnoreCase("auto") && t > 15000;
        if ((requested || (auto && !hosting && t >= nextAuto)) && menuReady()) {
            boolean byKey = requested;
            requested = false;
            int players = Math.max(2, Math.min(4, Revived.optInt("host_players", 2)));
            String nick = com.redlynx.mt.snapmobile.SNAPMobileClient.nickname;
            String room = "HOST_" + (nick == null ? "game" : nick) + "_" + (++games);
            int hero = heroOption(byKey ? -2 : -1);
            if (host(com.redlynx.mt.snapmobile.SNAPMobileClient.relayUrl, room, players, nick == null ? "" : nick, hero)) {
                hosting = true;
                wanted = players - 1;
                shownPlayers = -1;
                System.out.println("HostServer: hosting " + room + " for " + players + " players"
                        + (hero == -2 ? " (choosing a hero)" : hero >= 0 ? " as " + HEROES[hero] : ""));
            } else {
                nextAuto = t + 2000;
                System.out.println("HostServer: cannot host now (already hosting, or a hosted match is running)");
            }
        }
        pump();
        int st = hosting ? state() : 0;
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
            if (n != shownPlayers) {
                shownPlayers = n;
                Revived.setStatus(n < wanted ? "hosting, waiting for players " + n + "/" + wanted + " (others: Quick Match)" : "hosting");
            }
        }
    }
}

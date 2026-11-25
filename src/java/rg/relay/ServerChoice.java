package rg.relay;

import java.net.URI;

/**
 * Which relay a game uses (revived.ini {@code server=}):
 * <ul>
 * <li>{@code auto} (default): a game hosted on this network if there is one, else the public
 * server; a host uses the public server when it is reachable, else its own LAN relay;</li>
 * <li>{@code online}: always the public server ({@link #ONLINE});</li>
 * <li>{@code lan}: only this network (the host's game runs the relay, {@link LocalRelay});</li>
 * <li>a WebSocket address ({@code ws://host:port/ws}): that relay.</li>
 * </ul>
 */
public final class ServerChoice {
    /** The public server (server/ deployed by rgtool; plain ws: every client speaks it). */
    public static final String ONLINE = "ws://resetgeneration.rustyapps.com/ws";
    private static final long LAN_FIND_MS = 20000;
    private static final long AUTO_FIND_MS = 2500;

    private ServerChoice() {
    }

    static boolean isMode(String s, String mode) {
        return s == null || s.isEmpty() ? "auto".equals(mode) : s.equalsIgnoreCase(mode);
    }

    /** Where Quick Match connects; null if no relay can be found. */
    public static String forJoining(String setting) {
        if (isMode(setting, "online")) {
            return ONLINE;
        }
        if (isMode(setting, "lan")) {
            return LanDiscovery.find(LAN_FIND_MS);
        }
        if (isMode(setting, "auto")) {
            String lan = LanDiscovery.find(AUTO_FIND_MS);
            return lan != null ? lan : ONLINE;
        }
        return setting;
    }

    /** Where a hosted game (F5) is served; null if no relay can be used. */
    public static String forHosting(String setting, String nick) {
        if (isMode(setting, "online")) {
            return ONLINE;
        }
        if (isMode(setting, "auto") && reachable(ONLINE)) {
            return ONLINE;
        }
        if (isMode(setting, "lan") || isMode(setting, "auto")) {
            return LocalRelay.start(nick);
        }
        return setting;
    }

    /** True for a relay on this PC or the LAN (the window shows the LAN address then). */
    public static boolean isLocal(String url) {
        return url != null && url.startsWith("ws://127.0.0.1:");
    }

    /** Whether the relay at this address answers (GET /status, 1-2 s). */
    static boolean reachable(String url) {
        try {
            URI u = URI.create(url);
            int port = u.getPort() > 0 ? u.getPort() : "wss".equalsIgnoreCase(u.getScheme()) ? 443 : 80;
            return !"wss".equalsIgnoreCase(u.getScheme()) && LanDiscovery.isRelay(u.getHost(), port);
        } catch (RuntimeException e) {
            return false;
        }
    }
}

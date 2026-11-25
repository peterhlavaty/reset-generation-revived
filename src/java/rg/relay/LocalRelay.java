package rg.relay;

import java.io.File;
import java.net.Inet4Address;
import java.net.InetAddress;
import java.net.NetworkInterface;
import java.util.ArrayList;
import java.util.Collections;
import java.util.List;

/**
 * The relay a hosting player's game runs for LAN play: the bundled rg-relay executable (the same
 * Rust server as the public one, server/), started with --lan so the other games on the network
 * find it ({@link LanDiscovery}). It listens on TCP + UDP {@link #PORT} and exits with the game
 * (--exit-with-parent: its stdin is a pipe from this JVM).
 */
public final class LocalRelay {
    public static final int PORT = 8787;
    private static Process running;

    private LocalRelay() {
    }

    /**
     * Starts the relay once per game session (later calls return the same address). Returns the
     * WebSocket address for this PC, or null if it cannot run (no executable, port taken by
     * something that is not a relay).
     */
    public static synchronized String start(String hostName) {
        String local = "ws://127.0.0.1:" + PORT + "/ws";
        if (running != null && running.isAlive()) {
            return local;
        }
        if (LanDiscovery.isRelay("127.0.0.1", PORT)) {
            return local;                        // another game on this PC already runs one
        }
        File exe = executable();
        if (exe == null) {
            System.out.println("LocalRelay: rg-relay executable not found");
            return null;
        }
        String name = hostName == null || hostName.isEmpty() ? "Host" : hostName;
        try {
            ProcessBuilder pb = new ProcessBuilder(exe.getAbsolutePath(), "--lan", "--host-name", name,
                    "--port", Integer.toString(PORT), "--exit-with-parent");
            pb.redirectOutput(ProcessBuilder.Redirect.INHERIT);
            pb.redirectError(ProcessBuilder.Redirect.INHERIT);
            running = pb.start();                // stdin stays a pipe: closes when this JVM ends
        } catch (Exception e) {
            System.out.println("LocalRelay: cannot start " + exe + ": " + e);
            return null;
        }
        for (int i = 0; i < 30; i++) {           // up to 3 s to listen
            if (!running.isAlive()) {
                System.out.println("LocalRelay: rg-relay exited (" + running.exitValue() + "): port " + PORT + " in use?");
                running = null;
                return null;
            }
            if (LanDiscovery.isRelay("127.0.0.1", PORT)) {
                return local;
            }
            try {
                Thread.sleep(100);
            } catch (InterruptedException e) {
                break;
            }
        }
        return local;
    }

    /** rg-relay(.exe) next to the game: the app folder, its parent or the working directory. */
    private static File executable() {
        String name = System.getProperty("os.name", "").startsWith("Windows") ? "rg-relay.exe" : "rg-relay";
        List<File> tries = new ArrayList<File>();
        try {
            File jar = new File(LocalRelay.class.getProtectionDomain().getCodeSource().getLocation().toURI());
            File dir = jar.isFile() ? jar.getParentFile() : jar;
            tries.add(new File(dir, name));
            if (dir.getParentFile() != null) {
                tries.add(new File(dir.getParentFile(), name));
            }
        } catch (Exception e) {
            // unknown location: try the working directory
        }
        String extra = System.getProperty("rg.relay");
        if (extra != null) {
            tries.add(0, new File(extra));
        }
        tries.add(new File(name));
        for (File f : tries) {
            if (f.isFile()) {
                return f;
            }
        }
        return null;
    }

    /** IPv4 address of this PC on the LAN (for the window title). */
    public static String lanAddress() {
        try {
            for (NetworkInterface ni : Collections.list(NetworkInterface.getNetworkInterfaces())) {
                if (!ni.isUp() || ni.isLoopback() || ni.isVirtual()) {
                    continue;
                }
                for (InetAddress a : Collections.list(ni.getInetAddresses())) {
                    if (a instanceof Inet4Address && a.isSiteLocalAddress()) {
                        return a.getHostAddress();
                    }
                }
            }
        } catch (Exception e) {
            // no address to show
        }
        return null;
    }
}

package rg.relay;

import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.net.DatagramPacket;
import java.net.DatagramSocket;
import java.net.InetAddress;
import java.net.InetSocketAddress;
import java.net.InterfaceAddress;
import java.net.NetworkInterface;
import java.net.Socket;
import java.net.SocketTimeoutException;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.Collections;
import java.util.LinkedHashSet;
import java.util.List;
import java.util.Set;

/**
 * Finds a hosting game on the LAN (server=auto): broadcasts "RGR-DISCOVER 1" to UDP 8787 on every
 * network and this PC, and connects to whoever answers ({@link LanRelay}). Plain JDK sockets, so
 * joining players never load Netty. Windows lets the answer to our own broadcast in without a
 * firewall prompt; only the hosting PC has to allow the game once.
 */
public final class LanDiscovery {
    private LanDiscovery() {
    }

    /**
     * Waits up to timeoutMs for a host; returns its relay address (ws://ip:port/ws) or null. A host
     * that is waiting for players wins over one that is not (several hosts on one network).
     */
    public static String find(long timeoutMs) {
        long end = System.currentTimeMillis() + timeoutMs;
        String any = null;
        long settle = Long.MAX_VALUE;
        byte[] probe = LanRelay.DISCOVER.getBytes(StandardCharsets.US_ASCII);
        try (DatagramSocket s = new DatagramSocket()) {
            s.setBroadcast(true);
            s.setSoTimeout(200);
            List<InetAddress> targets = targets();
            long nextProbe = 0;
            byte[] buf = new byte[256];
            while (System.currentTimeMillis() < Math.min(end, settle)) {
                long now = System.currentTimeMillis();
                if (now >= nextProbe) {
                    nextProbe = now + 1000;
                    for (InetAddress a : targets) {
                        try {
                            s.send(new DatagramPacket(probe, probe.length, a, LanRelay.PORT));
                        } catch (IOException e) {
                            // network without broadcast: skip
                        }
                    }
                }
                DatagramPacket in = new DatagramPacket(buf, buf.length);
                try {
                    s.receive(in);
                } catch (SocketTimeoutException e) {
                    continue;
                } catch (IOException e) {
                    continue;        // e.g. ICMP port unreachable from this PC
                }
                String[] f = new String(in.getData(), 0, in.getLength(), StandardCharsets.US_ASCII).trim().split(" ", 5);
                if (f.length < 4 || !(f[0] + " " + f[1]).equals(LanRelay.ANSWER)) {
                    continue;
                }
                int port, waiting;
                try {
                    port = Integer.parseInt(f[2]);
                    waiting = Integer.parseInt(f[3]);
                } catch (NumberFormatException e) {
                    continue;
                }
                String url = "ws://" + in.getAddress().getHostAddress() + ":" + port + "/ws";
                String who = f.length > 4 ? f[4] : "?";
                if (waiting > 0) {
                    System.out.println("LanDiscovery: " + who + " hosts at " + url);
                    return url;
                }
                if (any == null) {
                    any = url;
                    System.out.println("LanDiscovery: " + who + " runs a relay at " + url + " (no game waiting yet)");
                    settle = System.currentTimeMillis() + 1500;      // a waiting host may still answer
                }
            }
        } catch (IOException | RuntimeException e) {
            System.out.println("LanDiscovery: " + e);
        }
        return any;
    }

    /** Broadcast addresses of every network, the limited broadcast and this PC. */
    private static List<InetAddress> targets() throws IOException {
        Set<InetAddress> out = new LinkedHashSet<InetAddress>();
        for (NetworkInterface ni : Collections.list(NetworkInterface.getNetworkInterfaces())) {
            try {
                if (!ni.isUp() || ni.isLoopback()) {
                    continue;
                }
                for (InterfaceAddress ia : ni.getInterfaceAddresses()) {
                    if (ia.getBroadcast() != null) {
                        out.add(ia.getBroadcast());
                    }
                }
            } catch (IOException e) {
                // skip this interface
            }
        }
        out.add(InetAddress.getByName("255.255.255.255"));
        out.add(InetAddress.getByName("127.0.0.1"));
        return new ArrayList<InetAddress>(out);
    }

    /** Whether host:port answers GET /status like a relay (1 s timeout). */
    static boolean isRelay(String host, int port) {
        try (Socket s = new Socket()) {
            s.connect(new InetSocketAddress(host, port), 1000);
            s.setSoTimeout(1000);
            OutputStream o = s.getOutputStream();
            o.write(("GET /status HTTP/1.0\r\nHost: " + host + "\r\n\r\n").getBytes(StandardCharsets.US_ASCII));
            o.flush();
            InputStream in = s.getInputStream();
            byte[] buf = new byte[2048];
            int n, got = 0;
            while (got < buf.length && (n = in.read(buf, got, buf.length - got)) > 0) {
                got += n;
            }
            String r = new String(buf, 0, got, StandardCharsets.US_ASCII);
            return r.startsWith("HTTP/1.") && r.contains("\"players\"");
        } catch (IOException e) {
            return false;
        }
    }
}

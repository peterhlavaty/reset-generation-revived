package rg.relay;

import com.redlynx.mt.snapmobile.Json;
import io.netty.bootstrap.Bootstrap;
import io.netty.bootstrap.ServerBootstrap;
import io.netty.buffer.ByteBuf;
import io.netty.buffer.ByteBufUtil;
import io.netty.buffer.Unpooled;
import io.netty.channel.Channel;
import io.netty.channel.ChannelFutureListener;
import io.netty.channel.ChannelHandlerContext;
import io.netty.channel.ChannelInitializer;
import io.netty.channel.ChannelOption;
import io.netty.channel.EventLoopGroup;
import io.netty.channel.SimpleChannelInboundHandler;
import io.netty.channel.nio.NioEventLoopGroup;
import io.netty.channel.socket.DatagramPacket;
import io.netty.channel.socket.SocketChannel;
import io.netty.channel.socket.nio.NioDatagramChannel;
import io.netty.channel.socket.nio.NioServerSocketChannel;
import io.netty.handler.codec.http.DefaultFullHttpResponse;
import io.netty.handler.codec.http.FullHttpRequest;
import io.netty.handler.codec.http.FullHttpResponse;
import io.netty.handler.codec.http.HttpHeaderNames;
import io.netty.handler.codec.http.HttpObjectAggregator;
import io.netty.handler.codec.http.HttpResponseStatus;
import io.netty.handler.codec.http.HttpServerCodec;
import io.netty.handler.codec.http.HttpVersion;
import io.netty.handler.codec.http.websocketx.BinaryWebSocketFrame;
import io.netty.handler.codec.http.websocketx.CloseWebSocketFrame;
import io.netty.handler.codec.http.websocketx.TextWebSocketFrame;
import io.netty.handler.codec.http.websocketx.WebSocketFrameAggregator;
import io.netty.handler.codec.http.websocketx.WebSocketServerProtocolConfig;
import io.netty.handler.codec.http.websocketx.WebSocketServerProtocolHandler;
import io.netty.util.AttributeKey;
import io.netty.util.concurrent.DefaultThreadFactory;
import java.net.Inet4Address;
import java.net.InetAddress;
import java.net.InetSocketAddress;
import java.net.NetworkInterface;
import java.nio.charset.StandardCharsets;
import java.text.SimpleDateFormat;
import java.util.Collections;
import java.util.Date;
import java.util.concurrent.TimeUnit;

/**
 * The relay built into the game (Netty): a hosting player's game runs it, so LAN play needs no
 * separate server. WebSocket at ws://&lt;this PC&gt;:8787/ws (GET /status: JSON counters), plus the
 * LAN discovery responder on UDP 8787 ({@link LanDiscovery}). One event-loop thread runs the
 * network and the relay logic, so the relay itself needs no locking.
 */
public final class LanRelay {
    public static final int PORT = 8787;
    static final String DISCOVER = "RGR-DISCOVER 1";
    static final String ANSWER = "RGR-HOST 1";
    private static final long IDLE_MS = 60000;      // clients ping every 15 s
    private static final AttributeKey<Relay.Handle> HANDLE = AttributeKey.valueOf("rg.relay.handle");

    private static LanRelay running;

    private final EventLoopGroup group;
    private final Relay relay;
    private final String hostName;
    private Channel tcp, udp;

    private LanRelay(String hostName) {
        if (System.getProperty("io.netty.allocator.type") == null) {
            System.setProperty("io.netty.allocator.type", "unpooled");   // tiny traffic, 32-bit JVM
        }
        this.hostName = hostName == null || hostName.isEmpty() ? "Host" : hostName;
        this.group = new NioEventLoopGroup(1, new DefaultThreadFactory("lan-relay", true));
        final SimpleDateFormat time = new SimpleDateFormat("HH:mm:ss.SSS");
        this.relay = new Relay(this.group.next(), s -> System.out.println(time.format(new Date()) + " relay: " + s));
    }

    /**
     * Starts the relay once per game session (later calls return the same address). Returns the
     * WebSocket address for this PC, or null if port 8787 is taken by something that is not a relay.
     */
    public static synchronized String start(String hostName) {
        String local = "ws://127.0.0.1:" + PORT + "/ws";
        if (running != null) {
            return local;
        }
        LanRelay r = new LanRelay(hostName);
        try {
            r.bind();
        } catch (Exception e) {
            r.group.shutdownGracefully(0, 1, TimeUnit.SECONDS);
            // Another game on this PC (or the Node relay) may already serve this port: use it.
            System.out.println("LanRelay: cannot listen on port " + PORT + " (" + e + ")");
            return LanDiscovery.isRelay("127.0.0.1", PORT) ? local : null;
        }
        running = r;
        return local;
    }

    /** IPv4 addresses of this PC on the LAN (for the window title). */
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

    private void bind() throws InterruptedException {
        ServerBootstrap b = new ServerBootstrap()
                .group(this.group)
                .channel(NioServerSocketChannel.class)
                .childOption(ChannelOption.TCP_NODELAY, true)
                .childHandler(new ChannelInitializer<SocketChannel>() {
                    @Override
                    protected void initChannel(SocketChannel ch) {
                        ch.pipeline().addLast(
                                new HttpServerCodec(),
                                new HttpObjectAggregator(8192),
                                new WebSocketServerProtocolHandler(WebSocketServerProtocolConfig.newBuilder()
                                        .websocketPath("/ws")
                                        .maxFramePayloadLength(Relay.MAX_PACKET + 1024)
                                        .handshakeTimeoutMillis(10000)
                                        .build()),
                                new WebSocketFrameAggregator(Relay.MAX_PACKET + 1024),
                                new Connection());
                    }
                });
        this.tcp = b.bind(new InetSocketAddress(PORT)).sync().channel();
        System.out.println("LanRelay: listening on ws://0.0.0.0:" + PORT + "/ws");
        this.group.scheduleAtFixedRate(() -> this.relay.reapIdle(IDLE_MS), 10, 10, TimeUnit.SECONDS);
        try {
            this.udp = new Bootstrap()
                    .group(this.group)
                    .channel(NioDatagramChannel.class)
                    .option(ChannelOption.SO_BROADCAST, true)
                    .handler(new Discovery())
                    .bind(new InetSocketAddress(PORT)).sync().channel();
            System.out.println("LanRelay: answering LAN discovery on UDP " + PORT);
        } catch (Exception e) {
            System.out.println("LanRelay: no LAN discovery (UDP " + PORT + "): " + e);
        }
    }

    /** One WebSocket connection (or a plain HTTP request for / and /status). */
    private final class Connection extends SimpleChannelInboundHandler<Object> {
        @Override
        public void userEventTriggered(ChannelHandlerContext ctx, Object evt) throws Exception {
            if (evt instanceof WebSocketServerProtocolHandler.HandshakeComplete) {
                final Channel ch = ctx.channel();
                ch.attr(HANDLE).set(LanRelay.this.relay.connect(new Relay.Transport() {
                    @Override
                    public void sendText(String s) {
                        if (ch.isActive()) {
                            ch.writeAndFlush(new TextWebSocketFrame(s));
                        }
                    }

                    @Override
                    public void sendBinary(byte[] b) {
                        if (ch.isActive()) {
                            ch.writeAndFlush(new BinaryWebSocketFrame(Unpooled.wrappedBuffer(b)));
                        }
                    }

                    @Override
                    public void close(int code, String reason) {
                        if (ch.isActive()) {
                            ch.writeAndFlush(new CloseWebSocketFrame(code, reason)).addListener(ChannelFutureListener.CLOSE);
                        }
                    }
                }));
            } else {
                super.userEventTriggered(ctx, evt);
            }
        }

        @Override
        protected void channelRead0(ChannelHandlerContext ctx, Object msg) {
            Relay.Handle h = ctx.channel().attr(HANDLE).get();
            if (msg instanceof TextWebSocketFrame && h != null) {
                h.onText(((TextWebSocketFrame) msg).text());
            } else if (msg instanceof BinaryWebSocketFrame && h != null) {
                h.onBinary(ByteBufUtil.getBytes(((BinaryWebSocketFrame) msg).content()));
            } else if (msg instanceof FullHttpRequest) {
                boolean status = ((FullHttpRequest) msg).uri().equals("/status");
                String body = status ? Json.write(LanRelay.this.relay.status())
                        : "Reset Generation Revived relay (built into the game). Connect a game client to ws://<host>:" + PORT + "/ws\n";
                FullHttpResponse res = new DefaultFullHttpResponse(HttpVersion.HTTP_1_1, HttpResponseStatus.OK,
                        Unpooled.copiedBuffer(body, StandardCharsets.UTF_8));
                res.headers().set(HttpHeaderNames.CONTENT_TYPE, status ? "application/json" : "text/plain");
                res.headers().setInt(HttpHeaderNames.CONTENT_LENGTH, res.content().readableBytes());
                ctx.writeAndFlush(res).addListener(ChannelFutureListener.CLOSE);
            }
        }

        @Override
        public void channelInactive(ChannelHandlerContext ctx) throws Exception {
            Relay.Handle h = ctx.channel().attr(HANDLE).getAndSet(null);
            if (h != null) {
                h.onClose();
            }
            super.channelInactive(ctx);
        }

        @Override
        public void exceptionCaught(ChannelHandlerContext ctx, Throwable cause) {
            ctx.close();
        }
    }

    /** LAN discovery: answers "RGR-DISCOVER 1" with "RGR-HOST 1 &lt;port&gt; &lt;waiting games&gt; &lt;host name&gt;". */
    private final class Discovery extends SimpleChannelInboundHandler<DatagramPacket> {
        @Override
        protected void channelRead0(ChannelHandlerContext ctx, DatagramPacket pkt) {
            ByteBuf in = pkt.content();
            if (!in.toString(StandardCharsets.US_ASCII).startsWith(DISCOVER)) {
                return;
            }
            String answer = ANSWER + " " + PORT + " " + LanRelay.this.relay.hostedWaiting() + " " + LanRelay.this.hostName;
            ctx.writeAndFlush(new DatagramPacket(Unpooled.copiedBuffer(answer, StandardCharsets.US_ASCII), pkt.sender()));
        }

        @Override
        public void exceptionCaught(ChannelHandlerContext ctx, Throwable cause) {
            // e.g. ICMP port unreachable for an earlier answer: keep listening
        }
    }
}

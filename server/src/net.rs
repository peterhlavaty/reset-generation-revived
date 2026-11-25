//! The relay's transport: one thread, one tokio event loop, plain TCP.
//!
//! * `GET /ws` with a WebSocket upgrade: a game client (the relay protocol).
//! * `GET /status`: JSON counters; anything else: a short text page.
//! * Optional LAN discovery on UDP (same port): answers `RGR-DISCOVER 1` with
//!   `RGR-HOST 1 <port> <waiting hosted games> <host name>` (rg.relay.LanDiscovery).
//!
//! TLS is left to a reverse proxy (nginx in front of the public server); LAN play is plain.

use crate::relay::{Config, Out, Relay, MAX_PACKET};
use futures_util::{SinkExt, StreamExt};
use std::cell::{Cell, RefCell};
use std::net::{IpAddr, SocketAddr};
use std::rc::Rc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::{mpsc, Notify};
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, Role, WebSocketConfig};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

pub const DISCOVER: &str = "RGR-DISCOVER 1";
pub const ANSWER: &str = "RGR-HOST 1";
const HEAD_LIMIT: usize = 8192;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// Messages queued for one client before it counts as too slow and is dropped.
const QUEUE: usize = 1024;

pub struct Options {
    pub listen: SocketAddr,
    /// Answer LAN discovery on UDP (same port) with this host name.
    pub discovery: Option<String>,
    pub max_connections: usize,
    pub cfg: Config,
    pub quiet: bool,
}

struct Shared {
    relay: RefCell<Relay>,
    start: Instant,
    open: Cell<usize>,
    max: usize,
}

impl Shared {
    /// Milliseconds on the relay's clock (starts at 1,000,000 so "long ago" never underflows).
    fn now(&self) -> u64 {
        1_000_000 + self.start.elapsed().as_millis() as u64
    }
}

pub fn log_line(s: &str) {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    let secs = t.as_secs() % 86_400;
    println!("{:02}:{:02}:{:02}.{:03} {s}", secs / 3600, secs / 60 % 60, secs % 60, t.subsec_millis());
}

/// Runs the relay until the process ends. Must be called inside a `LocalSet`.
pub async fn serve(opts: Options) -> std::io::Result<()> {
    let listener = TcpListener::bind(opts.listen).await?;
    let addr = listener.local_addr()?;
    serve_on(listener, opts, addr).await
}

/// Like [`serve`] on an already bound listener (tests bind port 0).
pub async fn serve_on(listener: TcpListener, opts: Options, addr: SocketAddr) -> std::io::Result<()> {
    let log: Box<dyn Fn(&str)> = if opts.quiet { Box::new(|_| {}) } else { Box::new(log_line) };
    let shared = Rc::new(Shared {
        relay: RefCell::new(Relay::new(opts.cfg.clone(), log)),
        start: Instant::now(),
        open: Cell::new(0),
        max: opts.max_connections,
    });
    if !opts.quiet {
        log_line(&format!("relay listening on ws://{addr}/ws"));
    }
    if let Some(host) = opts.discovery.clone() {
        let bind = SocketAddr::new(std::net::Ipv4Addr::UNSPECIFIED.into(), addr.port());
        match UdpSocket::bind(bind).await {
            Ok(udp) => {
                if !opts.quiet {
                    log_line(&format!("answering LAN discovery on UDP {}", addr.port()));
                }
                tokio::task::spawn_local(discovery(udp, shared.clone(), addr.port(), host));
            }
            Err(e) => log_line(&format!("no LAN discovery (UDP {}): {e}", addr.port())),
        }
    }
    let ticker = shared.clone();
    tokio::task::spawn_local(async move {
        let mut iv = tokio::time::interval(Duration::from_millis(250));
        iv.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            iv.tick().await;
            let now = ticker.now();
            ticker.relay.borrow_mut().tick(now);
        }
    });
    loop {
        let (stream, peer) = match listener.accept().await {
            Ok(x) => x,
            Err(_) => {
                // out of file descriptors and the like: back off a little
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        let _ = stream.set_nodelay(true);
        let shared = shared.clone();
        tokio::task::spawn_local(async move {
            let _ = connection(stream, peer.ip(), shared).await;
        });
    }
}

async fn discovery(udp: UdpSocket, shared: Rc<Shared>, port: u16, host: String) {
    let mut buf = [0u8; 256];
    loop {
        let Ok((n, from)) = udp.recv_from(&mut buf).await else { continue };
        if !buf[..n].starts_with(DISCOVER.as_bytes()) {
            continue;
        }
        let waiting = shared.relay.borrow().hosted_waiting();
        let answer = format!("{ANSWER} {port} {waiting} {host}");
        let _ = udp.send_to(answer.as_bytes(), from).await;
    }
}

/// The request head: method, path and the headers (names lower-case).
struct Head {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
}

impl Head {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }
}

fn parse_head(raw: &[u8]) -> Option<Head> {
    let text = std::str::from_utf8(raw).ok()?;
    let mut lines = text.split("\r\n");
    let mut first = lines.next()?.split(' ');
    let method = first.next()?.to_string();
    let path = first.next()?.to_string();
    let headers = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    Some(Head { method, path, headers })
}

/// Reads up to the end of the request head; returns the head and any bytes after it.
async fn read_head(stream: &mut TcpStream) -> Option<(Head, Vec<u8>)> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    loop {
        let n = stream.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = parse_head(&buf[..end])?;
            return Some((head, buf[end + 4..].to_vec()));
        }
        if buf.len() > HEAD_LIMIT {
            return None;
        }
    }
}

async fn respond(stream: &mut TcpStream, status: &str, ctype: &str, body: &str) {
    let resp = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(resp.as_bytes()).await;
    let _ = stream.shutdown().await;
}

struct OpenGuard(Rc<Shared>);

impl Drop for OpenGuard {
    fn drop(&mut self) {
        self.0.open.set(self.0.open.get() - 1);
    }
}

async fn connection(mut stream: TcpStream, peer: IpAddr, shared: Rc<Shared>) -> Option<()> {
    if shared.open.get() >= shared.max {
        respond(&mut stream, "503 Service Unavailable", "text/plain", "relay full\n").await;
        return None;
    }
    shared.open.set(shared.open.get() + 1);
    let _guard = OpenGuard(shared.clone());
    let (head, rest) = tokio::time::timeout(HANDSHAKE_TIMEOUT, read_head(&mut stream)).await.ok()??;
    let path = head.path.split('?').next().unwrap_or("");
    let upgrade = head.header("upgrade").is_some_and(|u| u.eq_ignore_ascii_case("websocket"));
    if head.method != "GET" {
        respond(&mut stream, "405 Method Not Allowed", "text/plain", "GET only\n").await;
        return None;
    }
    match (path, upgrade) {
        ("/ws", true) => {}
        ("/status", _) => {
            let body = shared.relay.borrow().status().to_string();
            respond(&mut stream, "200 OK", "application/json", &body).await;
            return None;
        }
        ("/", _) | ("/ws", false) => {
            let body = "Reset Generation Revived relay. Connect a game client to ws://<this host>/ws\n";
            respond(&mut stream, "200 OK", "text/plain; charset=utf-8", body).await;
            return None;
        }
        _ => {
            respond(&mut stream, "404 Not Found", "text/plain", "not found\n").await;
            return None;
        }
    }
    let key = head.header("sec-websocket-key")?;
    let accept = derive_accept_key(key.as_bytes());
    let resp = format!(
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
    );
    stream.write_all(resp.as_bytes()).await.ok()?;
    let cfg = WebSocketConfig::default()
        .max_message_size(Some(MAX_PACKET + 1024))
        .max_frame_size(Some(MAX_PACKET + 1024));
    let ws = WebSocketStream::from_partially_read(stream, rest, Role::Server, Some(cfg)).await;
    // behind the local reverse proxy (the public server's nginx) the player's address is its header
    let addr = match head.header("x-real-ip").and_then(|a| a.parse().ok()) {
        Some(real) if peer.is_loopback() => real,
        _ => peer,
    };
    client(ws, addr, shared).await;
    Some(())
}

async fn client(mut ws: WebSocketStream<TcpStream>, addr: IpAddr, shared: Rc<Shared>) {
    let (tx, mut rx) = mpsc::channel::<Out>(QUEUE);
    let slow = Rc::new(Notify::new());
    let slow2 = slow.clone();
    let sink = Box::new(move |o: Out| {
        if tx.try_send(o).is_err() {
            slow2.notify_one(); // the client does not keep up (or is gone): drop it
        }
    });
    let id = {
        let now = shared.now();
        shared.relay.borrow_mut().connect_from(now, sink, Some(addr))
    };
    loop {
        tokio::select! {
            msg = ws.next() => {
                let now = shared.now();
                match msg {
                    Some(Ok(Message::Text(t))) => shared.relay.borrow_mut().on_text(now, id, t.as_str()),
                    Some(Ok(Message::Binary(b))) => shared.relay.borrow_mut().on_binary(now, id, &b),
                    Some(Ok(Message::Ping(_) | Message::Pong(_))) => {
                        if let Some(c) = shared.relay.borrow_mut().net.clients.get_mut(&id) {
                            c.last_seen = now;
                        }
                    }
                    Some(Ok(Message::Frame(_))) => {}
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                }
            }
            out = rx.recv() => {
                let Some(out) = out else { break };
                let msg = match out {
                    Out::Text(s) => Message::text(s),
                    Out::Binary(b) => Message::binary(b),
                    Out::Close(code, reason) => {
                        let frame = CloseFrame { code: CloseCode::from(code), reason: reason.into() };
                        let _ = tokio::time::timeout(Duration::from_secs(2), ws.close(Some(frame))).await;
                        break;
                    }
                };
                // flush only when nothing else is queued: one syscall per burst
                let r = if rx.is_empty() { ws.send(msg).await } else { ws.feed(msg).await };
                if r.is_err() {
                    break;
                }
            }
            _ = slow.notified() => break,
        }
    }
    let now = shared.now();
    shared.relay.borrow_mut().disconnect(now, id);
}

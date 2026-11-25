//! The real transport: WebSocket handshake, relaying between sockets, /status, LAN discovery.

use futures_util::{SinkExt, StreamExt};
use rg_server::net::{serve_on, Options, ANSWER, DISCOVER};
use rg_server::relay::Config;
use std::time::Duration;
use tokio::net::{TcpListener, UdpSocket};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

async fn next_text<S: StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin>(
    ws: &mut S,
) -> serde_json::Value {
    loop {
        let m = tokio::time::timeout(Duration::from_secs(5), ws.next()).await.expect("timeout").unwrap().unwrap();
        if let Message::Text(t) = m {
            return serde_json::from_str(t.as_str()).unwrap();
        }
    }
}

#[test]
fn two_clients_meet_over_websockets() {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let local = tokio::task::LocalSet::new();
    local.block_on(&rt, async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let opts = Options {
            listen: addr,
            discovery: Some("Tester".into()),
            max_connections: 16,
            cfg: Config::default(),
            quiet: true,
        };
        tokio::task::spawn_local(serve_on(listener, opts, addr));
        tokio::task::yield_now().await;

        let url = format!("ws://{addr}/ws");
        let (mut a, _) = connect_async(&url).await.unwrap();
        let (mut b, _) = connect_async(&url).await.unwrap();
        for (ws, name) in [(&mut a, "Ann"), (&mut b, "Bob")] {
            ws.send(Message::text(format!(r#"{{"t":"hello","v":1,"name":"{name}"}}"#))).await.unwrap();
            assert_eq!(next_text(ws).await["name"], name);
            ws.send(Message::text(r#"{"t":"move","room":"R"}"#)).await.unwrap();
            assert_eq!(next_text(ws).await["t"], "joined");
        }
        // the plain room greets each joiner with a binary frame first
        loop {
            if let Some(Ok(Message::Binary(_))) = b.next().await {
                break;
            }
        }
        a.send(Message::binary(vec![1u8, 2, 3])).await.unwrap();
        loop {
            match tokio::time::timeout(Duration::from_secs(5), b.next()).await.unwrap() {
                Some(Ok(Message::Binary(p))) if p.len() == 3 => {
                    assert_eq!(&p[..], &[1, 2, 3]);
                    break;
                }
                Some(Ok(_)) => {}
                other => panic!("{other:?}"),
            }
        }

        // /status over plain HTTP
        let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        s.write_all(b"GET /status HTTP/1.1\r\nHost: x\r\n\r\n").await.unwrap();
        let mut body = String::new();
        s.read_to_string(&mut body).await.unwrap();
        assert!(body.starts_with("HTTP/1.1 200") && body.contains("\"players\":2"), "{body}");

        // LAN discovery on the same port
        let udp = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        udp.send_to(DISCOVER.as_bytes(), ("127.0.0.1", addr.port())).await.unwrap();
        let mut buf = [0u8; 128];
        let (n, _) = tokio::time::timeout(Duration::from_secs(5), udp.recv_from(&mut buf)).await.unwrap().unwrap();
        assert_eq!(std::str::from_utf8(&buf[..n]).unwrap(), format!("{ANSWER} {} 0 Tester", addr.port()));

        // a client gone without a close frame frees its name at once
        drop(a);
        tokio::time::sleep(Duration::from_millis(100)).await;
        let (mut c, _) = connect_async(&url).await.unwrap();
        c.send(Message::text(r#"{"t":"hello","v":1,"name":"Ann"}"#)).await.unwrap();
        assert_eq!(next_text(&mut c).await["name"], "Ann");
    });
}

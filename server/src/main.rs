//! `rg-relay`: the Reset Generation Revived multiplayer server.
//!
//! ```text
//! rg-relay [--listen ADDR:PORT] [--lan [--host-name NAME]] [--match-size N] [--rejoin-secs N]
//!          [--max-connections N] [--log-packets] [--quiet] [--exit-with-parent]
//! ```
//!
//! * Public server (behind nginx): `rg-relay --listen 127.0.0.1:8787`.
//! * A hosting player's game starts it with `--lan --host-name NICK --exit-with-parent`: it
//!   listens on all interfaces and answers LAN discovery; it ends when the game ends (its stdin
//!   closes).

use rg_server::net::{self, Options};
use rg_server::relay::Config;
use std::net::SocketAddr;

const USAGE: &str = "usage: rg-relay [--listen ADDR:PORT] [--lan [--host-name NAME]] [--match-size N]
                [--rejoin-secs N] [--max-connections N] [--log-packets] [--quiet] [--exit-with-parent]
  --listen ADDR:PORT   where to listen (default 0.0.0.0:8787)
  --lan                also answer LAN discovery on UDP (same port)
  --host-name NAME     the host's name in discovery answers (default Host)
  --match-size N       also form Quick Match rooms of N players without a host (they need a
                       separate game server; default 0)
  --rejoin-secs N      how long a player whose connection dropped may go back into their match
                       (default: as long as the match runs)
  --max-connections N  default 4096
  --exit-with-parent   exit when stdin closes (a game that started the relay has ended)";

fn main() {
    let mut opts = Options {
        listen: "0.0.0.0:8787".parse().unwrap(),
        discovery: None,
        max_connections: 4096,
        cfg: Config::default(),
        quiet: false,
    };
    let mut lan = false;
    let mut host_name = "Host".to_string();
    let mut exit_with_parent = false;
    let mut args = std::env::args().skip(1);
    let fail = |m: String| -> ! {
        eprintln!("{m}\n{USAGE}");
        std::process::exit(2)
    };
    while let Some(a) = args.next() {
        let (k, inline) = match a.split_once('=') {
            Some((k, v)) => (k.to_string(), Some(v.to_string())),
            None => (a.clone(), None),
        };
        let mut val = || inline.clone().or_else(|| args.next()).unwrap_or_else(|| fail(format!("{k} needs a value")));
        match k.as_str() {
            "--listen" => opts.listen = val().parse::<SocketAddr>().unwrap_or_else(|e| fail(format!("--listen: {e}"))),
            "--port" => {
                let p: u16 = val().parse().unwrap_or_else(|e| fail(format!("--port: {e}")));
                opts.listen.set_port(p);
            }
            "--lan" => lan = true,
            "--host-name" => host_name = val(),
            "--match-size" => opts.cfg.match_size = val().parse().unwrap_or_else(|e| fail(format!("--match-size: {e}"))),
            "--rejoin-secs" => {
                let s: u64 = val().parse().unwrap_or_else(|e| fail(format!("--rejoin-secs: {e}")));
                opts.cfg.rejoin_ms = Some(s * 1000);
            }
            "--max-connections" => {
                opts.max_connections = val().parse().unwrap_or_else(|e| fail(format!("--max-connections: {e}")))
            }
            "--log-packets" => opts.cfg.log_packets = true,
            "--quiet" => opts.quiet = true,
            "--exit-with-parent" => exit_with_parent = true,
            "-h" | "--help" => {
                println!("{USAGE}");
                return;
            }
            _ => fail(format!("unknown option {a}")),
        }
    }
    if lan {
        host_name.retain(|c| c.is_ascii_graphic());
        opts.discovery = Some(if host_name.is_empty() { "Host".into() } else { host_name });
    }
    if exit_with_parent {
        std::thread::spawn(|| {
            let mut sink = [0u8; 64];
            use std::io::Read;
            let mut stdin = std::io::stdin();
            while matches!(stdin.read(&mut sink), Ok(n) if n > 0) {}
            std::process::exit(0);
        });
    }
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("tokio runtime");
    let local = tokio::task::LocalSet::new();
    if let Err(e) = local.block_on(&rt, net::serve(opts)) {
        eprintln!("rg-relay: {e}");
        std::process::exit(1);
    }
}

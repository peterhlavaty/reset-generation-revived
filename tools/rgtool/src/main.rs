//! rgtool: the project's one developer tool, run as `cargo x <command>` from anywhere in the
//! repository. It replaces the old shell / PowerShell / Python scripts and builds on any OS that
//! has Rust (some commands need the toolchains `cargo x setup` fetches).

mod deploy;
mod util;

use util::R;

const HELP: &str = "cargo x <command>

Server
  deploy                 build the relay for Linux and put it on the public server
  deploy-status          GET /status of the public server
  deploy-logs [N]        the last N lines of the server's log (default 100)
  deploy-ssh CMD         run one command on the server
  undeploy               remove the relay from the server again
";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = args.first().map(String::as_str) else {
        print!("{HELP}");
        return;
    };
    let rest = &args[1..];
    let r: R<()> = match cmd {
        "deploy" => deploy::deploy(),
        "deploy-status" => deploy::status(),
        "deploy-logs" => deploy::logs(rest.first().and_then(|n| n.parse().ok()).unwrap_or(100)),
        "deploy-ssh" => deploy::remote(&rest.join(" ")),
        "undeploy" => deploy::undeploy(),
        "help" | "-h" | "--help" => {
            print!("{HELP}");
            Ok(())
        }
        _ => Err(format!("unknown command {cmd}; `cargo x help` lists them").into()),
    };
    if let Err(e) = r {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

//! rgtool: the project's one developer tool, run as `cargo x <command>` from anywhere in the
//! repository. It replaces the old shell / PowerShell / Python scripts and builds on any OS that
//! has Rust (some commands need the toolchains `cargo x setup` fetches).

mod deploy;
mod play;
mod setup;
mod util;
mod windows;

use util::R;

const HELP: &str = "cargo x <command>

Setup
  setup [what…|all]      download toolchains into .toolchains/ (default: what the Windows build
                         needs; known: jdk17-x86, llvm-mingw, jdk21, ghidra)
  doctor                 show the toolchains found

Build
  build-windows          dist/ResetGeneration-Revived (+ .zip): the Windows game

Play / test (Windows build)
  run [game args…]       start the built game (e.g. --nickname=Bob --server=lan)
  lan-test [N] [--server=auto|online|lan|ws://…]
                         N games on this PC: one hosts, the others join with Quick Match
  keys \"K1,K2,WAIT500\" [shot] [--pid=N]
                         key presses to the game window (RETURN ESC UP DOWN LEFT RIGHT CTRL F5
                         F11 …, hex codes), then optionally a screenshot to build/shots/<shot>.png
  shot [name] [--pid=N]  screenshot of the game window
  killgame               end the game processes (only the game, not other Java programs)

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
        "setup" => setup::setup(rest),
        "doctor" => setup::doctor(),
        "build-windows" => windows::build(),
        "run" => play::run(rest),
        "lan-test" => play::lan_test(rest),
        "keys" => play::keys(rest),
        "shot" => play::shot(rest),
        "killgame" => play::killgame(),
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

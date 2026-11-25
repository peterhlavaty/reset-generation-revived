//! rgtool: the project's one developer tool, run as `cargo x <command>` from anywhere in the
//! repository. It replaces the old shell / PowerShell / Python scripts and builds on any OS that
//! has Rust (some commands need the toolchains `cargo x setup` fetches).

mod android;
mod data;
mod deploy;
mod native;
mod pakpatch;
mod play;
mod recomp;
mod setup;
mod util;
mod web;

use util::R;

const HELP: &str = "cargo x <command>

Setup
  setup [what…|all]      download toolchains into .toolchains/ (default: what the Windows build
                         needs; known: llvm-mingw, jdk25, ghidra, bearssl)
  doctor                 show the toolchains found

Build
  build-desktop          dist/ResetGeneration-Revived-<os> (+ .zip): the game for this OS
                         (Windows, Linux, macOS; build-windows is the same)
  build-web              dist/web: the game in a web browser (WebAssembly; single-player)
  serve-web [PORT]       serve dist/web on http://localhost:PORT (default 8080) to try it

Play / test (the desktop build, Windows)
  run [game args…]       start the built game (e.g. --nickname=Bob --server=lan)
  lan-test [N] [--server=auto|online|lan|ws://…]
                         N games on this PC: one hosts, the others join with Quick Match
  keys \"K1,K2,WAIT500\" [shot] [--pid=N]
                         key presses to the game window (RETURN ESC UP DOWN LEFT RIGHT CTRL F5
                         F11 …, hex codes), then optionally a screenshot to build/shots/<shot>.png
  shot [name] [--pid=N]  screenshot of the game window
  killgame               end the game processes (and rg-relay.exe)

Native (recompiled engine)
  ghidra-export [DLL]    headless Ghidra analysis -> decompiled/native/pcode.txt (needs
                         `cargo x setup jdk25 ghidra`; the first run takes several minutes)
  recomp PCODE OUT [--files N] [--dll DLL] [--trace a,b] [--hook a,b]
                         P-code -> C (the build commands run it themselves when needed)
  build-native-test [--calltrace] [--trace a,b]
                         build/native/rgtest: headless test driver for the recompiled engine
  native-test [FRAMES] [--size WxH] [--keys \"F:KEY,…\"] [--shots F,F] [--realtime]
                         run it (virtual clock; PPM screenshots in build/native/test)
  img-diff A.png B.png   count the pixels that differ (e.g. a web screenshot vs rgtest)

Android
  build-android [--abis=arm64-v8a,x86_64] [--diag]
                         dist/ResetGeneration-Revived-<version>.apk (needs the Android SDK + NDK)
  android-run [--avd=NAME]
                         start the emulator if no device is connected, install the APK, start it
  android-keys K1,K2     key presses (ENTER ESC UP DOWN LEFT RIGHT CTRL, WAITms)
  android-shot [name]    screenshot to build/shots/<name>.png
  android-log [N]        the game's last N log lines (logcat)

Game data
  pak list|extract FILE.pak [OUTDIR]   list / unpack a .pak (real file names from its name list)
  gfx FILE.gfx OUT.png                 a .gfx image (format 0, raw ARGB) to PNG
  spc FILE.spc OUT.wav [--low-first]   a .spc sound to WAV (tentative decoder)
  sisx FILE.sis OUTDIR                 unpack a Symbian 9 .sis (N-Gage packages)
  split-ngage FILE.n-gage OUTDIR       split an N-Gage 2.0 install file into its parts

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
        "build-windows" => native::build_desktop(rest), // the Windows game is the native desktop build
        "run" => play::run(rest),
        "lan-test" => play::lan_test(rest),
        "keys" => play::keys(rest),
        "shot" => play::shot(rest),
        "killgame" => play::killgame(),
        "ghidra-export" => native::ghidra_export(rest),
        "recomp" => recomp::cmd(rest),
        "build-native-test" => native::build_test(rest),
        "native-test" => native::run_test(rest),
        "build-desktop" => native::build_desktop(rest),
        "build-android" => android::build(rest),
        "build-web" => web::build(rest),
        "img-diff" => native::img_diff(rest),
        "serve-web" => web::serve(rest),
        "pak" => data::pak(rest),
        "pak-patch" => pakpatch::cmd(rest),
        "gfx" => data::gfx(rest),
        "spc" => data::spc(rest),
        "sisx" => data::sisx(rest),
        "split-ngage" => data::split_ngage(rest),
        "android-run" => android::run_emulator(rest),
        "android-keys" => android::keys(rest),
        "android-shot" => android::shot(rest),
        "android-log" => android::log(rest),
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
        if std::env::var_os("GITHUB_ACTIONS").is_some() {
            // shown on the workflow run's page (and in the public API's check-run annotations)
            println!("::error::{}", e.to_string().replace('\n', "%0A"));
        }
        std::process::exit(1);
    }
}

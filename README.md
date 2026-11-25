# Reset Generation Revived

Bringing **Reset Generation** (RedLynx / Nokia, 2008 — N-Gage and free web release) to modern
Windows in widescreen HD, to Linux, macOS and Android as native programs, and back online with our
own server.

| | Status |
|---|---|
| Windows 10/11 (the original engine DLL in a 32-bit Java host), widescreen HD, fullscreen | **Playable**, hosts and joins online/LAN games — `cargo x build-windows` → `dist/ResetGeneration-Revived/` (+ .zip) |
| Native desktop on the recompiled engine (Windows, Linux, macOS) | **Playable**, hosts and joins online games — `cargo x build-desktop` → `dist/ResetGeneration-Native-<os>/` (+ .zip). Tested on Windows; Linux/macOS are built by CI (`.github/workflows/native.yml`) |
| Android (phones, tablets, Chromebooks; touch + gamepad) | **Playable test build**, hosts and joins online games — `cargo x build-android` → `dist/ResetGeneration-Revived-<version>.apk` (sideload) |
| Online server (relay + matchmaking, Rust) | **Running** at `resetgeneration.rustyapps.com` — `cargo x deploy` |
| WebAssembly | Not yet: see `docs/NATIVE_PORT.md`, "WebAssembly" |

Multiplayer in short: press **F5** in the main menu (Android: the HOST button), choose the number of
players and a hero; everyone else (Windows, native, Android) chooses **Online → Quick Match**. The
games meet on the public server, or on the LAN without internet (the host's game runs the relay).
A player whose game crashed goes back into the running match by starting again with the same name
and choosing Quick Match — for as long as the match runs. Details: `docs/MULTIPLAYER.md`.

## Build

Everything is one Rust program, **rgtool**, run as `cargo x <command>` (`cargo x help` lists
them). It needs only [Rust](https://rustup.rs); it downloads the other toolchains itself into
`.toolchains/` (checksummed), so a fresh clone builds with:

```bash
cargo x setup              # once: 32-bit JDK 17 + llvm-mingw (what the Windows build needs)
cargo x build-windows      # dist/ResetGeneration-Revived/ResetGeneration-Revived.exe (+ .zip)
cargo x build-desktop      # dist/ResetGeneration-Native-<os>/ (the first run also does the
                           #   one-time Ghidra analysis of the engine: JDK 25 + Ghidra, minutes)
cargo x build-android      # dist/ResetGeneration-Revived-<version>.apk (Android SDK + NDK)
cargo test                 # server and rgtool tests
```

Running and testing: `cargo x run`, `cargo x lan-test 3` (a host and two clients on this PC),
`cargo x keys "RETURN,DOWN" shot` (drive the game window and take a screenshot), `cargo x
native-test`, `cargo x android-run` / `android-keys` / `android-shot` (emulator). Server:
`cargo x deploy`, `deploy-status`, `deploy-logs`, `undeploy` (settings in `deploy.local.ini`,
see `deploy.example.ini`).

Linux builds need a C compiler, `pkg-config` and `libasound2-dev`; macOS needs the Xcode command
line tools. Windows needs nothing besides Rust.

## Start here

* `docs/GAME_ANALYSIS.md` — what the downloads are, how the game and engine work, file formats, mechanics
* `docs/WINDOWS_PORT.md` — what the Windows build changes, how to build it, known issues
* `docs/NATIVE_PORT.md` — the static recompiler, the native runtime, desktop/Android/WebAssembly hosts
* `docs/ANDROID_PLAN.md` — why static recompilation (the options that were weighed)
* `docs/MULTIPLAYER.md` — the original online stack, our relay and matchmaker, the reconstructed protocol, deployment

## Layout

```
game/                       the original game files (engine DLL, jars, data paks), included with permission
src/java/rg/                Windows launcher + patched wrapper classes; rg.PatchTool for build-time data patches
src/java/rg/relay/          starts the bundled relay (rg-relay.exe) for LAN games; LAN discovery; server choice
src/java/com/redlynx/...    replacement online client (talks to our relay instead of N-Gage Arena)
src/assets/, src/dist/      icon, default revived.ini, player README
server/                     rg-relay: relay + Quick Match matchmaker + hosted game rooms (Rust, tokio)
native/rt/                  runtime for the recompiled engine: memory, Win32 + JNI shims, engine API, net client
native/desktop/             the native desktop game (Rust: minifb window, tinyaudio sound) + build.rs
native/host/desktop/        desktop platform services (host.c) and rgtest, the headless test driver
native/android/             Android NativeActivity host (touch controls, gamepad, audio), manifest, icons
native/win32/               rghost.dll: "host a game" for the Windows build (32-bit JNI helper)
tools/rgtool/               cargo x: setup, builds, recompiler, Ghidra export, data tools, test driving, deploy
tools/lan-setup/            LAN-Setup.exe: opens port 8787 to the local network (Windows Firewall)
tools/ghidra_scripts/       Ghidra scripts used by cargo x ghidra-export (P-code and C export)
tools/testdata/             a post-tutorial save for testing
decompiled/                 Ghidra output and the P-code export (generated locally; git-ignored)
.toolchains/, build/, dist/ downloaded toolchains, build outputs (git-ignored)
```

## Game files and decompiled code

`game/` holds the original game files, included with permission: the 2020 standalone of the
web version (`ResetGeneration.jar`, `lib/rg-signed.jar`, `ResetGeneration/` with the engine
DLL, `zlib1.dll`, the macOS engine and all data paks), minus its obsolete Java 6 runtime. They
are kept pristine; every fix is applied at build or launch time.

Decompiled code is not included: `decompiled/` and the recompiler's generated C
(`build/native/gen`) are produced locally (`cargo x ghidra-export`, run by the native builds when
needed). The one exception is `src/java/rg/ResetGeneration.java`, a modified decompile of the
original wrapper class, kept because the launcher needs to replace it to fix the screen size and
scaling.

## Credits

Reset Generation © 2008 RedLynx / Nokia. 2020 standalone revival by curoviyxru, bodyz and
bs0dd. Fonts by Larabie Fonts.

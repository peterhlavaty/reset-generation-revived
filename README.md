# Reset Generation Revived

**Reset Generation** (RedLynx / Nokia, 2008) is a game of falling blocks and strategy for one to
four players: build paths across the board with blocks, shoot the enemy's blocks with your
cannons and send your hero to free a princess. This project brings it back as a game for Windows,
Linux, macOS, Android and web browsers, in widescreen, with online multiplayer on our own server.

## Play

**In the browser, nothing to install:** https://resetgeneration.rustyapps.com/ — on a computer
with the keyboard, on phones and tablets with on-screen buttons (hold the phone sideways). Your
progress is saved in that browser (clearing the site's data deletes it).

**Or download the game** (the newest build, made automatically from this repository):

| System | Download |
|---|---|
| Windows 10 or 11 (64-bit) | [ResetGeneration-Revived-windows.zip](https://github.com/peterhlavaty/reset-generation-revived/releases/download/latest/ResetGeneration-Revived-windows.zip) |
| Linux (64-bit PC: x86-64) | [ResetGeneration-Revived-linux.zip](https://github.com/peterhlavaty/reset-generation-revived/releases/download/latest/ResetGeneration-Revived-linux.zip) |
| macOS on Apple Silicon (M1 or newer) | [ResetGeneration-Revived-macos.zip](https://github.com/peterhlavaty/reset-generation-revived/releases/download/latest/ResetGeneration-Revived-macos.zip) |
| Android 8 or newer (64-bit) | [ResetGeneration-Revived-android.apk](https://github.com/peterhlavaty/reset-generation-revived/releases/download/latest/ResetGeneration-Revived-android.apk) |

All files are also on the [Latest build](https://github.com/peterhlavaty/reset-generation-revived/releases/tag/latest)
page. Intel Macs and other systems: play in the browser. The Windows version is the most
tested; Linux, macOS and Android are newer. Something wrong?
[Report it](https://github.com/peterhlavaty/reset-generation-revived/issues).

### Start it

* **Windows:** right-click the zip → *Extract All…*, open the extracted folder and double-click
  **ResetGeneration.exe** (starting it from inside the zip does not work). Windows may say
  "Windows protected your PC", because the game is not signed by a publisher: click *More info*
  → *Run anyway*. (If it says *Smart App Control* blocked it, Windows offers no way around that:
  play in the browser.)
* **macOS:** open the zip (Safari may already have unzipped it in Downloads), open the folder and
  double-click **ResetGeneration**. It runs together with a Terminal window: leave that open. The
  first time, macOS refuses to open it because the game is not signed: click *Done*, open
  *System Settings → Privacy & Security*, scroll down, click *Open Anyway*, start the game again
  and confirm with your password. If macOS asks whether Terminal may access a folder, click
  *Allow*.
* **Linux:** unzip it, open the folder, right-click an empty spot → *Open in Terminal*, type
  `./ResetGeneration` and press Enter (if it does not start, the terminal says why). It needs a
  recent distribution (built on the newest Ubuntu LTS) with X11 or XWayland and ALSA sound
  (libasound2, called libasound2t64 on Ubuntu 24.04 and newer); on older ones play in the
  browser.
* **Android:** download the APK on the phone (if Chrome warns, tap *Download anyway*) and open
  it. Allow installing apps from that source when asked, then *Install*. If Play Protect warns,
  tap *More details → Install anyway*. To update, install the new APK over the old one: your
  progress stays.

Keep the game folder somewhere you can write to (Desktop, Documents, a Games folder; not
*Program Files*): your progress and settings are saved in it. To update, extract the new zip
over the old folder and replace the files: your saved games stay (settings and achievements
start over).

### Controls

| | Keyboard (computer, browser) | Touch (Android, phone browser) | Gamepad (Android) |
|---|---|---|---|
| Move the cursor or the block, choose in menus | Arrow keys | ◀ ▲ ▶ ▼ on the left | D-pad or stick |
| Action: rotate the block, OK, move or attack | Ctrl (tap) | **ROTATE / OK**, the big button on the right | A |
| Drop the block | hold Ctrl | **DROP** above it (or hold ROTATE / OK) | hold A |
| Select (the choice shown at the bottom left) | Enter | **SELECT** (Android: ▬), top left | Start or X |
| Back (the choice at the bottom right), skip the intro | Esc or Backspace | **BACK** (Android: ✕), top right; in the app also the phone's Back | B |
| Host a multiplayer game (main menu) | F5 (on Macs and many laptops fn + F5) | **HOST** | — |
| Full screen | F11 or Alt+Enter on Windows; elsewhere maximize the window | — | — |

On a computer the keys can be changed in the game: *Settings → Custom Keys*. On a Mac, Control +
arrow keys may switch desktops; turn those shortcuts off in *System Settings → Keyboard →
Keyboard Shortcuts → Mission Control*.

### How to play

Rescue your Princess and bring an enemy Princess to your Castle. Every round has three phases,
which all players play at the same time:

1. **Blocks:** drop the falling blocks onto the board. Five starred blocks in a row make a Combo;
   your hero moves farther and fights better on your own Combos.
2. **Cannons:** shoot two enemy blocks (Combos and heroes cannot be hit).
3. **Move:** choose a path with the arrows, then Action to move, attack or pick something up.

Items (grenades, batteries…) and every hero's special Power add some chaos. New to the game?
Start with **Single-Player → Tutorial**; then Story Mode, or Arcade against up to three computer
players.

### Play with friends (2–4 players)

1. One player hosts: in the main menu press **F5** (touch: **HOST**), choose the number of
   players (Left/Right, then Enter or OK) and a hero.
2. Everyone else chooses **Quick Match** in the main menu and a hero. The *Players in Match*
   screen shows whose game you are in: the host and the players already in it. The match starts
   when all seats are taken; the host plays too.

It works over the internet between all versions (Windows, Linux, macOS, Android and the
browser). Quick Match picks a game hosted from your own home network first, otherwise the game
hosted most recently, so join right after your friend starts hosting. Wrong host on the list?
Press Back and choose Quick Match again: you get the next waiting game. (On the public server a
stranger may take a free seat too.) After a match the host presses F5 (HOST) again and the
others choose *Play Again*.

* **Your name online** (letters, digits and _, up to 15): in the browser the *Online name* field
  at the top left. In the downloaded game start it once, close it, open `revived.ini` in the
  game folder with a text editor (Notepad, TextEdit) and change the `nickname=` line. On Android
  a name like Droid1234 is chosen for you.
* **Without internet, on one network:** the downloaded games for Windows, Linux and macOS find a
  game hosted on the same network by themselves. Windows asks once whether the game may use the
  network: allow it. If the others still cannot connect, double-click **LAN-Setup.exe** on the
  host's PC (*More info → Run anyway* again) and answer *Yes* when Windows asks for
  administrator rights. A Linux or Mac host with a firewall must allow TCP and UDP port 8787
  from the local network. The browser version plays over the internet only.
* **If your game crashes during a match,** start it again with the same name and choose Quick
  Match: you return to your seat, for as long as the match runs. (Not for the host: if the
  host's game ends, so does the match.)

More about LAN games, the firewall and the settings: `README.txt` in the game folder.

## For developers

The game's engine is the original native x86 DLL, statically recompiled to C for every platform
(no emulator); see [docs/NATIVE_PORT.md](docs/NATIVE_PORT.md).

| | Status |
|---|---|
| Desktop: Windows, Linux, macOS (one executable + data; fullscreen on Windows) | **Playable**, hosts and joins online and LAN games — `cargo x build-desktop` → `dist/ResetGeneration-Revived-<os>/` (+ .zip). Tested on Windows; Linux/macOS are built by CI |
| Android (phones, tablets, Chromebooks; touch + gamepad) | **Playable test build**, hosts and joins online games — `cargo x build-android` → `dist/ResetGeneration-Revived-<version>.apk` (sideload) |
| Online server (relay + matchmaking, Rust) | **Running** at `resetgeneration.rustyapps.com` — `cargo x deploy` |
| Web browser (WebAssembly) | **Playable at https://resetgeneration.rustyapps.com/**, online play included (keyboard, touch buttons; saves in the browser) — `cargo x build-web` → `dist/web/` (static files), `cargo x serve-web` to try it, `cargo x deploy-web` to publish it |

CI (`.github/workflows/native.yml`) builds the desktop game for Windows, Linux and macOS, the APK
and the browser version on every push to main, and publishes the downloads above as the rolling
`latest` pre-release (`cargo x github-release`). The APK is signed with the key in the repository
secret `APK_KEYSTORE` (the keystore in base64), so every release updates the installed app.

### Build

Everything is one Rust program, **rgtool**, run as `cargo x <command>` (`cargo x help` lists
them). It needs only [Rust](https://rustup.rs); it downloads the other toolchains itself into
`.toolchains/` (checksummed), so a fresh clone builds with:

```bash
cargo x setup              # optional: the toolchains up front (the builds fetch what they need)
cargo x build-desktop      # dist/ResetGeneration-Revived-<os>/ (+ .zip); the first run also does
                           #   the one-time Ghidra analysis of the engine: JDK 25 + Ghidra, minutes
cargo x build-android      # dist/ResetGeneration-Revived-<version>.apk (Android SDK + NDK)
cargo x build-web          # dist/web/: the browser version (uses the NDK's clang)
cargo test                 # server and rgtool tests
```

Running and testing: `cargo x run`, `cargo x lan-test 3` (a host and two clients on this PC),
`cargo x keys "RETURN,DOWN" shot` (drive the game window and take a screenshot), `cargo x
native-test`, `cargo x oracle` (the recompiled engine against the original DLL), `cargo x
android-run` / `android-keys` / `android-shot` (emulator). Browser: `cargo x serve-web`, then
http://localhost:8080/. Server: `cargo x deploy`, `deploy-web`, `deploy-status`, `deploy-logs`, `undeploy`
(settings in `deploy.local.ini`, see `deploy.example.ini`).

Linux builds need a C compiler, `pkg-config` and `libasound2-dev`; macOS needs the Xcode command
line tools. Windows needs nothing besides Rust.

### Start here

* `docs/GAME_ANALYSIS.md` — what the downloads are, how the game and engine work, file formats, mechanics
* `docs/NATIVE_PORT.md` — the static recompiler, the native runtime, desktop/Android/browser hosts
* `docs/WINDOWS_PORT.md` — the widescreen fixes, and the retired Java-hosted Windows build
* `docs/ANDROID_PLAN.md` — why static recompilation (the options that were weighed)
* `docs/MULTIPLAYER.md` — the original online stack, our relay and matchmaker, the reconstructed protocol, deployment

### Layout

```
game/                       the original game files (engine DLL, jars, data paks), included with permission
server/                     rg-relay: relay + Quick Match matchmaker + hosted game rooms (Rust, tokio)
native/rt/                  runtime for the recompiled engine: memory, Win32 + JNI shims, engine API,
                            online client, game host, relay links + LAN discovery
native/desktop/             the desktop game (Rust: minifb window, tinyaudio sound, in-process LAN
                            relay), icon, player README
native/host/desktop/        desktop platform services (host.c) and rgtest, the headless test driver
native/android/             Android NativeActivity host (touch controls, gamepad, audio), manifest, icons
tools/rgtool/               cargo x: setup, builds, recompiler, Ghidra export, data tools (pak patcher),
                            test driving, deploy
tools/lan-setup/            LAN-Setup.exe: opens port 8787 to the local network (Windows Firewall)
tools/ghidra_scripts/       Ghidra scripts used by cargo x ghidra-export (P-code and C export)
tools/testdata/             a post-tutorial save for testing
decompiled/                 Ghidra output and the P-code export (generated locally; git-ignored)
.toolchains/, build/, dist/ downloaded toolchains, build outputs (git-ignored)
```

### Game files and decompiled code

`game/` holds the original game files, included with permission: the 2020 standalone of the
web version (`ResetGeneration.jar`, `lib/rg-signed.jar`, `ResetGeneration/` with the engine
DLL, `zlib1.dll`, the macOS engine and all data paks), minus its obsolete Java 6 runtime. They
are kept pristine; every fix is applied at build time.

Decompiled code is not included: `decompiled/` and the recompiler's generated C
(`build/native/gen`) are produced locally (`cargo x ghidra-export`, run by the native builds when
needed).

## Credits

Reset Generation © 2008 RedLynx / Nokia. 2020 standalone revival by curoviyxru, bodyz and
bs0dd. Fonts by Larabie Fonts.

This repository is a fork of [rcadden/reset-generation-revived](https://github.com/rcadden/reset-generation-revived),
where the project began: the widescreen Windows build, the reverse-engineering docs and tools,
the static recompiler and the Android port, and the first multiplayer server.

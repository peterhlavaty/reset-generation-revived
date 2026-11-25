# Reset Generation Revived — project notes for agents

Read `README.md`, then the doc for the area you touch (`docs/`). Public repo:
github.com/peterhlavaty/reset-generation-revived (branch `main`). Never commit personal paths,
emails, secrets (`deploy.local.ini`, SSH keys) or decompiled/generated code (`decompiled/`,
`build/` are git-ignored). The original game files in `game/` are included with permission; keep
them pristine (patch at build/launch time, never in place).

Languages: Rust and C (and C++ if ever needed); Java only for the Ghidra scripts
(`tools/ghidra_scripts`). No Python, shell, PowerShell or JS/TS: tools go into rgtool
(`tools/rgtool`, run as `cargo x <command>`).

## Facts that are easy to get wrong

* The real game is the **native x86 DLL** `ResetGeneration-Win32-Web.dll` (the original Java
  wrapper was only a host). Every build runs it **statically recompiled**; only the test oracle
  (`native/oracle`, `cargo x oracle`) runs the original, natively in a 32-bit process, to check
  the recompiler (the 32-bit Java host was retired 2026-11-25, see git history). JDK 25 is only a
  build tool (Ghidra 12.2, keytool, apksigner).
* The engine accepts **any screen size** at `init` and lays out its UI for it. Default is
  640×360 (≥320 rows so 240×320 cutscenes fit). The native builds are fixed at 640×360 (their
  data and layout patches are made for it).
* The **N-Gage paks are DRM-encrypted**; only the web/standalone paks are usable.
* Game data paks are patched at build time (`tools/rgtool/src/pakpatch.rs`, `cargo x pak-patch`:
  wipe masks, cutscene offsets for 640Ã360) into the `data/` of each build. Never edit `*.pak` by
  hand; change the transform.
* The engine only draws when ahead of its 32 fps schedule (frame limiter on `timeGetTime`);
  hosts must supply time and let `Sleep()` advance it.
* No floating point in the engine's hot code (fixed-point).
* The engine's file API is ANSI (`CreateFileA`) and gets its home directory as UTF-8: a non-ASCII
  game folder crashes it, so hosts make that folder the current directory and pass a relative
  home (`native/desktop/src/main.rs`).
* The engine's clock must start small: it misbehaves with large `timeGetTime` values (long
  uptimes), so hosts rebase it (`rt_now_ms` starts at 5,000,000).
* Native (desktop, Android) = **static recompilation**: Ghidra P-code (`cargo x ghidra-export`)
  → `tools/rgtool/src/recomp.rs` → C in `build/native/gen` + `native/rt`. Code only reachable
  through vtables/stored callbacks must be recovered by the export's pre-pass; an "unknown
  address" fatal means a missed entry point. The guest address space is a 4 GB reservation
  (native builds).
* Browser (`build-web`, `native/web`): the same C for wasm32 with our own small libc
  (`native/web/libc`), linked into a Rust crate. Guest addresses are linear-memory addresses
  (`M = 0`, fixed map above 0x10000000 in `native/rt/host.h`; an offset base breaks because clang
  folds constant addresses into wasm load offsets). One thread (`RT_NO_THREADS`): its relay links
  are the page's WebSockets (`native/web/src/link.rs`, polled), and net.c/server.c step their
  network work on the game thread instead of blocking. Its frames must equal rgtest's
  (`?keys=…&shots=…` + `cargo x img-diff`). Public at https://resetgeneration.rustyapps.com/
  (`cargo x deploy-web`: static files in /var/www/resetgeneration, our nginx site).
* Online: the engine waits for the room's server to speak first; Quick Match and hosted rooms
  are served by `server/` (rg-relay, Rust); Quick Match seats each player in one hosted game (own
  network first, else the newest; Back + Quick Match = the next one). Matches run on the hosting player's engine
  (`native/rt/server.c`). The desktop game runs the relay in-process for LAN games (the
  `rg-server` library; clients find it by UDP broadcast on 8787, `server=auto`). A dropped
  player's seat is held for as long as the match runs (`rejoin_ms: None`). Games reach the public
  relay by `wss://` (BearSSL in `native/rt/link.c`, Let's Encrypt roots built in), falling back
  to `ws://` on port 80; LAN relays are plain `ws://`.
* Windows child processes inherit our std handles unless `util::no_inherit()` ran: piped rgtool
  commands then hang until the game exits.
* aapt2 on Windows writes asset names with `\`; android.rs rewrites them (regression test).

## Tools

* `cargo x setup [all|llvm-mingw|jdk25|ghidra|bearssl]`, `cargo x doctor` — toolchains in
  `.toolchains/` (`RG_TOOLCHAINS` overrides). The Android SDK is found via `ANDROID_HOME` or the
  Android Studio default location.
* Builds: `build-desktop` (Windows, Linux, macOS; `build-windows` is an alias), `build-android`,
  `build-web` (+ `serve-web`), `build-native-test` (+ `native-test`; `img-diff` compares shots),
  `oracle` (the recompiled engine against the original DLL, frame by frame and call by call;
  Windows; run it after changing recomp.rs or native/rt).
* Play/test: `run`, `lan-test N`, `keys "RETURN,DOWN,WAIT500" [shot] [--pid=N]`, `shot`,
  `killgame` (only game processes — never kill all `java.exe`), `android-run`, `android-keys`,
  `android-shot`, `android-log`. Screenshots go to `build/shots/`, logs to `build/logs/`.
* Data: `pak list|extract`, `gfx`, `spc`, `sisx`, `split-ngage`.
* Server: `server/` (`cargo test -p rg-server`, `cargo run -p rg-server -- --log-packets`);
  `deploy`, `deploy-web`, `deploy-status`, `deploy-logs`, `deploy-ssh`, `undeploy` (settings in
  `deploy.local.ini`; the server also runs other sites — touch only our unit and nginx site).
* CI: `.github/workflows/native.yml` builds the desktop game (Windows, Linux, macOS), the APK and
  the browser version, and publishes them as the rolling `latest` pre-release (`cargo x
  github-release`); the README's download links point there. Player downloads live on GitHub,
  not on the rustyapps server.

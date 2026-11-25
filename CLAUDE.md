# Reset Generation Revived — project notes for agents

Read `README.md`, then the doc for the area you touch (`docs/`). Public repo:
github.com/peterhlavaty/reset-generation-revived (branch `main`). Never commit personal paths,
emails, secrets (`deploy.local.ini`, SSH keys) or decompiled/generated code (`decompiled/`,
`build/` are git-ignored). The original game files in `game/` are included with permission; keep
them pristine (patch at build/launch time, never in place).

Languages: Rust, Java, C (and C++ if ever needed). No Python, shell, PowerShell or JS/TS: tools go
into rgtool (`tools/rgtool`, run as `cargo x <command>`).

## Facts that are easy to get wrong

* The real game is the **native x86 DLL** `ResetGeneration-Win32-Web.dll`; Java is only a host.
  The DLL is 32-bit, so any JVM that loads it must be **32-bit** (`.toolchains/jdk17-x86`; JDK 25
  x64 is for Ghidra 12.2 and the build-time Java tools elsewhere).
* The engine accepts **any screen size** at `init` and lays out its UI for it. Default is
  640×360 (≥320 rows so 240×320 cutscenes fit). The native builds are fixed at 640×360 (their
  data and layout patches are made for it).
* The **N-Gage paks are DRM-encrypted**; only the web/standalone paks are usable.
* Game data paks are patched at launch by `PakPatcher` from `*.pak.orig` (wipe masks, cutscene
  offsets); the native builds pre-patch them with `rg.PatchTool`. Never edit `*.pak` by hand;
  change the transform and bump `PakPatcher.VERSION`.
* The engine only draws when ahead of its 32 fps schedule (frame limiter on `timeGetTime`);
  hosts must supply time and let `Sleep()` advance it.
* No floating point in the engine's hot code (fixed-point).
* The engine's file API is ANSI (`CreateFileA`) and gets its home directory as UTF-8: a non-ASCII
  game folder crashes it, so hosts make that folder the current directory and pass a relative
  home (`HostServer.engineHome` / `rghost.dll`; the native desktop game does the same).
* The engine's clock must start small: it misbehaves with large `timeGetTime` values (long
  uptimes), so hosts rebase it (`rt_now_ms` starts at 5,000,000).
* Native (desktop, Android) = **static recompilation**: Ghidra P-code (`cargo x ghidra-export`)
  → `tools/rgtool/src/recomp.rs` → C in `build/native/gen` + `native/rt`. Code only reachable
  through vtables/stored callbacks must be recovered by the export's pre-pass; an "unknown
  address" fatal means a missed entry point. The guest address space is a 4 GB reservation.
* Online: the engine waits for the room's server to speak first; Quick Match and hosted rooms
  are served by `server/` (rg-relay, Rust). Matches run on the hosting player's engine
  (`rghost.dll`, Windows build only); native/Android clients join. The Windows build bundles
  `rg-relay.exe` for LAN games (clients find it by UDP broadcast on 8787, `server=auto`). A
  dropped player's seat is held for as long as the match runs (`rejoin_ms: None`).
* Windows child processes inherit our std handles unless `util::no_inherit()` ran: piped rgtool
  commands then hang until the game exits.
* aapt2 on Windows writes asset names with `\`; android.rs rewrites them (regression test).

## Tools

* `cargo x setup [all|jdk17-x86|llvm-mingw|jdk25|ghidra]`, `cargo x doctor` — toolchains in
  `.toolchains/` (`RG_TOOLCHAINS` overrides). The Android SDK is found via `ANDROID_HOME` or the
  Android Studio default location.
* Builds: `build-windows`, `build-desktop`, `build-android`, `build-native-test` (+ `native-test`).
* Play/test: `run`, `lan-test N`, `keys "RETURN,DOWN,WAIT500" [shot] [--pid=N]`, `shot`,
  `killgame` (only game processes — never kill all `java.exe`), `android-run`, `android-keys`,
  `android-shot`, `android-log`. Screenshots go to `build/shots/`, logs to `build/logs/`.
* Data: `pak list|extract`, `gfx`, `spc`, `sisx`, `split-ngage`.
* Server: `server/` (`cargo test -p rg-server`, `cargo run -p rg-server -- --log-packets`);
  `deploy`, `deploy-status`, `deploy-logs`, `deploy-ssh`, `undeploy` (settings in
  `deploy.local.ini`; the server also runs other sites — touch only our unit and nginx site).
* CI: `.github/workflows/native.yml` builds the desktop game on Linux/macOS and the APK.

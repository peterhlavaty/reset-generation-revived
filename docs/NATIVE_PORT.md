# Native port: recompiled engine, desktop game, Android APK

The native desktop game (Windows, Linux, macOS) and the Android version run the original x86
engine as **portable C produced by static recompilation** — option C of `ANDROID_PLAN.md`. No
emulator and no JIT: every instruction of `ResetGeneration-Win32-Web.dll` is translated ahead of
time.

```
game/ResetGeneration/ResetGeneration-Win32-Web.dll
   │  cargo x ghidra-export: Ghidra 12.2 headless + tools/ghidra_scripts/ExportPcode.java
   ▼
decompiled/native/pcode.txt        raw P-code of all 476k instructions (local, git-ignored)
   │  cargo x recomp (tools/rgtool/src/recomp.rs; the build commands run it when needed)
   ▼
build/native/gen/rg_gen_*.c        ~11,000 C functions + address→function table (generated)
   │  + native/rt/      runtime: guest memory, dispatch, Win32 + JNI shims, engine glue, net client
   │  + native/host/desktop/host.c + native/desktop (Rust: window, sound)  → desktop game
   │  + native/host/desktop/rgtest.c   headless test driver
   │  + native/android/main.c          NativeActivity host (NDK)
   ▼
ResetGeneration(.exe)  /  rgtest  /  libresetgen.so → dist/ResetGeneration-Revived-<ver>.apk
```

## How the translation works

* **Input is Ghidra P-code**, not x86 decoded by hand: Ghidra's SLEIGH spec gives exact
  semantics for every instruction, including flags. `ExportPcode.java` writes one line per
  instruction and P-code op, plus Ghidra's function entries and switch-table targets.
* **One C function per guest function.** Entries are Ghidra's functions, plus code addresses
  found in data (vtables: 1,271 extra) and in instruction immediates (callbacks pushed as
  arguments). A function's body is everything reachable from its entry without crossing a
  call; code shared between functions is duplicated; a jump into another entry is a tail call.
* **Guest memory** is one flat 4 GB reservation; guest pointers stay 32-bit (`M + addr`).
  Memory map: stack `0x00100000`, TEB `0x00400000`, JNI tables `0x00500000`, image
  `0x10000000`, heap `0x20000000`, VirtualAlloc `0x40000000`, host stubs `0xF0000000+`.
* **Registers:** EAX..EDI are copied into C locals at function entry and spilled around calls,
  so the C compiler keeps them in host registers. Flags are function-local (MSVC code never
  keeps flags live across a call) and dead flag computations disappear in the optimiser.
  x87, DF and rare registers live in a register file indexed by Ghidra's register offsets;
  80-bit x87 values are host doubles (the engine's hot code is integer/fixed-point).
* **Calls** push the real return address on the guest stack (code that inspects it still
  works) and call the C function; `RET` pops it and returns. Indirect calls and jumps go
  through `rt_call()` (hash table), which also routes the fake addresses of imports and JNI
  functions to host stubs.
* **Fast paths:** `REP MOVS/STOS` become `memmove`/`memset`; `CPUID` reports a Pentium
  without SSE2, so the CRT never takes its SSE2 paths (the only instructions left untranslated).

## Runtime (native/rt)

| File | Role |
|---|---|
| `rt.h`, `rt.c` | memory access helpers used by generated code, dispatch, string ops, CPUID, 80-bit floats |
| `win32.c` | PE loader (sections, exports, IAT → stubs), guest heap, the 86 kernel32/winmm imports |
| `jni.c` | fake `JNIEnv` (240-entry table in guest memory): strings, direct buffers, `Call*Method` → host |
| `engine.c`, `engine.h` | boot sequence and per-frame API (`rg_init`, `rg_tick`, `rg_key`, `rg_audio`, `rg_pixels`) |
| `net.c`, `net.h` | online play: the engine's SNAP Mobile interface on the relay (`MULTIPLAYER.md`), ws:// |

The Win32/JNI behaviour was checked call by call against the original DLL running under a CPU
emulator (Unicorn; that Python oracle is no longer in the repository).

## Building

All through rgtool; toolchains are downloaded into `.toolchains/` on demand.

```bash
cargo x ghidra-export      # once per DLL change (the builds below run it when pcode.txt is
                           #   missing): JDK 25 + Ghidra 12.2, several minutes
cargo x build-desktop      # dist/ResetGeneration-Native-<os>/ (+ .zip): the desktop game
cargo x build-android      # dist/ResetGeneration-Revived-<version>.apk  [--abis=arm64-v8a,x86_64]
cargo x build-native-test  # build/native/rgtest(.exe): headless test driver  [--calltrace]
```

The recompiler regenerates `build/native/gen` when the P-code or rgtool changed; C objects are
compiled in parallel and only when out of date. The generated code is ~90 MB of C, so keep the
checkout out of synced folders (OneDrive and the like).

Per platform:

* **Windows** (desktop): llvm-mingw (`cargo x setup llvm-mingw`) and the Rust target
  `x86_64-pc-windows-gnullvm` (added automatically). One static `.exe`, no DLLs.
* **Linux**: a C compiler, `pkg-config`, `libasound2-dev` (ALSA); the window is X11 (also under
  XWayland), loaded at run time.
* **macOS**: the Xcode command line tools.
* **Android**: Android SDK with NDK (≥ 26) and build-tools (found via `ANDROID_HOME` or the
  Android Studio default); the APK is debug-signed with `build/android/debug.keystore`.
* CI (`.github/workflows/native.yml`) builds the Linux and macOS desktop games and the APK and
  keeps them as workflow artifacts; the Ghidra P-code is cached between runs.

## Desktop game

`native/desktop` (Rust, package `rg-native`) links the engine (built by `build-desktop` into
`build/native/desktop/librgengine.a`) and gives it a [minifb](https://crates.io/crates/minifb)
window and [tinyaudio](https://crates.io/crates/tinyaudio) sound:

* The engine runs on the main thread at its own 32 fps pace; the 640×360 frame is shown scaled
  with the aspect ratio kept (the window can be resized). Keys are sent as Java AWT codes
  (arrows, Enter, Esc/Backspace, Ctrl, Space, letters, digits).
* Sound: the engine's 8 kHz mono mix goes into a queue (~100 ms) that the audio thread resamples
  to 44.1 kHz stereo.
* Next to the program: `data/` (the game data, pre-patched for 640×360 by `rg.PatchTool`; saves
  go there too, so the folder must be writable), `revived.ini` (`server=online` or a `ws://`
  URL, `nickname=`, `scale=fit|1|2|4`; written on first start, `--key=value` arguments override
  it) and `ResetGeneration.log`. The game runs with `data/` as its current directory, so a
  folder name with non-ASCII characters works.
* Online: joins games (Quick Match) like Android and hosts them: F5 in the main menu (or `host=auto`,
  `host_players=2..4`, `host_hero=NAME|RANDOM`); the title bar shows the hosting status.

## WebAssembly (not built yet)

The same C would compile with Emscripten; what it needs:

* **Memory.** The runtime reserves the full 4 GB guest address space (`M` + 32-bit guest
  address) and the guest heap sits at `0x20000000`, stack at `0x00100000`. wasm32 has at most
  4 GB in total, so guest memory must shrink: map the DLL image, a heap of a few hundred MB and the
  stack into one linear block and translate addresses (or rebase the heap low and build with
  memory64).
* **Main loop.** `rg_tick` sleeps to keep 32 fps; in a browser it has to be driven from
  `requestAnimationFrame` with the virtual clock (`rg_use_virtual_clock`), and `Sleep` must not
  block.
* **Network.** `net.c` uses BSD sockets and a thread; in the browser that becomes a WebSocket
  (`wss://resetgeneration.rustyapps.com/ws` is already served) with callbacks.
* **Data.** The ~35 MB of paks are fetched into the virtual file system (IndexedDB for saves).
* **Size/speed.** ~90 MB of generated C; the per-function dispatch table and the code size are
  the main risks for load time.

## Android app

Install the debug APK by sideloading: copy `dist/ResetGeneration-Revived-<version>.apk` to the
phone and open it (allow "install unknown apps" for the file manager), or
`adb install -r dist/ResetGeneration-Revived-<version>.apk`.

* NativeActivity with no Java code; min SDK 26 (Android 8), target 36; arm64-v8a + x86_64,
  16 KB page-aligned native library.
* Game data ships as assets and is copied to internal storage on first launch of each build
  (saves are kept). The paks are pre-patched for the 640×360 engine size (`rg.PatchTool`).
* The window buffer is 720 px high at the screen's aspect ratio; the game is drawn 2× in the
  middle and the touch controls sit in the side bars: d-pad (left), Action (right, tap/hold
  like Ctrl), Select and Back (top corners). Tapping the engine's "Select"/"Back" soft-key
  labels at the bottom corners works too. Gamepads and keyboards: d-pad/stick, A = Action,
  Start/X = Select, B/Back = Back; on-screen controls hide while a pad is used.
* **Drop button** (above Action): a tap that holds Ctrl for 700 ms by itself. The engine drops a
  piece when Ctrl has been held 500 ms (`FUN_100d4ed0` polls the key-down time), which is awkward
  on a touchscreen.
* The tutorial only accepts a drop at its forced target (position and rotation compared in
  `FUN_100d4c00`; the target is the blinking outline). The piece cursor starts one cell off and its
  ghost sits over existing blocks, so it is nearly invisible. Move it with the d-pad, tap Action
  to rotate, then drop. A tutorial popup swallows the first key press while it is shown.
* **Tutorial dialogue centring.** The tutorial / story dialogue screen (speech box, text, portrait)
  is laid out for 240 px width. The recompiler (`recomp.rs`) has a `PATCHES` table that shifts its three x
  positions by (640 - 240) / 2 = 200 at recompile time (`FUN_100b8570`: box centre `0x78`, text
  `0x4b`, portrait x). Constants are for the native 640x360 build.
* **Win/lose screen centring.** The end-of-match scenes (`menus.pak` `heroNN_win` / `_loseNN`,
  `end_arena_menu`) are drawn by `FUN_100dafa0` straight onto the screen with 240-wide coordinates.
  The recompiler (`XOFF_FUNCS`, `XOFF_DRAW`) sets `rt_xoff = 200` while that function runs, and the
  engine's device draw `FUN_10052de0` (surface, &pos, &rect, frame) adds it to pos.x (restored on
  return); draws coming from the text-box widgets (return address 0x100a2xxx) are skipped because
  they centre on the real screen width themselves. Verified in the headless driver with the
  engine's built-in `WIN_LOSE_TEST` screen (the debug "View" animation), sprites, labels and
  "ABUSED BY" text centred; a real match end was not reached. The debug screen is opened with
  `RG_DEBUG_WINLOSE=<tick>` (`engine.c`): after the main menu is up, DOWN x3 + RETURN on "View".
* **Ghost / highlight clipped on the right.** The board's tile views (the block-placement ghost,
  highlight layers; `FUN_100867c0`) draw only the viewport columns `[start, grid+0x18+1)`; `grid+0x18`
  is 13 (sized for 240 px). On 640 the centred board starts at viewport column 4, so tile columns 10-11
  were never drawn. The recompiler patches the viewport load at `0x10086822` (`VIEW_COLS_EXTRA`); the
  Windows build gets the same fix from `DllPatcher` (31 columns).
* **Tutorial piece ghost.** The ghost of the piece being placed lives in an overlay layer drawn
  under the blocks, and the tutorial starts the cursor on top of existing blocks, so it was
  invisible. `hook_tile_tool` (engine.c, hooked at `FUN_100d5350`, the placement tool's
  activation) moves the start of the cursor 3 cells left of the forced target when one is set.
  Full path: move Right x3, rotate once, hold (or tap Drop).
* The native host clears the framebuffer before every tick (the Java host's `CLEAR_EACH_FRAME`);
  a tick that draws nothing keeps the previous frame.
* Audio: AAudio, the engine's 8 kHz mono mix resampled to the device rate.
* Pause/resume freezes the engine clock so its 32 fps limiter does not race on resume.
* Online: `files/revived.ini` in the app's internal storage holds `server=` and `nickname=`
  (written on first launch with the build's default server and a random name).

## Testing

`cargo x native-test [FRAMES] [--size WxH] [--keys "F:KEY,…"] [--shots F,F] [--realtime]` (or
`build/native/rgtest <dll> <data dir> <frames> <out dir> …` directly) runs the engine (virtual clock unless `--realtime`), writes `hashes.txt` (framebuffer hash per
frame) and PPM screenshots. `RG_NICK` / `RG_SERVER` set the online identity.

Verified (2026-10-17): boot, intro, menus, wipes, tutorial with input on the desktop driver;
the APK on an Android 15 x86_64 emulator (intro, menu, touch d-pad/Select into a mode);
Quick Match through login, casino and matchmaking against the local relay.

Verified (2026-11-25): the desktop game on Windows (intro, menus, an online match joined through
the public server against a Windows host); the APK on an Android 17 x86_64 emulator (`cargo x
android-run`, `android-keys`, `android-shot`): intro, menus and an online match against a
Windows host. Linux and macOS builds come from CI and were not run here.

Differential testing against the original: `RT_API_TRACE=file rgtest …` logs every Win32/JNI
call; compared with the same log from the emulated original DLL, the first divergence pointed at
each bug (155,659 calls matched after the fixes).

Debug build (`cargo x build-native-test --calltrace`): crashes print the last 48 function
entries and the guest call stack; `cargo x build-native-test --trace a,b` logs entries of chosen
functions; `RT_LIVE_TRIGGER=addr:N` logs the N function entries after `addr`;
`RT_NET_DEBUG=1` logs incoming packets.

## Known limits

* On 20:9 phones the side bars are narrow, so the d-pad and Action button overlap the edges of
  the game image (translucent). A layout option (smaller game, wider bars) would help.
* Debug-signed APK (`build/android/debug.keystore`); a release key is needed for Play.
* C++ exceptions / SEH unwinding are not implemented (`RaiseException` aborts); none were seen.
* The native net client speaks plain `ws://` only (no TLS yet).

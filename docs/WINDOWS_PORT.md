# Widescreen, and the Windows build

The Windows game is the native desktop build (`cargo x build-desktop`, `docs/NATIVE_PORT.md`,
"Desktop game"): the engine statically recompiled, one `ResetGeneration.exe` plus `data/`.
From 2026-10-16 to 2026-11-25 Windows had a different build, "Reset Generation Revived" for the
JVM: the original engine DLL in a trimmed 32-bit Temurin 17 runtime with a new launcher, patched
Java classes, `DllPatcher` (code patches in a copy of the DLL), `PakPatcher` (data) and
`rghost.dll` (hosting). It found the widescreen fixes below; the native build applies the same
fixes in the recompiler and the build-time pak patcher, so the Java build was retired (sources in
the git history: `src/java/`, `native/win32/`, `tools/rgtool/src/windows.rs`).

## What changed vs. the 2020 standalone

| Problem in the 2020 build | Fix (where it is now) |
|---|---|
| Fixed 240×320 portrait screen, integer zoom only | The engine is initialised at a **widescreen size** (640×360) and scaled to the window with the aspect ratio kept. The engine re-lays-out its own UI, so menus, HUD and the playfield use the extra space — a whole 12×12 arena fits on screen. |
| Ancient bundled JRE 6 | No runtime at all: native code. |
| No fullscreen | **Borderless fullscreen**: F11 or Alt+Enter, remembered in `revived.ini` (Windows). |
| Above 240×320 the game crashed on the first screen transition (and showed striped wipes) | The wipe masks in `base.pak` are 240×320 and the blit loops over the whole screen: the masks are **regenerated at 640×360** (`tools/rgtool/src/pakpatch.rs`, at build time). |
| Above 240×320, screens left stale pixels where the original art did not reach | The engine framebuffer is **cleared before every frame** (`native/rt`). |
| Cutscenes drawn in the top-left corner and cut off at the bottom | Cutscene scripts in `intro.pak`/`story.pak` use absolute 240×320 coordinates; they are **offset to the centre** (`pakpatch.rs`). |
| Attack cut-ins ("fight" popups) drawn against the left edge | Their frame and 240×320 surface are drawn at hard-coded positions; the recompiler **patches those two draw positions** (`PATCHES` in `recomp.rs`, `POPUP_DX`). |
| End-of-match win/lose scenes drawn left | The scene draw `FUN_100dafa0` uses 240-wide coordinates; while it runs, the sprite/text draw `FUN_10052de0` adds an x offset (`XOFF_FUNCS` in `recomp.rs`). |
| Speech boxes in cutscenes/tutorial at the left | Box centre, text and portrait x patched (`DIALOG_DX`). |
| Block-placement ghost invisible over the right-most board columns | The engine's tile views only draw 13 viewport columns (sized for 240 px) and the centred board starts at column 4; the viewport load at `0x10086822` is widened (`VIEW_COLS_EXTRA`). |
| Launched via `start.bat` | `ResetGeneration.exe` with an icon. |

Resolution: the Java build could run the engine at other sizes (`render_height=`, 240×320 up to
1280×720; 1280×720 works but sprites become tiny and cutscene art cannot fill it). The native
builds are fixed at 640×360 — on any 16:9 monitor that is an exact ×2 (720p), ×3 (1080p) or ×4
(1440p); other aspect ratios get black bars.

## Known issues / polish backlog

1. Some **menu backgrounds** are narrower than 640 px (e.g. the opponent-select space backdrop),
   leaving black on the right. Menus are laid out correctly; it is art coverage only. Fix idea:
   extend `pakpatch.rs` to centre or tile `menus.pak` background sprites.
2. Cutscene backdrops are 240 px wide: centred with black (or tiled effect layers) either side.
   A nicer frame (blurred/extended art) would need new art.
3. The game folder must be **writable** (saves live in `data/`): don't install under
   `Program Files`.
4. No **gamepad** on the desktop (minifb has none); Android has gamepad support.
5. Audio is the engine's original 8 kHz mono mix — faithful to the original, but low fidelity.
6. Windows "Smart App Control"/SmartScreen may warn about the unsigned exe on other PCs.

## Developer loop

`cargo x run [--nickname=… --server=…]` starts the built game (log in `build/logs/run.log`);
`cargo x keys "RETURN,DOWN,WAIT500" name` sends keys to its window and takes a screenshot
(`build/shots/name.png`); `cargo x lan-test 3 [--server=lan]` starts a host and two clients on
this PC; `cargo x killgame` ends them. `tools/testdata/single_player_game.save` is a
post-tutorial save for testing (copy it into `data/`).

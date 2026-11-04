# Plan: centre the win/lose screens in the Windows build

Goal: the end-of-match win/lose scenes are centred on the 640x360 (or any wide) screen in the
Windows build, as they already are in the Android/native build (commit `33552bc`). Work on `dev`;
`main` only on explicit instruction. Read `CLAUDE.md`, `docs/WINDOWS_PORT.md` and
`src/java/rg/DllPatcher.java` first.

## What is wrong, and what the Android fix does

The scene draw `FUN_100dafa0` (RVA 0xdafa0) draws the win/lose layers straight onto the screen with
240-wide coordinates, so on a wide screen they hug the left edge. Shifting the scene scripts
does not work (tried and reverted, see `PakPatcher.SCENE_PAKS`).

The Android build (`tools/recomp/recomp.py`, `XOFF_FUNCS` / `XOFF_DRAW`, `rt_xoff` in `native/rt`)
does this:

1. While `FUN_100dafa0` runs, a global `xoff` = `(screenWidth - 240) / 2` (200 at 640 wide).
2. The engine's device draw `FUN_10052de0` (RVA 0x52de0; args `surface, &pos, &rect, frame`, so
   `[esp+4]`, `[esp+8]` at entry) adds `xoff` to `pos.x` (a dword at `[pos]`) before drawing and
   subtracts it again before returning (callers reuse the pos struct).
3. Draws whose return address is in 0x100a2000-0x100a2fff (the text-box widgets, e.g. the
   "ABUSED BY" text) are skipped, because they already centre on the real screen width.

## Windows implementation (x86 patch, same style as the fight-popup patch)

`DllPatcher` patches the original DLL bytes at launch (the original is kept as `*.dll.orig`;
patch sites are verified first). Add, in the same code cave area as the popup patch (unused tail of
.text, RVA 0x176550+; extend the cave and the `.text` VirtualSize fix-up if needed):

1. **A writable dword for `xoff`.** .text is not writable, so find unused, writable space (the tail
   of .data / .bss; check section flags with `tools/pe_info.py`, or add a section) and use its
   absolute address (the image base is 0x10000000; it is relocatable, so either add a base
   relocation or address it with a fixup consistent with how the popup cave is placed).
2. **Wrap `FUN_100dafa0`:** at its entry (RVA 0xdafa0: `SUB ESP,0x3c; PUSH EBX; PUSH EBP; PUSH ESI;
   PUSH EDI`) jump to a cave that saves `xoff`, sets it to dx, runs the displaced instructions and
   continues. Its returns (find every `RET` of the function in `decompiled/native/pcode.txt`)
   must clear `xoff` back: either patch each `RET` to jump to a cave that clears it, or, simpler,
   turn the entry into a call to a thunk that calls the real body then restores `xoff`
   (the function is `__thiscall`, args on the stack, callee cleans with `RET n`, so mind `n`).
3. **Hook `FUN_10052de0`** (entry bytes `83 EC 08  56  8B 74 24 10`): jump to a cave that, if `xoff`
   is non-zero and the return address `[esp]` is not in 0x100a2000-0x100a2fff, does
   `add dword [pos], xoff` (`pos = [esp+8]`), calls the displaced original code, and subtracts
   again after the call. Easiest: the cave is a wrapper that calls the original body (a
   `call` into the original entry shifted past the displaced instructions) then undoes the add and
   returns with the same `RET n`. Be careful: this is the hottest draw function, so a bug
   corrupts every draw; keep the fast path (`xoff == 0`) to a compare and a jump.
4. Bump `DllPatcher.VERSION` (the stamp is `VERSION:dx,dy[,w]`, so existing installs re-patch),
   compute dx from the screen width like the popup does (`(width - 240) / 2` only when `width > 240`),
   and add the new patch sites to `matchesOriginal` so a wrong DLL build leaves everything alone.

The Android code in `tools/recomp/recomp.py` (`XOFF_*`) and its P-code
(`decompiled/native/pcode.txt`, `grep "^I 100dafa0"` etc.) is the reference for exact behaviour.

## Verification

* **Oracle:** `tools/emu/rgemu.py` runs the DLL under Unicorn, so the patched DLL can be run
  without the Windows launcher; compare frames against the Android build.
* **Getting to the screen:** the shipped menus do not link the engine's debug "WIN/LOSE ANIM." screen.
  The Android driver opens it with `RG_DEBUG_WINLOSE=<tick>` (`native/rt/engine.c`): then DOWN x3
  and RETURN on "View" plays the win/lose animations. For the emulator, replicate it by calling
  `FUN_100c7c70(newobj(0x20), menu_ctx)` and pushing the screen through the menu flow object
  vtable `+0x14`, as `debug_winlose()` in engine.c does (the menu flow object is the ECX at
  `FUN_1008f500`, ctx = `[menu + 0x10]`), or just play a real battle to the end with the Windows launcher
  (`tools/k.sh` sends keys and screenshots).
* Screenshot: unpatched (left-aligned) vs patched (sprites, labels and text centred) at 640x360.
  Also check that the main menu, tutorial dialogue, fight cut-in and the debug test menu are
  unchanged (the `xoff` must be 0 outside the scene draw).
* Test data: a save past the tutorial lets the engine skip the forced tutorial
  (`single_player_game.save` in the game folder).

## Finish

Update `docs/WINDOWS_PORT.md` (remove the "still left-aligned" note, item 2 under Known issues),
build with `tools/build_windows.sh` (close any running game first, it deletes `dist/`), and cut a
`1.1.1` GitHub release as before (`gh release create` with the win32 zip and the APK; see release
v1.1.0). Commit to `dev` with the Co-Authored-By line; do not push `main`.

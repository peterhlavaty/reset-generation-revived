RESET GENERATION REVIVED
========================

Reset Generation (RedLynx / Nokia, 2008) running natively on Windows 10/11
in widescreen HD. Single-player: Tutorial, Story Mode and Arcade (you vs up to 3 AI).

RUN
  Double-click ResetGeneration-Revived.exe. No Java install needed.
  Keep this folder somewhere you can write to (Desktop, Documents, Games),
  not "Program Files": the game saves progress and settings inside it.

CONTROLS (keyboard)
  Arrow keys ........ move cursor / blocks / menu selection
  Ctrl .............. action: tap to rotate blocks or select, HOLD to drop blocks
  Enter ............. Select (left soft key)
  Esc ............... Back / skip intro
  F11 or Alt+Enter .. toggle fullscreen
  Keys can be remapped in Settings > Custom Keys.

HOW TO PLAY (short)
  Rescue your Princess and bring an enemy Princess to your Castle.
  Each round has simultaneous phases:
    1. Blocks  - drop Tetris-style blocks; five starred blocks in a row form a
                 Combo. You move farther and fight better on your own Combos.
    2. Cannons - shoot two enemy blocks (Combos and Heroes are immune).
    3. Move    - pick a path with the arrows, press Ctrl to move/attack/pick up.
  Items (grenades, batteries...) and each hero's special Power add chaos.

SETTINGS
  See app\revived.ini (resolution, fullscreen, scaling filter).

MULTIPLAYER (2-4 players, LAN or one PC; nothing to install or type in)
  1. The host presses F5 in the main menu, chooses the number of players
     (Left/Right, Enter) and a hero (Esc cancels). The first time, Windows
     asks to allow the game on the network: allow it (private networks), the
     host's game is the server (port 8787). The title bar then shows
     "hosting on <address>, waiting for players".
  2. The others choose Quick Match and pick a hero: their game finds the host
     on the network by itself. When enough players have joined, the match
     starts. The host plays too.
  After a match the host presses F5 again (the title bar says so) and the
  others choose Play Again.
  If the others cannot find the host ("Could not connect"), double-click
  LAN-Setup.cmd on the host PC and confirm the administrator prompt. It
  opens port 8787 (TCP and UDP) in Windows Firewall to the local network
  only, in any network type, and removes rules that block the game (made
  when the firewall question was cancelled). Once per PC is enough;
  "LAN-Setup.cmd -Remove" undoes it. An antivirus with its own firewall
  (ESET, Norton, McAfee...) replaces Windows Firewall: LAN-Setup.cmd warns
  about it, and the game (or port 8787) has to be allowed in that program.
  A VPN can also hide the PC from the local network: turn it off.
  If a player's game crashes or loses the connection during a match, the
  host keeps their place for 3 minutes (their turns time out meanwhile):
  that player starts the game again with the same name and chooses
  Online -> Quick Match. They go back into the same match, with the same
  colour, hero and board. If they do not, the match goes on without them.
  (If the host quits, the match ends for everyone.)
  To try it alone on one PC, start the .exe several times (the second one
  can use: ResetGeneration-Revived.exe --nickname=Bob).
  Over the internet: run your own relay (the project's server/ folder) and
  set server= in app\revived.ini on every PC.

NOTES
  * If the host quits, the match ends for everyone. "Online" (friendly casinos,
    rankings) is not implemented.
  * On first launch the game adapts its data files to your resolution
    (originals are kept as *.pak.orig).

Credits: Reset Generation (c) 2008 RedLynx / Nokia. 2020 standalone revival by
curoviyxru, bodyz and bs0dd. Widescreen/HD launcher: Reset Generation Revived.

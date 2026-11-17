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

MULTIPLAYER (2-4 players, LAN or one PC)
  1. Someone runs the relay server (needs Node.js, from the project's server/
     folder):  npm install  then  npm start   (listens on port 8787).
  2. Every player sets server= in app\revived.ini to that PC, e.g.
     server=ws://192.168.1.20:8787/ws   (ws://127.0.0.1:8787/ws on the same PC).
  3. The host presses F5 in the main menu. The title bar shows
     "hosting, waiting for players". host_players= in revived.ini sets the
     number of players including the host (default 2).
  4. The others choose Quick Match and pick a hero; when enough players have
     joined, the match starts. The host plays too: their game is the server.
  To try it alone on one PC, start the .exe several times (the second one
  can use: ResetGeneration-Revived.exe --nickname=Bob).

NOTES
  * If the host quits, the match ends for everyone. "Online" (friendly casinos,
    rankings) is not implemented.
  * On first launch the game adapts its data files to your resolution
    (originals are kept as *.pak.orig).

Credits: Reset Generation (c) 2008 RedLynx / Nokia. 2020 standalone revival by
curoviyxru, bodyz and bs0dd. Widescreen/HD launcher: Reset Generation Revived.

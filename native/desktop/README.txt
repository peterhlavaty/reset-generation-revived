RESET GENERATION REVIVED
========================

Reset Generation (RedLynx / Nokia, 2008) as a native program for Windows, Linux
and macOS, in widescreen. Single-player: Tutorial, Story Mode and Arcade (you vs
up to 3 AI); multiplayer for 2-4 players over the internet or a LAN.

RUN
  Windows: double-click ResetGeneration.exe (if Windows says "Windows protected
  your PC": More info -> Run anyway; the game is not signed by a publisher).
  Linux/macOS: run ./ResetGeneration (macOS: the first time macOS blocks it, as
  it is not signed: System Settings -> Privacy & Security -> Open Anyway).
  Nothing to install. Keep this folder somewhere you can write to (Desktop,
  Documents, Games), not "Program Files": the game saves progress and settings
  inside it (data/, revived.ini, ResetGeneration.log).

CONTROLS (keyboard)
  Arrow keys ........ move cursor / blocks / menu selection
  Ctrl .............. action: tap to rotate blocks or select, HOLD to drop blocks
  Enter ............. Select (left soft key)
  Esc or Backspace .. Back / skip intro
  F11 or Alt+Enter .. toggle fullscreen (Windows; elsewhere maximize the window)
  F5 ................ host a multiplayer game (main menu)
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
  revived.ini (written on the first start): server, nickname, window scale,
  fullscreen, hosting defaults. Each line can also be given on the command
  line, e.g. ResetGeneration --nickname=Bob --scale=2.

MULTIPLAYER (2-4 players over the internet, on a LAN or on one PC; nothing to set up)
  1. The host presses F5 in the main menu, chooses the number of players
     (Left/Right, Enter) and a hero (Esc cancels). The host's game is the
     match's server; the title bar shows "Hosting, waiting for players".
  2. The others choose Quick Match (main menu) and pick a hero. The Players
     in Match screen shows whose game they are in (the host and the players
     already in it). When all seats are taken, the match starts. The host
     plays too.
  Quick Match picks a game hosted from the player's own home network first,
  otherwise the game hosted most recently. Wrong host? Back, then Quick Match
  again: the next waiting game.
  After a match the host presses F5 again (the title bar says so) and the
  others choose Play Again.

  Where the games meet (server= in revived.ini):
    auto    (default) a game hosted on this network if there is one, else
            the public server resetgeneration.rustyapps.com. Without internet
            the host's game runs the LAN server itself (port 8787) and the
            others find it on the network.
    online  always the public server.
    lan     only this network.
    ws://address:port/ws   a relay you run yourself (rg-relay from the
            project's server/ folder).
  LAN without internet: the first time Windows asks to allow ResetGeneration
  on the network: allow it. If the others cannot find the host ("Could not
  connect"), double-click LAN-Setup.exe on the host PC and confirm the
  administrator prompt: it opens port 8787 (TCP and UDP) to the local network
  only and removes rules that block the game. "LAN-Setup.exe --remove" undoes
  it. An antivirus with its own firewall (ESET, Norton, McAfee...) has to
  allow the game itself. A VPN can hide the PC from the local network.
  (Linux/macOS: allow TCP and UDP port 8787 from the LAN in your firewall.)

  If a player's game crashes or loses the connection during a match, the
  host keeps their place for as long as the match runs (their turns time out
  meanwhile): that player starts the game again with the same name and
  chooses Quick Match. They go back into the same match, with the
  same colour, hero and board. (If the host quits, the match ends for
  everyone.)
  To try it alone on one PC, start the game several times (the second one
  with another name: ResetGeneration --nickname=Bob).

NOTES
  * "Online" friendly casinos and rankings are not implemented.
  * The Android version (APK) plays with the same server and can host too.

Newest version, Android app: https://github.com/peterhlavaty/reset-generation-revived
Play in the browser, no download: https://resetgeneration.rustyapps.com/

Credits: Reset Generation (c) 2008 RedLynx / Nokia. 2020 standalone revival by
curoviyxru, bodyz and bs0dd. Reset Generation Revived: the engine statically
recompiled to native code, widescreen, multiplayer servers; a fork of
github.com/rcadden/reset-generation-revived, where the project began.

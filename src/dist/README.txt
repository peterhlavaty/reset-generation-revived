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

MULTIPLAYER (2-4 players over the internet, on a LAN or on one PC; nothing to set up)
  1. The host presses F5 in the main menu, chooses the number of players
     (Left/Right, Enter) and a hero (Esc cancels). The host's game is the
     match's server; the title bar shows "hosting, waiting for players".
  2. The others choose Online -> Quick Match and pick a hero. When enough
     players have joined, the match starts. The host plays too.
  After a match the host presses F5 again (the title bar says so) and the
  others choose Play Again.

  Where the games meet (server= in app\revived.ini):
    auto    (default) a game hosted on this network if there is one, else
            the public server resetgeneration.rustyapps.com. Without internet
            the host's game runs the relay itself (rg-relay.exe, port 8787)
            and the others find it on the network.
    online  always the public server.
    lan     only this network.
    ws://address:port/ws   a relay you run yourself (rg-relay from the
            project's server/ folder).
  LAN without internet: the first time Windows asks to allow rg-relay.exe on
  the network: allow it. If the others cannot find the host ("Could not
  connect"), double-click LAN-Setup.exe on the host PC and confirm the
  administrator prompt: it opens port 8787 (TCP and UDP) to the local network
  only and removes rules that block the game. "LAN-Setup.exe --remove" undoes
  it. An antivirus with its own firewall (ESET, Norton, McAfee...) has to
  allow rg-relay.exe itself. A VPN can hide the PC from the local network.

  If a player's game crashes or loses the connection during a match, the
  host keeps their place for as long as the match runs (their turns time out
  meanwhile): that player starts the game again with the same name and
  chooses Online -> Quick Match. They go back into the same match, with the
  same colour, hero and board. (If the host quits, the match ends for
  everyone.)
  To try it alone on one PC, start the .exe several times (the second one
  can use: ResetGeneration-Revived.exe --nickname=Bob).

NOTES
  * If the host quits, the match ends for everyone. "Online" (friendly casinos,
    rankings) is not implemented.
  * On first launch the game adapts its data files to your resolution
    (originals are kept as *.pak.orig).

Credits: Reset Generation (c) 2008 RedLynx / Nokia. 2020 standalone revival by
curoviyxru, bodyz and bs0dd. Widescreen/HD launcher: Reset Generation Revived.

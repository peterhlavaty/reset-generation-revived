# Multiplayer: our own server for Quick Match / Online

Status (2026-11-17):

| Piece | State |
|---|---|
| **Hosted games (Windows build)** | **Works, 2–4 players** — F5 makes a player's own engine the game server (the original LAN host mode) and they play as player 0; Quick Match puts the others into that game. See "Hosted games" |
| Relay server (`server/`, Node + Cloudflare Worker) | **Done**, 19 tests (`npm test`) |
| Built-in LAN relay (Windows, `src/java/rg/relay/`, Netty) | **Done** — the host's game runs it, clients find it by UDP broadcast (`server=auto`, the default); see "Built-in LAN relay" |
| Client: Windows (Java replacement for `SNAPMobileClient`) | **Done** — logs in to the relay with a nickname, no Arena login screen |
| Client: native/Android (`native/rt/net.c`) | **Done** (plain `ws://` only) |
| Casino system (login, casino enter, Quick Match queue, lock, matched-up) | **Done** — both players listed in "Players in Match", "Game starting…" |
| Game room bot (`server/src/gameroom.js`: handshake, seating on 0x15, forwarding) | **Done** — both players seated in a shared room |
| Game server (channel 0x16: world setup, then the match) | **Phase 0 works**: a headless second engine instance (`rgtest --server`) serves M1..M4 and the client shows the match screen; a crash in the first game event is open (see "Phase 0 result") |
| Friendly arenas, rankings, statistics, achievements, friends | Answered with empty/OK replies so the client does not stall; not implemented |

## Hosted games (Windows build) — working multiplayer

The shortest path to a real match: no second engine instance and no recompiler. The original
game had a **LAN host mode**: the host screen `0x100caa80` runs the server-mode controller and the
host plays as local player 0; clients joined over `tcp://`, which this build lacks. `rghost.dll`
(`native/win32/rghost.c`, a 32-bit JNI helper that `rg.HostServer` loads into the game's JVM) does
in the real engine DLL what `native/rt/server.c` does for the recompiled one:

* at start: hooks the menu flow constructor `0x1008f500` (inline `jmp` to a trampoline that
  records `ECX`). The engine DLL is usually relocated, so every address is rebased;
* on **F5** (or `host=auto`): patches the TCP acceptor's `setController` (vtable `0x10188b64`,
  slot 1) to install a fake listener. F5 first shows the engine's own hero select
  (`HeroSelectionScreen`, ctor `0x100dcff0`, the ten heroes plus Random) with a private copy of
  its vtable (`0x101888e0`) whose onSelect/onBack (slots 11/12) run in rghost: Select replaces it
  with the LAN host screen, Back cancels. `host_hero=NAME|RANDOM` (and `host=auto`, default random)
  skip the select. The LAN host screen gets a hero list of `host_players` entries;
* connects to the relay as `RGHost` and sends `{"t":"serve","room":"HOST_<nick>_<n>","players":N-1}`.
  Each `player_joined` becomes a connection object (`accept`); channel 0x16 travels as
  `[u16 pid][zlib message]` exactly as in the Phase 0 design. `HostServer.pump()` runs on the
  engine thread after every tick.

Relay side: `serve` with `players` registers a hosted game with the casino; `matchmake()` fills
hosted games first (once enough players are queued), and the node server's `--match-size`
defaults to 0, so Quick Match never forms `QM_n` rooms that have no game server. When the host's
engine closes the listener (match over, or the host left the host screen) rghost sends
`{"t":"unserve"}`, so no one is matched into a dead room, and the title says "F5 = host a new
game" (`host=auto` re-hosts by itself). Once a game server serves a room, the game room stops
sending EVENT SEAT_LEAVE (the match reports leavers itself): a client still on its win screen that
got it stayed on a black screen. The Java clients need no change.

The host's hero and name: the LAN host screen's entry (`0x100ca920`) rolls the host's hero
(`rand() % 10` at `0x100ca9ce`) and hands the host's player setup, name left empty, to the
controller (`0x100caa30`, vt+0x6c), which sends it to every client. rghost detours both (13-byte
signature checks): the roll keeps its RNG call but takes the chosen hero, and the setup gets the
host's nickname, so the end screens say "Host Wins" instead of a blank name.

Engine bug fix (rghost.dll, applied in every instance, host or client): `PlaceTileUIState::enter`
(`0x100b2c20`) sets the state's tile-layer grid (`this+0x84 = this->layer(2)`) only for players
still in the game, but `update` (`0x100b5df0` -> `0x100b5290`) copies board snapshots into it for
everyone. A player who had surrendered (player state 5) therefore wrote through an uninitialised
pointer: a crash at `0x100b5311`/`0x100b5325` or a silently corrupted heap (black screen), seen
for the last player to surrender in 3-4 player games. Both early exits of `enter` now set the grid
first (a 26-byte signature check guards the patch).

Second engine fix (same place): the online user object (context service 0x39, vtable
`0x1017b020`) belongs to the app context, yet a casino room also puts it in its member map under
the player id from the profile (`0x10047573`). Leaving the room removes only the current id
(`0x100482c0` -> `0x1000aee0`) and then deletes every remaining member (`0x10094e50`). When the
id had changed between casino visits (our casino used to hand out a new id per join, so after any
match), the stale entry deleted the context's user object; the next tick after a dropped
connection called through it (`0x100ecf90`) and the client crashed a few seconds after losing the
relay. The member delete at `0x10094eac` now skips the context's own object, and the casino keeps
a player's id per nickname (`Casino.playerId`, `casino.js` `playerId`), which alone also avoids it.

Verified on one PC with the real Windows build (`tools/lan_test.sh 2|3|4`): 2-, 3- and 4-player
matches start; boards, items, heroes and health stay identical on all screens over many rounds;
a client quitting mid-match leaves the host running; when the players surrender one by one every
client reaches the win screen, Battle Stats and the casino (2, 3 and 4 players, any order); the
host then hosts again and "Play Again" starts the next match. The host's chosen hero (picked in
the select, Random, or `host_hero=`) is the one every screen shows, and the host's nickname
appears in Battle Stats. Known limits: if the host quits the match ends for everyone; Android
clients should work through the same relay but were not tested.

### Built-in LAN relay (no setup)

With `server=auto` (the default) nobody runs `server/` or types an address:

* **F5** first shows a Java2D overlay drawn into the engine's backbuffer
  (`HostServer.drawPicker`, called from `ResetGeneration.drawBuffer`): number of players 2–4
  (Left/Right or 2–4, Enter, Esc). Keys reach it through Revived's key dispatcher and are kept
  from the engine; the choice is saved as `host_players`. Then the engine's hero select.
* The host's game then starts `rg.relay.LanRelay` (once per process): Netty on TCP 8787 (HTTP
  `GET /status`, WebSocket `/ws`) and a UDP responder on 8787, all on one event-loop thread, so
  the relay code (`Relay`, `Casino`, `GameRoom` — a line-by-line port of `server/src/relay.js`,
  `casino.js`, `gameroom.js`; keep them in sync) needs no locks. `rghost` connects to
  `ws://127.0.0.1:8787/ws` as before. If 8787 is taken by another relay (`/status` answers),
  that one is used; by anything else, hosting fails with a message in the title bar.
* Clients: `SNAPMobileClient` login calls `LanDiscovery.find` (up to 20 s): it broadcasts
  `RGR-DISCOVER 1` to every interface's broadcast address, 255.255.255.255 and 127.0.0.1 each
  second; hosts answer `RGR-HOST 1 <port> <waiting hosted games> <host nick>`. A host with a
  waiting game wins; otherwise the first answer, 1.5 s later. Nothing found → the engine's
  "cannot connect" error.
* While the overlay is shown the launcher taps Right (a no-op in the main menu; Shift is Back)
  when it opens and every 10 s: the main menu otherwise replays the intro after ~35 s without
  input. The key that closes the overlay is kept from the engine until released.
* Windows Firewall asks once, on the host only (inbound TCP/UDP 8787). If that prompt was
  cancelled (Windows then adds inbound Block rules for the exe, which win over any Allow) or the
  rule only covers the other network type, clients time out. `LAN-Setup.cmd` in the release
  (`src/dist/LAN-Setup.cmd` -> `app\lan-setup.ps1`, self-elevating) removes those Block rules and
  allows TCP/UDP 8787 from `LocalSubnet` in every profile (rule group "Reset Generation
  Revived"; `-Remove` deletes it). A third-party firewall (ESET, Norton...; listed in
  `root/SecurityCenter2 FirewallProduct`) replaces Windows Firewall and drops the connections
  silently: the script warns and says where to allow the game. A VPN client can hide the PC too.

A manual `server=ws://…` still works everywhere (internet play through `server/` or the Worker).

Test aids (Windows build): `--keys=T:KEY,...` presses keys T ms after start (`ESC`, `RETURN`,
`UP`/`DOWN`/`LEFT`/`RIGHT`, `SPACE`, `F5`, or a hex VK), `--window_x=`/`--window_y=`,
`--nickname=`, `--host=auto`, `--host_players=N`, `--host_hero=NAME|RANDOM`, `--dll_patch=false`
(pristine engine DLL);
`RT_NET_DEBUG=1` logs every 0x16 message in rghost (stderr). `--perflog=N` prints ticks/s, the
slowest tick, Java heap and thread count every N seconds (`HostServer.perf`).

### Dropped connections and going back into a match

A client whose game crashed or lost the network can go back into the match it was in: start the
game again (or go Online -> Quick Match) with the **same nickname** within 3 minutes. Meanwhile
the match goes on; that player's turns time out.

What the engine does on its own is not enough. Its acceptor is polled all match long and the
per-connection handshake (`0x1000d8c0`) takes a late joiner in, but always as a **new** player:
next free key (`0x10023f40`) and seat (`0x10024c60`). A player who came back to a 3-player match
became player 4, with no base and no hero, and crashed on their first move (`0x100b4a90` finds no
hero entity). Giving them the old key and seat is not enough either: once the engine has seen the
connection drop (leave command, `0x1005c530`) it sends that player nothing more. So rghost holds
the seat instead:

* Relay: `player_left` carries `lost: 1` when the connection dropped (not a normal leave or a
  surrender). rghost then does **not** tell the engine: the session stays "connected" (sends to
  it are dropped) for `HOLD_MS` = 180 s, then it is closed and the engine sees the leave. A
  served room is kept while its host serves it, even with no members (in a 2-player match the
  only client's drop emptied it: the player came back into a new room without the host); it
  goes at `unserve`.
* Casino (`Casino.java` / `casino.js`): a hosted room becomes `running` when its players are sent
  into it; a player whose connection drops in it is remembered in `dropped` with the time. Their
  next Quick Match request (case 9) within `REJOIN_MS` = 170 s is answered as usual and then
  starts a one-player match into that room instead of queueing. `unserve` (the host's listener
  closed: match over) forgets the room.
* rghost, host side (`track_seats`): a new connection with the same name as a held session gets
  its old seat (preset in the pending join, so state 3 skips the seat search) and key (hook on
  the key allocator `0x10023f40`). The handshake sends the battle state on the new connection;
  then the held session takes over the new relay player id (the engine keeps using its old
  connection object for that key: `0x1005c340` does not register a key twice).
* The handshake also dispatches a join command (type 0xc) for the player. Applied to a player
  who is already in the game it replaces their player object (`0x10010010`, game+0x14 -> +0x2c
  by seat) and record (`0x10027450`, game+0x1c -> +0x10 by key) with empty ones, on every
  machine: the others crashed a minute later (`0x1004a0f4`). rghost hooks the server
  controller's dispatch (vtable `0x101779a0` slot 9) and sends that one command only to the
  player coming back; the host's game does not apply it.
* The player coming back needs it (their screen stays black without it), but must keep the
  objects from the battle state, or their game dies a few minutes later. rghost.dll is loaded
  by every Windows game, so it also hooks the client's join handler (vtable `0x10188d18` slot
  0x58, `0x100e16a0`): for a key and seat the game already has, the old objects are put back.
  The Android build (recompiled engine, no rghost) does not have this: an Android player who
  comes back is not safe.
* Relay `hello`: names are unique per server, so a restarted client used to become `Player22`
  while its old connection was still open (a crash or a frozen PC leaves a half-open TCP
  connection that only the 60 s idle timer closes). A connection with the same name that has
  sent nothing (not even a ping, every 15 s) for `STALE_MS` = 20 s is now closed first, so the
  player keeps their name and the casino finds them.
* rghost: the engine never destroys connection objects, so session slots are never reused
  (`MAX_SESS` 4096; it was 64, which a long-running host could use up), and nothing is sent on a
  closed session.

Client login state (`SNAPMobileClient`, `native/rt/net.c`): the engine logs in with `login()`
and never calls `startLoginScreen()`, which is what reset `loginErrorState`/`loginCancelled`. One
failed attempt (no host found yet, a dropped broadcast) therefore made every later attempt report
"cannot connect" (and a logout made them report "cancelled") until the game was restarted. A
login request now clears both. A lost connection (`handleError`) also clears the queued actions
except a logout, so a half-finished join cannot run on the dead socket.

## How online worked in 2008–2010

Nokia **SNAP Mobile** (N-Gage Arena) provided accounts, named game rooms and per-room "game
servers". RedLynx's Java glue (`com.redlynx.mt.snapmobile`) exposes rooms and packets to the
native engine; everything above that — the engine's **MT** networking layer, the casino
(matchmaking) protocol, seating, and the match itself — is in the engine DLL.

```
engine MT layer ──JNI──► SNAPMobileClient (Java)  ──►  SNAP server: rooms, game servers
                         replaced by: Java client / native net.c ──WebSocket──► our relay
```

### Engine-facing contract (unchanged)

Engine → host (`NativeSNAPMobileInterface`): `init`, `startLoginScreen`, `getLoginScreenStatus`,
`login`, `logout`, `isLoggedIn`, `createRoom`, `joinRoom`, `moveToRoom`, `leaveRoom`,
`send(len)` (bytes in the shared 2 KB buffer), friends/presence calls.
Host → engine (`NativeSNAPMobileEventListener`): `onArenaLoginCompleted/Cancelled`,
`onConnected` (joined a room), `onDataReceived(len, room)`, `onDisconnectedFromGameRoom`,
`onDisconnectedFromNAF`, `onFriendList`, ….

## Relay protocol (ours)

WebSocket at `/ws`. Text frames are JSON control messages, binary frames are game packets.

| Client → server | Server → client |
|---|---|
| `{"t":"hello","v":1,"name":"Nick"}` | `{"t":"welcome","id","name"}` (name made unique) |
| `{"t":"create"\|"join"\|"move","room":"X"}` | `{"t":"joined","room","members":[…]}` or `{"t":"error","code":"not_found"\|"full"…}` |
| `{"t":"leave"}` | `{"t":"left"}`; others get `peer_joined` / `peer_left` |
| `{"t":"ping"}` | `{"t":"pong"}` |
| `{"t":"rooms"}` | `{"t":"rooms","rooms":[…]}` |
| binary packet (≤ 2048 bytes) | relayed in order to the other room members — except in `CasinoSystem` |

Rooms hold 4 players; `CasinoSystem` is a permanent lobby (1,000) whose packets go to the
casino bot instead of other players. The bot behaves like an MT peer: it opens with a
transport hello (channel 2 `00`) and the keep-alive frame, accepts the client's hello (`20`),
answers its pings and pings every 10 s. Without the hello the client never starts its own
keep-alive and drops the connection ("Connection lost") 20 s after entering the casino.
Because the greeting follows right behind `joined`, the clients enter a room on their WebSocket
reader when `joined` arrives (`SNAPMobileClient.onRelayJoined`, the reader in `native/rt/net.c`),
not on the action thread after the request returns: a greeting that won that race was dropped
as "not in a room", and the client then lost the casino 20 s later now and then (typically
when coming back to the casino after a match).

**Room greeting.** The engine only starts talking in a room after the room's server has
spoken. Plain rooms: the relay sends each joiner one frame (`05 00 01 20 4e 00 00`).
Matchmade rooms get a `GameRoom` bot that greets like the casino bot (channel 2 `00` +
keep-alive) once all matched players are present (max 10 s), because the engine's
handshake packets are lost on players who have not joined yet.

## The engine's wire format (reconstructed)

Inside a game packet: one or more frames `[u16 length][u8 channel][payload]`, little-endian.

| Channel | Use |
|---|---|
| 1 | keep-alive: `u32 20000` on join (peer timeout, ms), then 1-byte sequence pings |
| 2 | transport control: `00` = hello on join, `20` = accept, 1-byte ping replies (same sequence) |
| 3 | key/value "command": session hello (`name`, `pid`, `passwd`, `roomname` = room URL, …) |
| 0x11 | casino-system messages |
| 0x12 / 0x13 | messages to a matchmaker / friendly casino |
| 0x15 | seating in a game room (`SEAT_RESERVE`/`SEAT_TAKE`/`SEAT_LEAVE` + `u32` player id) |
| 0x16 | the game protocol: client ↔ authoritative game server (see below) |

Casino messages: `[u8 type][u8 action]` + fields; type 0 REQUEST, 1 RESPONSE, 2 EVENT; the
42 actions are in `server/src/casino.js` (`ACTIONS`). Strings are the engine's string object
`[u32 refcount|flags][u32 length][u32 capacity][bytes]` (flags bits 28–30 = 2: UTF-16).
Key/value maps: `u32 count` + key/value string pairs. A generic RESPONSE is `[u8 result
(0 = OK)][u32 value]`.

Messages the bot handles (the engine's deserializers are the reference; addresses are in the
engine DLL):

| Message | Layout |
|---|---|
| REQUEST `CASINOSYSTEM_LOGIN` | `u32 1`, string `"Reset Generation v. 1.1.3"` |
| RESPONSE login | generic + profile map + `u8 1` + casino descriptor |
| profile map | `id`, `name`, `fameLevel`, `heroType`, `random` (the client crashes on a missing key) |
| casino descriptor | `u8 kind` (1 matchmaker, 2 friendly), `u32 id`, 3 × `u32`, string name |
| REQUEST `CASINO_ENTER` (ch 0x12) | `u32 casino id` → RESPONSE generic + profile map + casino descriptor |
| REQUEST `CASINO_MATCHMAKING` | string hero (`PLUMBER`…), string random (`FALSE`) → RESPONSE generic + `u8 n` + profile maps of the players already waiting |
| EVENT `CASINO_ENTER` / `CASINO_MATCHMAKING` (8 / 9) | string id + profile map. 9 adds a player to the waiting screen's list (the screen exists while queued); 8 registers it in the client's casino, where the seating events look ids up |
| EVENT `CASINO_CANCEL_MATCHMAKING` (10) | string id: removes a player from the list |
| REQUEST `LOCK_PLAYER_IN_GAME` (29, ch 0x11) | `u32 0` → "Game starting…"; the client answers RESPONSE 29. `UNLOCK_PLAYER_FROM_GAME` (30) shows "matchmaking failed" |
| EVENT `CASINO_MATCHED_UP` | string casino name, table: string room, string address, `u8`, `u8`, `u32` players, settings (`u8 0`, `u8`, string address, `u8`) |

The address is an MT URL: `naf://rg.com:80/QM_1?hosted=yes`. The path is the SNAP room the
client moves to; `hosted` decides whether it creates or joins it. (A `room=` query parameter
breaks the client's room check — leave it out.) The login and casino flow also includes
`GET_STATISTICS`, `GET_ACHIEVEMENTS`, `REGISTER_FRIENDS` and `GET_FRIENDLY_CASINO_LIST`,
currently answered with empty OK responses.

## Running a server

```bash
cd server
npm install
npm start                       # ws://0.0.0.0:8787/ws   (--log-packets to hex-dump traffic;
                                #   --match-size N also forms QM_n rooms for rgtest --server)
npm test
npx wrangler login && npx wrangler deploy     # Cloudflare (Durable Object), wss://…workers.dev/ws
```

Clients: Windows `app\revived.ini` → `server=ws://<host>:8787/ws`, `nickname=…` (default
`server=auto`: LAN play through the host's built-in relay, see "Built-in LAN relay").
Android: `files/revived.ini` in the app's storage (or the APK's built-in default,
`tools/build_android.sh SERVER=ws://…`). The native client speaks `ws://` only for now, so
Android needs a self-hosted server (LAN or a VPS) until TLS is added; the Java client supports
`wss://` (Cloudflare).

## The game room (done)

Quick Match end to end, as served by `casino.js` and `gameroom.js` (verified with two
`rgtest` clients):

1. `CASINO_MATCHMAKING` → the "Players in Match" screen opens while the player waits; it lists
   the players from the RESPONSE and from later EVENTs 9 (not the seating events).
2. Group full → EVENT `CASINO_ENTER` per other member, REQUEST `LOCK_PLAYER_IN_GAME`
   ("Game starting…"), and after every RESPONSE 29 (or 3 s) EVENT `CASINO_MATCHED_UP`.
3. Each player moves to the room (`naf://rg.com:80/QM_n?hosted=yes|no`), is greeted once all
   are present, and sends channel 2 `00`, a channel-3 hello (`name`, `pid`, `roomname`), and
   REQUEST `SEAT_TAKE(pid)` on 0x15.
4. The bot answers RESPONSE `SEAT_TAKE` ok (this creates the client's game session,
   `0x100eb060` → `0x100ead20`) and sends EVENT `SEAT_TAKE` (string id) about every seated
   player to everyone; `SEAT_LEAVE` when a player leaves. Other channels are forwarded.

Both clients then wait for the first game-server message on channel 0x16, which nothing sends.

## The game protocol (channel 0x16) — reconstructed, not yet served

Channel 0x16 is **client ↔ authoritative game server**, not peer-to-peer lockstep.

* Framing: each MT message on 0x16 is one bare zlib stream (zlib 1.2.3, standard header,
  stream end required), no prefix. One message per MT packet. Receive `0x1005d100`
  (`conn->vt+0x7c(0x16)`), send `0x1005d1c0`.
* Message header (`0x1000c020` read, `0x1000d160` write), all `u32` LE: id, flag (1), kind,
  sender, N, recipient[N] (player keys, 0 = server), +0x30 (ignored), +0x34 turn, +0x38,
  +0x3c, +0x40, then the kind payload. Kinds 0–16 (2, 11, 13 invalid; table `0x1000c510`).
* Handshake: server M1 = kind 0 blob (`u32 len` + bytes) holding the whole world
  (`0x1003c4a0` dump, loader `0x1003e440`) + `u32` player key; client M2 = player info
  (name, hero, its player object); server M3 = slot; server M4 = battle state + queued events
  → the client is in the match. In-match messages go to the server, which re-addresses them
  and checks state hashes (mismatch → kind 10, `PLAYER_SYNC_ERROR`).
* Producer: only the engine's **server-mode controller** (`0x1000ecd0` ctor, vtable
  `0x101779a0`, per-frame `0x1000ddf0` → state machine `0x1000d8c0`). Sessions attach only
  through `0x1000d730`, called from the "TCP" acceptor `0x100df540`/`0x100df5f0` of the LAN
  host screen `0x100ca920` (listen URL `tcp://:12345`; join `tcp://%s:%u`). Single-player
  builds a controller too (`0x100debf0`, `0x100dea4a`, `0x100dee2d`), so the engine always
  plays against a local server. The DLL imports no socket API, so the `tcp://` transport
  has nothing underneath it in this build. Strings `MT_GAMESERVER`, `MT_HOSTMIGRATION`,
  `hosted`, `server`, `bluetooth` suggest the MT layer knew game-server and host roles.
* Rebuilding the server in Node is not realistic (world and battle formats are large), so
  the engine's own controller has to run somewhere.

## Next step: game server on the host player's device (plan "D")

**Goal:** the player the matchmaker marks `hosted=yes` also runs a **headless second engine
instance** in server mode (separate process, no rendering/input). It joins the relay room as
the game server; the `GameRoom` bot routes channel 0x16 between each player and it. Players'
clients stay unpatched (the server is the engine's own code), and the relay stays small enough
for the free Cloudflare Worker. Trade-offs accepted: the match ends if the host drops, and the
host has the latency advantage. The same server program can later run dedicated (VPS, fan PC).

### Phase 0 — prove the headless server (desktop, `C:\dev\rgbuild`)

Done = one `rgtest` client gets M1…M4 from a headless server instance and shows the match.

1. Read the LAN host path: `0x100ca920` (host screen) → `0x1000ecd0` (controller, called at
   `0x100ca99e`) and the acceptor `0x100df5f0`/`0x100df540` → `0x1000d730`. Record what the
   controller needs (world/level, hero choices, settings — compare the single-player callers
   `0x100dea4a`/`0x100dee2d` and `0x100debf0`/`0x100de940`) and what interface the session
   objects implement (they are the MT connections the clients' code sends on with
   `vt+0x68`/`vt+0x7c`).
2. Decide the server's transport (first open question). In order of preference:
   a. **The server engine is an MT peer in the relay room**, using the same SNAP/`net.c`
      path as clients: find whether the MT listen/accept can take a `naf://…/QM_n?…server…`
      URL (look at how `hosted`, `server`, `MT_GAMESERVER` are parsed near the URL parser
      and `0x10110210`/`0x1010fd80`), so each player's in-room connection becomes a session.
   b. Otherwise implement the `tcp://` transport the acceptor expects inside the recompiled
      runtime (host-side sockets under the MT transport vtable), and have a small bridge map
      relay players ↔ local TCP connections.
   c. Otherwise create session objects ourselves and call `0x1000d730` from the host.
3. Add a server mode to the desktop host (e.g. `native/host/server/rgserver.c`, or an
   `rgtest --server ROOM` flag): load the engine, skip menus, build the controller like the
   host screen does (`rt_guest_call`), run `tick` on the real clock without drawing. Watch for
   UI objects the controller assumes (the per-frame path may touch screens).
4. Debug with one `rgtest` client: `RECOMP_ARGS="--trace 1000d8c0,1000d730,1005d100,…"`,
   `RT_LIVE_TRIGGER`, relay `--log-packets`. Check the client's 40 s join window
   (`0x1000d730`: `timeGetTime()+40000`) and that M1 arrives only after the client's idle
   frame (`0x100ebed0`).

### Phase 0 result (2026-10-17)

`rgtest <dll> <dir> N <out> --realtime --server ws://host:8787/ws QM_1 --keys "250:ESC"` runs the
engine as the room's game server (`native/rt/server.c`); one `rgtest` client in Quick Match
(relay started with `--match-size 1`) receives M1 (world dump, ~2 KB), M3, M4 and shows the match
screen (two castles on the field). Transport findings:

* **No tcp:// and no multi-peer naf://.** The only MT transport in this DLL is NAF (SNAP rooms);
  `tcp://` URLs fail in the connection factory (`0x10118de0`), and one NAF connection is one room
  membership (`send(len)` has no room argument), so one engine cannot serve several players over
  its own transport. Option (c) is what works: create the sessions ourselves.
* **What the controller needs.** The LAN host screen `0x100caa80` (ctor; arguments: context
  `[menuflow+0x10]`, a list of hero ids whose *length* selects the mission, `MissionID` 2..4 in
  `data/levels/maps/*.xml`; an empty list crashes `0x1003fd50`) and its entry `0x100ca920` create
  the controller (`0x1000ecd0`, mode flags 3) and a TCP acceptor (vtable `0x10188b64`). The
  screen's per-frame code calls the acceptor's poll (slot 2, `0x100df540`), which asks a listener
  object (`[acceptor+8]`, `vt+0x4c` accept) for a connection, wraps it in a session (`0x1005d320`)
  and attaches it (`0x1000d730`). The host builds the screen itself: the hook on the menu flow
  constructor (`0x1008f500`, `rt_hook`) yields the flow object, `host_game()` calls the screen
  constructor and the flow's set-screen method (`vt+0x14`). No menu navigation is needed.
* **We replace the acceptor's slot 1** (`setController`, `0x100df500`, which would listen on
  `tcp://:12345`) with a stub that installs a fake listener and sets `[acceptor+0x18]`. The
  listener hands out "HostConn" objects: guest objects whose vtable implements the connection
  methods a session uses: `+0x14` address object (printed only), `+0x18` state (≥ 2),
  `+0x2c` alive, `+0x5c` close, `+0x68` send(channel, ptr, len) (raw bytes, here the zlib
  stream), `+0x7c` recv(channel, flag) (an MT message object: `0x1011f700(msg, capacity)`, with
  the bytes written through its ByteStream at `+0xc`; the *ostream part at +8* has
  `write(buf, n)` at vtable slot 0 = `0x100f3b90`; slot 0 of the primary vtable is a double
  reader). Anything else is a fatal that names the slot.
* **MT framing for large messages** (the relay's `mtPackets`/`mtMessages`): a message is
  `[u8 channel][payload]`, cut into packets `[u16 header][bytes]`; header = length | piece << 10;
  the pieces of a split message are numbered 1, 2, … and the last one is 0, so a message that
  fits one packet has piece 0 (the earlier "frames" were this with piece 0). Packet payload is
  at most 874 bytes (the fragmenter cap = MTU 876 − 2 from `initNative`). The shift 10 is derived
  from that cap; the casino connection starts with cap 254 (shift 8), but only the game
  connection is fragmented by us.
* **Relay protocol for the server role** (implemented, tested in `server/test`): the server says
  `hello` then `{"t":"serve","room":"QM_1"}`; it gets `{"t":"player_joined","pid":N,"name":…}`
  when the player has taken its seat (its client session exists) and `player_left` (with
  `"lost":1` if the connection dropped); binary frames both
  ways are `[u16 LE pid][zlib message]`. The server pings every 15 s (the relay reaps idle
  connections after 60 s). A `join` of a matchmade room that does not exist yet is treated as
  `create` (the client sends `join` for the `hosted=yes` player).
* **Recompiler**: `resolve_jump_tables` now also completes tables Ghidra only partially resolved
  (`0x10010147` had one of its two targets: "indirect call to unknown address 10010153").
  `DEFAULT_HOOKS` / `--hook` add `rt_hook()` calls at function entries.

**Fixed: crash in the first game event** — a recompiler bug, not the event data. Ghidra's P-code for
`BSR`/`BSF` writes 31 (or 0) to the destination when the source is 0, but hardware leaves it
unchanged, and the compiler relies on that (`MOV EAX,-1; BSR EAX,ECX` at `0x1001e10c`). Cells with
an empty piece mask therefore got type 31 instead of "no change"; `fix_bitscan` in `recomp.py`
now skips the destination write. Original symptom: shortly after the match screen appears both the
client and the server crash in `0x10018940` (the board's "existing combos" check, called from the
event processor `0x1001dbe0`): it indexes a 5-entry colour table with the cell type, and at that
call every cell of the board reads type 31 (the first two calls, during setup, show normal
boards). Not yet known whether the event data from the server is wrong (host-local player 0 is
also in the match, no input), a recompiler fault in the event path, or a state the single-player
path never produces. Debugging aids: `RT_NET_DEBUG=1` (server I/O, menu events),
`RT_HEAP_CHECK=1` (poisoned free blocks), `RT_CRASH_STACK=1` (rgtest prints host return
addresses on a crash; resolve with `llvm-addr2line` against a `-O0 -g` build of the generated
file, then read the `/* guest-address */` comments around that C line). Also still open from the plan: the host-local player 0 should be removed, the
client's 40 s join window and `hosted` semantics for two players, a headless build without the
menu flow, and the relay's idle timeout handling when the server drops.

### Phase 1 — relay and room bot

1. Relay protocol: let a connection join a room as its **game server**
   (e.g. `{"t":"serve","room":"QM_n"}`), not counted as a player and not seated.
2. `GameRoom`: forward channel 0x16 (and whatever else the controller uses) between each
   player and the server member, tagged by player (binary prefix or a per-player virtual
   connection, depending on the Phase 0 transport); greet the server like a peer.
3. Casino: tell the hosted player's client to start the server (it already learns
   `hosted=yes` from `CASINO_MATCHED_UP`; the host-side launcher can key off that), and keep
   seated players waiting until the server member has joined (extend the 10 s start timeout).
4. Tests in `server/test/` for the server role, routing and server-leaves-room handling
   (all players get a clean disconnect).

### Phase 2 — launching the server on the host device

* **Windows:** build the headless server as an x64 recompiled `.exe` in `dist/`
  (`tools/build_windows.sh`); the Java client starts it (room name, relay URL, game dir) when
  it is matched with `hosted=yes`, and stops it after the match.
* **Android:** run the same code in a separate process (a service with `android:process`),
  since the runtime's guest memory and CPU state are global and a second engine cannot share
  the game's process. Add it to `tools/build_android.sh`.
* **Dedicated (optional):** `rgserver --relay ws://… --dedicated` registers with the relay and
  serves rooms when no host is available.

### Phase 3 — Android to Cloudflare

Add TLS (`wss://`) to `native/rt/net.c` so Android can use the Worker; until then Android
needs a self-hosted relay.

### Phase 4 — a full match

Two clients play a whole match: lockstep stays in sync (no `PLAYER_SYNC_ERROR`), the match
ends cleanly back in the casino, and host / non-host disconnects end or continue the match as
expected. Then 3–4 players.

### Open questions / risks

* Can the controller run without its UI (Phase 0.3)? Single-player suggests yes.
* Which transport (Phase 0.2): `naf://` room peer is the cheapest if the MT layer allows it.
* Does `hosted=yes` change the client's MT routing (e.g. expecting the game server on its
  own side)? Test a client with `hosted=no` for both players if routing gets in the way.
* Server CPU on low-end phones: the server does not draw, so it should be cheap.

Debugging tools: `tools/build_native_desktop.sh` (`EXTRA_CFLAGS=-DRT_CALLTRACE`,
`RECOMP_ARGS="--trace <hex,…>"`; delete `C:\dev\rgbuild\gen\rg_gen_table.c` to force
regeneration when only `RECOMP_ARGS` changed), `RT_LIVE_TRIGGER=<addr>:<n>`, `RT_NET_DEBUG=1`,
relay `--log-packets`. Two clients: `RG_NICK=Alice …` / `RG_NICK=Bob …` with keys
`250:ESC,350:DOWN,400:RETURN,700:RETURN,800:RETURN,900:RETURN` (`--realtime`).

// Game server role: channel-0x16 messages are reassembled from MT packets, routed to the
// serving process tagged with a player id, and its replies are split into MT packets.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { GameRoom, CH_GAME, CH_TABLE, SEAT_TAKE, mtPackets, mtMessages, MT_PIECE } from '../src/gameroom.js';
import { Writer, frame } from '../src/casino.js';

function setup() {
  const sent = [], ctl = [], toServer = [];
  const room = new GameRoom({ name: 'QM_1', expected: 1, send: (c, b) => sent.push([c, Buffer.from(b)]) });
  room.attachServer({ ctl: (m) => ctl.push(m), send: (b) => toServer.push(Buffer.from(b)) });
  return { room, sent, ctl, toServer };
}

test('mtPackets / mtMessages round-trip a large message', () => {
  const data = Uint8Array.from({ length: 5000 }, (_, i) => i & 255);
  const pkts = mtPackets(CH_GAME, data);
  assert.equal(pkts.length, Math.ceil(5001 / MT_PIECE));
  const state = {}, out = [];
  for (const p of pkts) out.push(...mtMessages(state, p));
  assert.equal(out.length, 1);
  assert.equal(out[0].ch, CH_GAME);
  assert.deepEqual(Buffer.from(out[0].data), Buffer.from(data));
  assert.equal(mtPackets(CH_GAME, Uint8Array.of(1, 2, 3)).length, 1);   // small message: one packet, piece 0
});

test('the server hears about a player once it is seated; messages are routed both ways', () => {
  const { room, sent, ctl, toServer } = setup();
  const a = { name: 'A' };
  room.join(a);
  assert.deepEqual(ctl, []);
  room.packet(a, frame(CH_TABLE, new Writer().u8(0).u8(13).u32(7).bytes()));   // REQUEST SEAT_TAKE
  assert.deepEqual(ctl, [{ t: 'player_joined', pid: 1, name: 'A' }]);
  assert.equal(SEAT_TAKE, 13);
  sent.length = 0;
  room.packet(a, mtPackets(CH_GAME, Uint8Array.of(9, 8, 7))[0]);
  assert.deepEqual([...toServer[0]], [1, 0, 9, 8, 7]);                          // [u16 player id][message]
  const big = new Uint8Array(2000).fill(5);
  room.fromServer(Uint8Array.from([1, 0, ...big]));
  const state = {}, got = [];
  for (const [, b] of sent) got.push(...mtMessages(state, b));
  assert.equal(got.length, 1);
  assert.equal(got[0].data.length, 2000);
  room.leave(a);
  assert.deepEqual(ctl[1], { t: 'player_left', pid: 1 });
  room.close();
});

test('a hosted game takes the next Quick Match players; without one nobody is matched', async () => {
  const { Relay } = await import('../src/relay.js');
  const relay = new Relay({ matchSize: 0 });
  const casino = relay.rooms.get('casinosystem').bot;
  const started = [];
  casino.startMatch = (group, room) => started.push([group.map((q) => q.client.name), room]);
  const q = (name) => casino.queue.push({ client: { name }, ch: 0x12 });
  q('Bob');
  casino.matchmake();
  assert.deepEqual(started, []);                                  // no host yet: Bob waits
  const host = relay.connect(() => {}, () => {});
  host.onText(JSON.stringify({ t: 'hello', v: 1, name: 'RGHost' }));
  host.onText(JSON.stringify({ t: 'serve', room: 'HOST_Ann', players: 2 }));
  assert.deepEqual(started, []);                                  // needs two players
  q('Cid');
  casino.matchmake();
  assert.deepEqual(started, [[['Bob', 'Cid'], 'HOST_Ann']]);
  assert.deepEqual(casino.hosted, []);
  host.onText(JSON.stringify({ t: 'serve', room: 'HOST_Ann2', players: 1 }));
  host.onClose();                                                 // host quits: its offer is withdrawn
  assert.deepEqual(casino.hosted, []);
});

test('no SEAT_LEAVE events to the others once a game server serves the room', () => {
  const { room, sent } = setup();
  const a = { name: 'A' }, b = { name: 'B' };
  room.join(a);
  room.join(b);
  room.packet(a, frame(CH_TABLE, new Writer().u8(0).u8(13).u32(7).bytes()));
  room.packet(b, frame(CH_TABLE, new Writer().u8(0).u8(13).u32(8).bytes()));
  sent.length = 0;
  room.packet(a, frame(CH_TABLE, new Writer().u8(0).u8(14).u32(7).bytes()));   // REQUEST SEAT_LEAVE
  assert.deepEqual(sent.filter(([c]) => c === b), []);                          // B is not told
  assert.equal(sent.filter(([c]) => c === a).length, 1);                        // A gets its RESPONSE
  room.leave(a);
  assert.deepEqual(sent.filter(([c]) => c === b), []);
  room.close();
});

test('a player whose connection dropped in a hosted match goes back into it with Quick Match', async () => {
  const { Relay } = await import('../src/relay.js');
  const relay = new Relay({ matchSize: 0 });
  const casino = relay.rooms.get('casinosystem').bot;
  const player = (name) => {
    const p = relay.connect(() => {}, () => {});
    p.onText(JSON.stringify({ t: 'hello', v: 1, name }));
    p.onText(JSON.stringify({ t: 'join', room: 'CasinoSystem' }));
    return p;
  };
  const quickMatch = (p) => p.onBinary(frame(0x12, new Writer().u8(0).u8(9).str('PLUMBER').str('FALSE').bytes()));
  const host = relay.connect(() => {}, () => {});
  host.onText(JSON.stringify({ t: 'hello', v: 1, name: 'RGHost' }));
  host.onText(JSON.stringify({ t: 'serve', room: 'HOST_Ann', players: 1 }));
  const bob = player('Bob');
  quickMatch(bob);
  casino.locked(bob.client);                                     // Bob answers LOCK_PLAYER_IN_GAME
  bob.onText(JSON.stringify({ t: 'move', room: 'HOST_Ann' }));
  assert.equal(bob.client.room.name, 'HOST_Ann');
  const started = [];
  casino.startMatch = (group, room) => started.push([group.map((q) => q.client.name), room]);
  bob.onClose();                                                 // Bob's game crashes mid-match
  const cid = player('Cid');
  quickMatch(cid);                                               // someone else: waits for a new game
  assert.deepEqual(started, []);
  const bob2 = player('Bob');
  quickMatch(bob2);                                              // Bob is back: straight into his match
  assert.deepEqual(started, [[['Bob'], 'HOST_Ann']]);
  quickMatch(bob2);                                              // only once per drop
  assert.equal(started.length, 1);
  // not sent back: a player who left the match the normal way (surrendered), one who comes back too
  // late, anyone after the match ended
  casino.running.set('host_ann', 'HOST_Ann');
  casino.droppedOut('host_ann', 'Dan');
  casino.leftGame('host_ann', 'Dan');
  quickMatch(player('Dan'));
  const { REJOIN_MS } = await import('../src/casino.js');
  casino.droppedOut('host_ann', 'Fay');
  casino.dropped.get('host_ann').set('fay', Date.now() - REJOIN_MS - 1);   // too late: the host gave the seat up
  quickMatch(player('Fay'));
  casino.droppedOut('host_ann', 'Eve');
  host.onText(JSON.stringify({ t: 'unserve' }));                 // match over
  quickMatch(player('Eve'));
  assert.equal(started.length, 1);
});

test('a new connection takes over the name of a silent one (the same player after a crash)', async () => {
  const { Relay } = await import('../src/relay.js');
  let now = 1_000_000;
  const relay = new Relay({ matchSize: 0, now: () => now });
  const replies = [];
  const hello = (name) => {
    const p = relay.connect((m) => replies.push(m), () => {});
    p.onText(JSON.stringify({ t: 'hello', v: 1, name }));
    return p;
  };
  const a = hello('Bob');
  now += 5_000;
  const b = hello('Bob');                                        // the first one is alive: renamed
  assert.equal(b.client.name, 'Bob2');
  b.onClose();
  now += 30_000;                                                  // a has been silent for 35 s
  const c = hello('Bob');
  assert.equal(c.client.name, 'Bob');
  assert.equal(relay.clients.has(a.client.id), false);
});

test('the game server is told when a player was lost (dropped connection), so it can hold the seat', () => {
  const { room, ctl } = setup();
  const a = { name: 'A' }, b = { name: 'B' };
  room.join(a);
  room.join(b);
  room.packet(a, frame(CH_TABLE, new Writer().u8(0).u8(13).u32(7).bytes()));
  room.packet(b, frame(CH_TABLE, new Writer().u8(0).u8(13).u32(8).bytes()));
  room.leave(a);                                        // a normal leave
  b.lost = true;
  room.leave(b);                                        // b's connection dropped
  assert.deepEqual(ctl.slice(2), [{ t: 'player_left', pid: 1 }, { t: 'player_left', pid: 2, lost: 1 }]);
  room.close();
});

test('a hosted match room stays while its host serves it, so its last player can come back', async () => {
  const { Relay } = await import('../src/relay.js');
  const relay = new Relay({ matchSize: 0 });
  const host = relay.connect(() => {}, () => {});
  host.onText(JSON.stringify({ t: 'hello', v: 1, name: 'RGHost' }));
  host.onText(JSON.stringify({ t: 'serve', room: 'HOST_Ann', players: 1 }));
  relay.expectRoom('HOST_Ann', 1);
  const bob = relay.connect(() => {}, () => {});
  bob.onText(JSON.stringify({ t: 'hello', v: 1, name: 'Bob' }));
  bob.onText(JSON.stringify({ t: 'join', room: 'HOST_Ann' }));
  const room = relay.rooms.get('host_ann');
  assert.ok(room?.bot);
  bob.onClose();                                                 // a 2-player match: the only client drops
  assert.equal(relay.rooms.get('host_ann'), room);               // the room and its game server stay
  host.onText(JSON.stringify({ t: 'unserve' }));                 // match over
  assert.equal(relay.rooms.has('host_ann'), false);
});

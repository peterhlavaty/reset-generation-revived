/* Connections to a relay (server/, docs/MULTIPLAYER.md) for the online client (net.c) and the
 * host (server.c): WebSocket client links, and which relay to use (server=auto|online|lan|URL,
 * LAN discovery on UDP 8787). */
#pragma once
#include <stddef.h>
#include <stdint.h>

#define RELAY_PORT 8787

typedef struct rt_link rt_link;

/* Connects and upgrades to a WebSocket. NULL on failure (logged as "<who>: ..."). The link has
 * one reference, the caller's. */
rt_link *link_open(const char *url, const char *who);
void link_ref(rt_link *l);
void link_release(rt_link *l);   /* frees the link with its last reference */
/* One frame (masked, as from a client); any thread. 0 once the link is closed or broken. */
int link_send(rt_link *l, int opcode, const void *data, size_t len);
/* The next text (1) or binary (2) frame into buf (a NUL is added after it: cap must leave room);
 * pings are answered and pongs skipped. 0 when the connection ended (or a frame was too big). */
int link_read(rt_link *l, uint8_t *buf, size_t cap, size_t *len);
/* Sends a close frame and shuts the connection down: a blocked link_read returns 0. */
void link_close(rt_link *l);

/* The relay to join through for a server setting: "online" (or empty) the public server, "auto" a
 * game hosted on the LAN if one answers within 2.5 s else the public server, "lan" only the LAN
 * (20 s), anything else is the address itself. Blocks; NULL if nothing was found. */
const char *relay_for_joining(const char *setting, char *buf, size_t n);
/* The relay a hosted game is offered on: "online" the public server, "auto" the public server if
 * it answers else this device's own LAN relay, "lan" the own relay. NULL if none can be used. */
const char *relay_for_hosting(const char *setting, const char *nick, char *buf, size_t n);
/* Whether a relay answers GET /status at url (1 s). */
int relay_reachable(const char *url);

/* Starts a LAN relay on this device (the desktop game runs one in-process) and returns its
 * address, or NULL; set by the platform host, NULL where there is none (Android). */
typedef const char *(*relay_starter)(const char *nick);
void rg_set_relay_starter(relay_starter fn);

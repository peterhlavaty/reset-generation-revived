/* Hosting a game on this player's engine (server.c): the engine's own LAN host mode with the
 * remote players arriving through the relay. The public API is in engine.h (rg_host_*). */
#pragma once
#include <stdint.h>

/* Test mode (rgtest --server URL ROOM), before rg_init: host ROOM on the relay at URL by itself
 * once the main menu is up. */
void rg_server_enable(const char *url, const char *room);
int rg_server_enabled(void);

/* engine glue (engine.c) */
void rg_server_init(void);          /* after the engine image is loaded */
void rg_server_pump(void);          /* game thread, before each tick */
int rg_server_key(int down, int keycode);   /* 1 = the key is ours (F5, the picker) */
const uint32_t *rg_server_compose(const uint32_t *fb, int w, int h);   /* the frame with the picker / status */
void rg_engine_key(int down, int keycode);  /* a key straight to the engine */

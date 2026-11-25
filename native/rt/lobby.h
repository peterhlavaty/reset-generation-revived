/* Choosing a hosted game in Quick Match (lobby.c): a list drawn over the engine's "Players in
 * Match" screen, filled by the relay's "games" messages (server/src/casino.rs). */
#pragma once
#include <stdint.h>

void rg_lobby_message(const char *json);   /* any thread: a "games" message from the relay */
void rg_lobby_reset(void);                 /* any thread: the relay connection is gone */
int rg_lobby_key(int down, int keycode);   /* game thread: 1 = the key is the list's */
const uint32_t *rg_lobby_compose(const uint32_t *fb, int w, int h);   /* the frame with the list */

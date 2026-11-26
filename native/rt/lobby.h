/* Choosing a hosted game in Quick Match (lobby.c): a list drawn over the engine's "Players in
 * Match" screen, filled by the relay's "games" messages (server/src/casino.rs). `link` is net.c's
 * relay link generation (1, 2, ...): only the current link's messages count, so a frame that the
 * reader of a closed link still delivers cannot bring a list back after a logout. */
#pragma once
#include <stdint.h>

void rg_lobby_message(int link, const char *json);   /* any thread: a "games" message that came on `link` */
void rg_lobby_reset(int link);             /* any thread: `link` is the relay link now (0: none); clears the list */
void rg_lobby_lost(int link);              /* any thread: `link` ended: clears its list (a newer link's stays) */
int rg_lobby_key(int down, int keycode);   /* game thread: 1 = the key is the list's */
const uint32_t *rg_lobby_compose(const uint32_t *fb, int w, int h);   /* the frame with the list */

#ifndef RIWORK_NSH_TERMINAL_H
#define RIWORK_NSH_TERMINAL_H
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

// Every function below runs on AppKit's main thread. new returns an owned NSView.
// The context is owned until the view's final release, including asynchronous
// disposal. Event JSON is borrowed only for the duration of the callback.
typedef void (*rw_terminal_event_cb)(void *, const char *, const char *);
// Must schedule exact-tty detach OFF the main thread and call detach_result ON
// the main thread. One request is outstanding at a time; false retries in 2s.
typedef void (*rw_terminal_detach_cb)(void *, void *, const char *);
typedef void (*rw_terminal_context_release_cb)(void *);
void *rw_terminal_new(void *context, rw_terminal_event_cb event,
    rw_terminal_detach_cb detach, rw_terminal_context_release_cb release_context);
// The view a component gives NSH: an owned NSView that hosts one terminal view
// at a time, filling it. A Ghostty surface draws into the view it was made
// with, so a terminal kept for a later component moves between hosts.
void *rw_terminal_host_new(void);
// Hosts the terminal view in place of the one hosted before, which the host
// releases.
void rw_terminal_host_show(void *host, void *view);
// Takes an attached terminal whose child still runs out of its host and stops
// drawing it, keeping its surface, last size and tmux client for a later
// component. The caller holds its own reference, since the host releases the
// view. False, changing nothing, when there is no live surface to keep.
bool rw_terminal_pool(void *);
// Whether the view has a surface, not closing, whose child still runs.
bool rw_terminal_live(void *);
// Exchanges two views' contexts: each then emits to, detaches with and is
// finally released with the other's.
void rw_terminal_swap_context(void *, void *);
bool rw_terminal_attach(void *, const char *command, const char *cwd, const char *config_path);
void rw_terminal_set_state(void *, bool visible, bool frozen, bool focus_on_attach);
bool rw_terminal_update_config(void *, const char *config_path);
// Read the same parsed user configuration without creating an app or surface.
// Output is background, foreground, then 16 ANSI colors, packed as 0xRRGGBB.
bool rw_terminal_read_config_colors(const char *config_path, uint32_t colors[18]);
void rw_terminal_command(void *, const char *name);
void rw_terminal_find(void *, const char *query);
void rw_terminal_set_link(void *, const char *json);
// Ghostty's paste of plain text (bracketed when asked for), for remote bridges.
void rw_terminal_paste_text(void *, const char *text);
void rw_terminal_detach_result(void *, bool confirmed);
// shutdown leaves an idle reusable view; abandon permanently prevents attach.
void rw_terminal_shutdown(void *);
void rw_terminal_abandon(void *);
size_t rw_terminal_pending_count(void);
double rw_terminal_monotonic_ms(void);
#ifdef RIWORK_TERMINAL_FIXTURE
bool rw_terminal_fixture_probe(void *, const char *kind);
bool rw_terminal_fixture_pasteboard(void *, const char *json);
#endif

#endif

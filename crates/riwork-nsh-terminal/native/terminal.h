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
bool rw_terminal_attach(void *, const char *command, const char *cwd, const char *config_path);
void rw_terminal_set_state(void *, bool visible, bool frozen, bool focus_on_attach);
bool rw_terminal_update_config(void *, const char *config_path);
void rw_terminal_command(void *, const char *name);
void rw_terminal_set_link(void *, const char *json);
void rw_terminal_detach_result(void *, bool confirmed);
// shutdown leaves an idle reusable view; abandon permanently prevents attach.
void rw_terminal_shutdown(void *);
void rw_terminal_abandon(void *);
size_t rw_terminal_pending_count(void);
double rw_terminal_monotonic_ms(void);
#ifdef RIWORK_TERMINAL_FIXTURE
bool rw_terminal_fixture_probe(void *, const char *kind);
#endif

#endif

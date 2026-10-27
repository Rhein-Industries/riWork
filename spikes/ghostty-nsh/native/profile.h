// Spike-only timing recorder. No allocation/serialization/I/O in the hot path.
// One clock for native gaps, worker waits, and CF run-loop observations.
#import <pthread.h>
#import <stdlib.h>

typedef struct {
    double ms;
    const char *name;
    unsigned long surface_id;
    uint64_t thread_id;
    char phase;
    bool main_thread;
} SpikeProfileEvent;
static SpikeProfileEvent *profileEvents;
static _Atomic unsigned long profileCount;
static const unsigned long profileCapacity = 2000000;
static char profilePath[4096];
static CFRunLoopObserverRef profileObserver;
static CFRunLoopTimerRef profileTimer;

static void profile(const char *name, char phase, unsigned long sid) {
    if (!profileEvents) return;
    unsigned long index = atomic_fetch_add(&profileCount, 1);
    if (index >= profileCapacity) return;
    uint64_t tid = 0;
    pthread_threadid_np(NULL, &tid);
    profileEvents[index] = (SpikeProfileEvent){monotonicMS(), name, sid, tid, phase, pthread_main_np() != 0};
}
static void profileFlush(void) {
    if (!profileEvents) return;
    profile("process-exit", 'i', 0);
    FILE *file = fopen(profilePath, "w");
    if (!file) { perror("profile output"); _Exit(71); }
    unsigned long count = atomic_load(&profileCount);
    fprintf(file, "{\"kind\":\"metadata\",\"clock\":\"CLOCK_MONOTONIC\",\"fixture_pid\":%d,\"capacity\":%lu,\"recorded\":%lu,\"dropped\":%lu}\n",
        getpid(), profileCapacity, MIN(count, profileCapacity), count > profileCapacity ? count-profileCapacity : 0);
    for (unsigned long i = 0; i < MIN(count, profileCapacity); i++) {
        SpikeProfileEvent e = profileEvents[i];
        fprintf(file, "{\"seq\":%lu,\"monotonic_ms\":%.6f,\"name\":\"%s\",\"phase\":\"%c\",\"surface_id\":%lu,\"thread_id\":%llu,\"main_thread\":%s}\n",
            i, e.ms, e.name, e.phase, e.surface_id, (unsigned long long)e.thread_id, e.main_thread ? "true" : "false");
    }
    if (fclose(file) != 0) { perror("profile flush"); _Exit(71); }
}
static void profileRunLoop(CFRunLoopObserverRef observer, CFRunLoopActivity activity, void *info) {
    const char *name = "runloop-unknown";
    switch (activity) {
        case kCFRunLoopEntry: name = "runloop-entry"; break;
        case kCFRunLoopBeforeTimers: name = "runloop-before-timers"; break;
        case kCFRunLoopBeforeSources: name = "runloop-before-sources"; break;
        case kCFRunLoopBeforeWaiting: name = "runloop-before-waiting"; break;
        case kCFRunLoopAfterWaiting: name = "runloop-after-waiting"; break;
        case kCFRunLoopExit: name = "runloop-exit"; break;
    }
    profile(name, 'i', 0);
}
static void profileHeartbeat(CFRunLoopTimerRef timer, void *info) { profile("heartbeat", 'i', 0); }
static void profileStart(void) {
    if (profileEvents) return;
    profileEvents = calloc(profileCapacity, sizeof(SpikeProfileEvent));
    if (!profileEvents) abort();
    snprintf(profilePath, sizeof(profilePath), "%s/main-profile.jsonl", getenv("RIWORK_HOME"));
    atexit(profileFlush);
    profile("profile-start", 'i', 0);
    profileObserver = CFRunLoopObserverCreate(NULL, kCFRunLoopAllActivities, true, 0, profileRunLoop, NULL);
    CFRunLoopAddObserver(CFRunLoopGetMain(), profileObserver, kCFRunLoopCommonModes);
    profileTimer = CFRunLoopTimerCreate(NULL, CFAbsoluteTimeGetCurrent(), 1.0/60.0, 0, 0, profileHeartbeat, NULL);
    CFRunLoopAddTimer(CFRunLoopGetMain(), profileTimer, kCFRunLoopCommonModes);
}

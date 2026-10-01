import Foundation
import os

/// Signposts for Instruments (`xcrun xctrace record --template 'Time Profiler' …`, then the os_signpost track of subsystem
/// `com.riwork.remote`, category `Performance`), and counters the performance tests read. A signpost costs a few nanoseconds when nothing is
/// recording, so these stay in release builds; the counters are compiled out there.
@MainActor enum Perf {
    nonisolated static let signposter = OSSignposter(subsystem: "com.riwork.remote", category: "Performance")

    /// Runs `body` as a signpost interval named `name`.
    nonisolated static func interval<T>(_ name: StaticString, _ body: () throws -> T) rethrows -> T {
        let state = signposter.beginInterval(name)
        defer { signposter.endInterval(name, state) }
        return try body()
    }

    /// How often each counted thing happened (view bodies evaluated, rows painted, …) since `reset()`. Counted in debug builds, and in
    /// release builds made with `SWIFT_ACTIVE_COMPILATION_CONDITIONS=PERF_COUNTERS` (how the benchmarks measure what ships).
    private(set) static var counts: [String: Int] = [:]
    static func reset() { counts = [:] }
    /// Counts one occurrence of `name`.
    @inline(__always) static func count(_ name: String) {
        #if DEBUG || PERF_COUNTERS
        counts[name, default: 0] += 1
        #endif
    }
}

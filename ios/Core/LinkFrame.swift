import Foundation
import Compression

/// The plaintext of a sealed reply, in its two forms (the link extension, `docs/remote-protocol.md`):
///
/// - JSON text, starting with `{`: what every reply was before, and what every reply still is unless the phone asked for compression
///   and the reply is long enough to profit from it;
/// - `0x01 || inflated length (UInt32, big-endian) || raw deflate` of that JSON text (RFC 1951, no zlib header or checksum, which is
///   what `COMPRESSION_ZLIB` reads and writes).
///
/// The marker is inside the authenticated ciphertext, so nobody on the path can flip it, and the length is checked against what comes
/// out: a stream that inflates to more or less than it says, or past the limit, is refused. This is the only place that inflates.
public enum LinkFrame {
    public static let deflateMarker: UInt8 = 0x01
    /// The most a reply may inflate to (the desktop announces the same figure in `ready`).
    public static let maximumInflatedBytes = 2 * 1024 * 1024
    /// Marker and length.
    public static let headerBytes = 5

    /// The JSON text inside a sealed plaintext, and whether it came compressed.
    public static func decode(_ plaintext: Data) throws -> (json: Data, compressed: Bool) {
        guard let first = plaintext.first else { throw RemoteError.protocolViolation("Empty frame.") }
        guard first == deflateMarker else {
            // JSON starts with `{` (or, in a JSON text, white space); any other marker is a format this phone does not know.
            guard first == 0x7B || first == 0x20 || first == 0x09 || first == 0x0A || first == 0x0D else { throw RemoteError.protocolViolation("Unknown frame format.") }
            return (plaintext, false)
        }
        guard plaintext.count > headerBytes else { throw RemoteError.protocolViolation("Short compressed frame.") }
        let start = plaintext.startIndex
        let declared = plaintext[start + 1 ..< start + 5].reduce(0) { ($0 << 8) | Int($1) }
        return (try inflate(plaintext.dropFirst(headerBytes), expecting: declared), true)
    }

    /// Raw deflate in, exactly `count` bytes out or an error.
    public static func inflate(_ stream: Data, expecting count: Int) throws -> Data {
        guard count >= 2, count <= maximumInflatedBytes else { throw RemoteError.protocolViolation("Compressed frame declares an unusable size.") }
        // One byte of room more than declared, so a stream that inflates further is told apart from one that fits exactly:
        // the framework cuts a longer one short without a word.
        var out = [UInt8](repeating: 0, count: count + 1)
        let written = stream.withUnsafeBytes { (source: UnsafeRawBufferPointer) -> Int in
            out.withUnsafeMutableBytes { (target: UnsafeMutableRawBufferPointer) -> Int in
                guard let from = source.bindMemory(to: UInt8.self).baseAddress, let to = target.bindMemory(to: UInt8.self).baseAddress else { return 0 }
                return compression_decode_buffer(to, count + 1, from, source.count, nil, COMPRESSION_ZLIB)
            }
        }
        guard written == count else { throw RemoteError.protocolViolation("Compressed frame does not inflate to its declared size.") }
        return Data(out[0 ..< count])
    }

    /// Raw deflate of `data`, or nil when the framework cannot (or the result is not smaller). Phones do not compress what they send;
    /// this is for the tests' desktop.
    public static func deflate(_ data: Data) -> Data? {
        guard !data.isEmpty else { return nil }
        // Room for incompressible data plus the framework's own overhead.
        var out = [UInt8](repeating: 0, count: data.count + data.count / 8 + 64)
        let size = out.count
        let written = data.withUnsafeBytes { (source: UnsafeRawBufferPointer) -> Int in
            out.withUnsafeMutableBytes { (target: UnsafeMutableRawBufferPointer) -> Int in
                guard let from = source.bindMemory(to: UInt8.self).baseAddress, let to = target.bindMemory(to: UInt8.self).baseAddress else { return 0 }
                return compression_encode_buffer(to, size, from, source.count, nil, COMPRESSION_ZLIB)
            }
        }
        return written > 0 ? Data(out[0 ..< written]) : nil
    }

    /// The plaintext a desktop would seal for `json` when it compresses.
    public static func compressedFrame(for json: Data) -> Data? {
        guard let deflated = deflate(json), deflated.count + headerBytes < json.count, json.count <= maximumInflatedBytes else { return nil }
        var frame = Data([deflateMarker])
        withUnsafeBytes(of: UInt32(json.count).bigEndian) { frame.append(contentsOf: $0) }
        frame.append(deflated)
        return frame
    }
}

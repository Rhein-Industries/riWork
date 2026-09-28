import Foundation

public enum SocketMessage: Sendable, Equatable { case text(String), binary(Data) }

/// The slice of `URLSessionWebSocketTask` that `RelayClient` needs, so its handshake,
/// cancellation, timeout and keepalive paths can run against a scripted peer in tests.
public protocol WebSocketConnection: Sendable {
    func resume()
    func send(_ text: String) async throws
    func receive() async throws -> SocketMessage
    /// Completes when the matching pong arrives.
    func ping() async throws
    func cancel(with code: URLSessionWebSocketTask.CloseCode, reason: Data?)
    /// `.invalid` until the peer's close frame has been seen.
    var closeCode: URLSessionWebSocketTask.CloseCode { get }
    var closeReason: Data? { get }
}

public protocol WebSocketConnecting: Sendable {
    func connection(to url: URL) -> any WebSocketConnection
}

/// One ephemeral session per client. `timeoutIntervalForRequest` is an idle timer for WebSockets,
/// so it must comfortably exceed the ping interval; the resource timeout caps the socket's lifetime.
public struct URLSessionConnector: WebSocketConnecting {
    private let session: URLSession
    public init() {
        let config = URLSessionConfiguration.ephemeral
        config.urlCache = nil
        config.httpCookieStorage = nil
        config.timeoutIntervalForRequest = 30
        config.timeoutIntervalForResource = 24 * 60 * 60
        session = URLSession(configuration: config)
    }
    public func connection(to url: URL) -> any WebSocketConnection {
        let task = session.webSocketTask(with: url)
        task.maximumMessageSize = 262144
        return URLSessionSocket(task: task)
    }
}

struct URLSessionSocket: WebSocketConnection {
    let task: URLSessionWebSocketTask
    func resume() { task.resume() }
    func send(_ text: String) async throws { try await task.send(.string(text)) }
    func receive() async throws -> SocketMessage {
        switch try await task.receive() {
        case .string(let text): .text(text)
        case .data(let data): .binary(data)
        @unknown default: throw RemoteError.protocolViolation("Expected bounded text frame.")
        }
    }
    func ping() async throws {
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, any Error>) in
            task.sendPing { error in if let error { continuation.resume(throwing: error) } else { continuation.resume() } }
        }
    }
    func cancel(with code: URLSessionWebSocketTask.CloseCode, reason: Data?) { task.cancel(with: code, reason: reason) }
    var closeCode: URLSessionWebSocketTask.CloseCode { task.closeCode }
    var closeReason: Data? { task.closeReason }
}

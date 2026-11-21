import CryptoKit
import Foundation
import SQLite3

struct Failure: LocalizedError {
    let message: String
    init(_ message: String) { self.message = message }
    var errorDescription: String? { message }
}

func identifier(_ value: String) -> Bool {
    !value.isEmpty && value.utf8.count <= 128 && value.utf8.allSatisfy {
        (48...57).contains($0) || (65...90).contains($0) || (97...122).contains($0) || $0 == 45 || $0 == 95
    }
}

// Only the operator supplies the dashboard origin and agent at process startup.
// The notification payload cannot select a host, a URL scheme, or another agent.
struct Destination {
    let origin: URL
    let agent: String

    init(_ address: String, _ agent: String) throws {
        guard let parts = URLComponents(string: address),
              parts.scheme == "http" || parts.scheme == "https",
              ["127.0.0.1", "localhost", "[::1]"].contains(parts.host?.lowercased() ?? ""),
              parts.user == nil, parts.password == nil, parts.query == nil, parts.fragment == nil,
              parts.path.isEmpty || parts.path == "/", identifier(agent) else {
            throw Failure("Expected a loopback dashboard origin and an agent alias.")
        }
        var canonical = parts
        canonical.path = ""
        canonical.host = parts.host?.lowercased()
        if (parts.scheme == "http" && parts.port == 80) || (parts.scheme == "https" && parts.port == 443) {
            canonical.port = nil
        }
        guard let normalized = canonical.url else { throw Failure("Invalid dashboard origin.") }
        self.origin = normalized
        self.agent = agent
    }

    func link(_ session: String?) throws -> URL {
        var parts = URLComponents(url: origin, resolvingAgainstBaseURL: false)
        parts?.path = "/agent/" + agent
        var query = [URLQueryItem(name: "notify", value: "1")]
        if let session {
            guard identifier(session) else { throw Failure("Invalid conversation identifier.") }
            query.append(URLQueryItem(name: "session", value: session))
        }
        parts?.queryItems = query
        guard let url = parts?.url else { throw Failure("Invalid dashboard address.") }
        return url
    }
}

/// Keep bounded receipts for explicit event IDs across process restarts and banner dismissal.
/// One transaction also serializes two agents retrying the same delivery.
func deliverOnce(_ key: String, directory: URL, send: () async throws -> Void) async throws -> String {
    try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true,
                                           attributes: [.posixPermissions: 0o700])
    var db: OpaquePointer?
    guard sqlite3_open(directory.appendingPathComponent("receipts.sqlite3").path, &db) == SQLITE_OK else {
        sqlite3_close(db)
        throw Failure("Could not open the notification receipts.")
    }
    defer { sqlite3_close(db) }
    sqlite3_busy_timeout(db, 5000)
    func execute(_ sql: String) throws {
        guard sqlite3_exec(db, sql, nil, nil, nil) == SQLITE_OK else {
            throw Failure("Could not update the notification receipts.")
        }
    }
    try execute("CREATE TABLE IF NOT EXISTS receipts (id TEXT PRIMARY KEY, sent INTEGER NOT NULL)")
    try execute("BEGIN IMMEDIATE")
    defer { _ = sqlite3_exec(db, "ROLLBACK", nil, nil, nil) }
    try execute("DELETE FROM receipts WHERE sent < unixepoch() - 30 * 86400")
    // Hex output is the complete SQL input. The database contains no alert text or URLs.
    let hash = SHA256.hash(data: Data(key.utf8)).map { String(format: "%02x", $0) }.joined()
    try execute("INSERT OR IGNORE INTO receipts VALUES ('\(hash)', unixepoch())")
    if sqlite3_changes(db) == 0 {
        try execute("COMMIT")
        return "Already delivered."
    }
    try await send()
    try execute("COMMIT")
    return "Accepted by Notification Center."
}

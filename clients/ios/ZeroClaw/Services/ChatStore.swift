import Foundation

/// File-based chat persistence using JSON.
/// Stores conversations per session in the app's documents directory.
final class ChatStore {
    static let shared = ChatStore()

    private let directory: URL

    private init() {
        let docs = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
        directory = docs.appendingPathComponent("chat_sessions", isDirectory: true)
        try? FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
    }

    // MARK: - Save / Load

    func save(messages: [ChatMessage], sessionId: String) {
        let entries = messages.map { StoredMessage(id: $0.id, content: $0.content, role: $0.role, timestampMs: $0.timestampMs) }
        guard let data = try? JSONEncoder().encode(entries) else { return }
        let file = directory.appendingPathComponent("\(sessionId).json")
        try? data.write(to: file, options: .atomic)
    }

    func load(sessionId: String) -> [ChatMessage] {
        let file = directory.appendingPathComponent("\(sessionId).json")
        guard let data = try? Data(contentsOf: file),
              let entries = try? JSONDecoder().decode([StoredMessage].self, from: data) else {
            return []
        }
        return entries.map {
            ChatMessage(id: $0.id, content: $0.content, role: $0.role, timestampMs: $0.timestampMs)
        }
    }

    func delete(sessionId: String) {
        let file = directory.appendingPathComponent("\(sessionId).json")
        try? FileManager.default.removeItem(at: file)
    }

    // MARK: - Sessions

    func listSessions() -> [SessionInfo] {
        guard let files = try? FileManager.default.contentsOfDirectory(at: directory, includingPropertiesForKeys: [.contentModificationDateKey]) else {
            return []
        }

        return files
            .filter { $0.pathExtension == "json" }
            .compactMap { url -> SessionInfo? in
                let sessionId = url.deletingPathExtension().lastPathComponent
                let modified = (try? url.resourceValues(forKeys: [.contentModificationDateKey]))?.contentModificationDate ?? Date()
                let messages = load(sessionId: sessionId)
                let preview = messages.last(where: { $0.role == "assistant" })?.content
                    ?? messages.last?.content
                    ?? ""
                return SessionInfo(
                    id: sessionId,
                    lastModified: modified,
                    messageCount: messages.count,
                    preview: String(preview.prefix(100))
                )
            }
            .sorted { $0.lastModified > $1.lastModified }
    }
}

// MARK: - Types

struct StoredMessage: Codable {
    let id: String
    let content: String
    let role: String
    let timestampMs: Int64
}

struct SessionInfo: Identifiable {
    let id: String
    let lastModified: Date
    let messageCount: Int
    let preview: String
}

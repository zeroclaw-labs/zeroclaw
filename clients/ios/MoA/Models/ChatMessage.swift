import Foundation

/// A single chat message in the conversation.
struct ChatMessage: Identifiable {
    let id: UUID
    let role: Role
    let content: String
    let timestamp: Date

    enum Role {
        case user
        case assistant
        case error
    }

    init(role: Role, content: String) {
        self.id = UUID()
        self.role = role
        self.content = content
        self.timestamp = Date()
    }
}

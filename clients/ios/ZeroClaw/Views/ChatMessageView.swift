import SwiftUI

struct ChatMessageView: View {
    let message: ChatMessage

    var body: some View {
        if message.isToolMessage {
            ToolCallBubble(role: message.role, content: message.content)
        } else {
            messageBubble
        }
    }

    private var messageBubble: some View {
        HStack {
            if message.isUser { Spacer(minLength: 48) }

            VStack(alignment: message.isUser ? .trailing : .leading, spacing: 2) {
                messageContent
                    .padding(.horizontal, 14)
                    .padding(.vertical, 10)
                    .foregroundStyle(message.isUser ? .white : .primary)
                    .background {
                        if message.isUser {
                            RoundedRectangle(cornerRadius: 18)
                                .fill(.zeroClawOrange)
                        }
                    }
                    .glassEffect(
                        message.isUser ? .identity : .regular,
                        in: RoundedRectangle(cornerRadius: 18)
                    )
                    .contextMenu { contextMenuItems }

                Text(message.timestamp, style: .time)
                    .font(.caption2)
                    .foregroundStyle(.tertiary)
                    .padding(.horizontal, 4)
            }

            if !message.isUser { Spacer(minLength: 48) }
        }
    }

    @ViewBuilder
    private var messageContent: some View {
        if message.isUser {
            Text(message.content)
        } else {
            MarkdownView(content: message.content)
        }
    }

    @ViewBuilder
    private var contextMenuItems: some View {
        Button {
            UIPasteboard.general.string = message.content
            UIImpactFeedbackGenerator(style: .light).impactOccurred()
        } label: {
            Label("Copy", systemImage: "doc.on.doc")
        }

        if !message.isUser {
            ShareLink(item: message.content) {
                Label("Share", systemImage: "square.and.arrow.up")
            }
        }
    }
}

// MARK: - ChatMessage Helpers

extension ChatMessage {
    var isToolMessage: Bool {
        role == "tool_call" || role == "tool_result"
    }
}

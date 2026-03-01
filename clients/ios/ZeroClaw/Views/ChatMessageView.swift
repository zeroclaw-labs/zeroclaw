import SwiftUI

struct ChatMessageView: View {
    let message: ChatMessage

    var body: some View {
        HStack {
            if message.isUser { Spacer(minLength: 48) }

            VStack(alignment: message.isUser ? .trailing : .leading, spacing: 2) {
                Text(message.content)
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

                Text(message.timestamp, style: .time)
                    .font(.caption2)
                    .foregroundStyle(.tertiary)
                    .padding(.horizontal, 4)
            }

            if !message.isUser { Spacer(minLength: 48) }
        }
    }
}

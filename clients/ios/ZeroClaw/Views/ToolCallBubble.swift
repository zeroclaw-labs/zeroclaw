import SwiftUI

/// Displays a tool call or tool result inline in the chat.
struct ToolCallBubble: View {
    let role: String
    let content: String

    var body: some View {
        HStack {
            VStack(alignment: .leading, spacing: 4) {
                HStack(spacing: 6) {
                    Image(systemName: icon)
                        .font(.caption)
                        .foregroundStyle(.zeroClawOrange)
                    Text(title)
                        .font(.caption.bold())
                        .foregroundStyle(.secondary)
                }

                if !detail.isEmpty {
                    Text(detail)
                        .font(.system(.caption2, design: .monospaced))
                        .foregroundStyle(.tertiary)
                        .lineLimit(4)
                }
            }
            .padding(10)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(Color(.tertiarySystemGroupedBackground))
            .clipShape(RoundedRectangle(cornerRadius: 12))

            Spacer(minLength: 48)
        }
    }

    private var icon: String {
        role == "tool_call" ? "wrench.and.screwdriver" : "checkmark.square"
    }

    private var parsed: (name: String, body: String) {
        guard let data = content.data(using: .utf8),
              let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else {
            return ("tool", content)
        }

        let name = json["name"] as? String ?? "tool"

        if role == "tool_call" {
            if let args = json["args"] {
                let argsData = try? JSONSerialization.data(withJSONObject: args, options: .prettyPrinted)
                let argsStr = argsData.flatMap { String(data: $0, encoding: .utf8) } ?? ""
                return (name, argsStr)
            }
            return (name, "")
        } else {
            let output = json["output"] as? String ?? ""
            return (name, output)
        }
    }

    private var title: String {
        let p = parsed
        return role == "tool_call" ? "Calling \(p.name)" : "\(p.name) result"
    }

    private var detail: String {
        parsed.body
    }
}

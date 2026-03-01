import SwiftUI

struct StatusIndicatorView: View {
    let status: AgentStatus

    var body: some View {
        HStack(spacing: 5) {
            Circle()
                .fill(status.color)
                .frame(width: 6, height: 6)

            Text(status.displayText)
                .font(.caption2)
                .foregroundStyle(.secondary)
                .fixedSize()
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 5)
        .fixedSize()
    }
}

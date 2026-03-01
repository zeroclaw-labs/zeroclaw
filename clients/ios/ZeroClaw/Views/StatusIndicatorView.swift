import SwiftUI

struct StatusIndicatorView: View {
    let status: AgentStatus

    var body: some View {
        HStack(spacing: 5) {
            Circle()
                .fill(indicatorColor)
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

    private var indicatorColor: Color {
        switch status {
        case .running: .green
        case .starting, .thinking: .orange
        case .stopped: .gray
        case .error(_): .red
        }
    }
}

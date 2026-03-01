import SwiftUI

struct EmptyStateView: View {
    var body: some View {
        VStack(spacing: 16) {
            Spacer()

            Text("🦀")
                .font(.system(size: 56))

            Text("ZeroClaw")
                .font(.title.bold())
                .foregroundStyle(.primary)

            Text("Your autonomous agent, ready to assist.")
                .font(.subheadline)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)

            Spacer()
            Spacer()
        }
        .padding(.horizontal, 32)
    }
}

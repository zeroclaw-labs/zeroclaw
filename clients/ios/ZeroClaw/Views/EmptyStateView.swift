import SwiftUI

struct EmptyStateView: View {
    @EnvironmentObject var settingsManager: SettingsManager

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

            if !settingsManager.isConfigured {
                Label("Configure your API key in Settings to get started.", systemImage: "key.fill")
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .padding(12)
                    .glassEffect(.regular, in: RoundedRectangle(cornerRadius: 12))
            }

            Spacer()
            Spacer()
        }
        .padding(.horizontal, 32)
    }
}

import SwiftUI

struct PairingOnboardingView: View {
    @EnvironmentObject var settingsManager: SettingsManager
    @EnvironmentObject var agentService: AgentService
    @Binding var showSettings: Bool

    @State private var pairingCode = ""
    @State private var isPairing = false
    @State private var error: String?

    var body: some View {
        VStack(spacing: 20) {
            Spacer()

            Image(systemName: "antenna.radiowaves.left.and.right")
                .font(.system(size: 48))
                .foregroundStyle(.zeroClawOrange)

            Text("Connect to Gateway")
                .font(.title2.bold())

            Text("Pair with your ZeroClaw gateway to start chatting.")
                .font(.subheadline)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)

            VStack(spacing: 12) {
                HStack {
                    TextField("Host", text: $settingsManager.gatewayHost)
                        .autocorrectionDisabled()
                        .textInputAutocapitalization(.never)
                        .keyboardType(.URL)

                    Text(":")
                        .foregroundStyle(.secondary)

                    TextField("Port", value: $settingsManager.gatewayPort, format: .number.grouping(.never))
                        .keyboardType(.numberPad)
                        .frame(width: 70)
                }
                .padding(12)
                .glassEffect(.regular, in: RoundedRectangle(cornerRadius: 12))

                TextField("Pairing Code", text: $pairingCode)
                    .autocorrectionDisabled()
                    .textInputAutocapitalization(.never)
                    .font(.system(.body, design: .monospaced))
                    .padding(12)
                    .glassEffect(.regular, in: RoundedRectangle(cornerRadius: 12))

                Button {
                    pair()
                } label: {
                    if isPairing {
                        ProgressView()
                            .frame(maxWidth: .infinity)
                    } else {
                        Text("Pair")
                            .fontWeight(.semibold)
                            .frame(maxWidth: .infinity)
                    }
                }
                .buttonStyle(.borderedProminent)
                .tint(.zeroClawOrange)
                .disabled(pairingCode.isEmpty || isPairing)
            }
            .padding(.horizontal, 8)

            if let error {
                Label(error, systemImage: "exclamationmark.triangle")
                    .font(.caption)
                    .foregroundStyle(.red)
            }

            Button("Advanced Settings") {
                showSettings = true
            }
            .font(.footnote)
            .foregroundStyle(.secondary)

            Spacer()
            Spacer()
        }
        .padding(.horizontal, 32)
    }

    private func pair() {
        isPairing = true
        error = nil

        Task {
            do {
                try await agentService.pair(code: pairingCode)
                pairingCode = ""
            } catch {
                self.error = error.localizedDescription
            }
            isPairing = false
        }
    }
}

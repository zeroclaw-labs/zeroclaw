import SwiftUI

struct StatusDetailView: View {
    @EnvironmentObject var agentService: AgentService
    @EnvironmentObject var settingsManager: SettingsManager

    var body: some View {
        NavigationStack {
            List {
                agentSection
                diagnosticsSection
            }
            .navigationTitle("Status")
            .navigationBarTitleDisplayMode(.inline)
            .background(Color(.systemGroupedBackground))
            .presentationDetents([.medium])
            .presentationDragIndicator(.visible)
        }
    }

    // MARK: - Sections

    private var agentSection: some View {
        Section {
            if case .error = agentService.status {
                NavigationLink {
                    errorDetailView
                } label: {
                    agentRow
                }
            } else {
                agentRow
            }
        } header: {
            Text("Agent")
        }
    }

    private var agentRow: some View {
        HStack {
            Circle()
                .fill(agentService.status.color)
                .frame(width: 10, height: 10)
            Text(agentService.status.displayText)
        }
    }

    private var diagnosticsSection: some View {
        Section {
            diagnosticRow("Pairing", state: agentService.pairingDiagnostic)
            diagnosticRow("Reachability", state: agentService.reachabilityDiagnostic)
            diagnosticRow("Connection", state: agentService.connectionDiagnostic)
        } header: {
            Text("Diagnostics")
        }
    }

    private func diagnosticRow(_ label: String, state: DiagnosticStep) -> some View {
        HStack {
            Text(label)
            Spacer()
            diagnosticIndicator(state)
        }
    }

    @ViewBuilder
    private func diagnosticIndicator(_ state: DiagnosticStep) -> some View {
        switch state {
        case .pending:
            Text("—")
                .foregroundStyle(.tertiary)
        case .checking:
            ProgressView()
                .controlSize(.small)
        case .passed:
            Image(systemName: "checkmark.circle.fill")
                .foregroundStyle(.green)
        case .failed(let reason):
            Text(reason)
                .foregroundStyle(.red)
                .font(.subheadline)
        case .skipped:
            Text("Skipped")
                .foregroundStyle(.secondary)
                .font(.subheadline)
        }
    }

    // MARK: - Error Detail

    private var errorDetailView: some View {
        List {
            Section {
                if case .failed = agentService.pairingDiagnostic {
                    Text("This device hasn't been paired with a ZeroClaw gateway. Open Settings and enter the pairing code shown in the gateway terminal.")
                } else if case .failed = agentService.reachabilityDiagnostic {
                    Text("The gateway at \(settingsManager.gatewayHost):\(settingsManager.gatewayPort) is not responding. Verify the gateway is running and the host/port are correct in Settings.")
                } else if case .failed = agentService.connectionDiagnostic {
                    Text("The gateway is reachable but the real-time connection could not be established. The pairing token may have expired — try unpairing and pairing again in Settings.")
                }
            } header: {
                Text("What to do")
            }

            if let connError = agentService.connectionError {
                Section {
                    Text(connError)
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                } header: {
                    Text("System log")
                }
            }
        }
        .navigationTitle("Troubleshoot")
        .navigationBarTitleDisplayMode(.inline)
    }
}

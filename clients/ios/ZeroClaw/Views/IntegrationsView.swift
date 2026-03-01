import SwiftUI

struct IntegrationsView: View {
    @EnvironmentObject var agentService: AgentService

    @State private var integrations: [Integration] = []
    @State private var isLoading = false
    @State private var error: String?

    var body: some View {
        List {
            if let error {
                Section {
                    Label(error, systemImage: "exclamationmark.triangle")
                        .foregroundStyle(.red)
                        .font(.caption)
                }
            }

            if integrations.isEmpty && !isLoading {
                ContentUnavailableView("No Integrations", systemImage: "puzzlepiece", description: Text("No integrations configured on the gateway."))
            }

            ForEach(integrations) { integration in
                HStack {
                    VStack(alignment: .leading, spacing: 2) {
                        Text(integration.name)
                            .font(.subheadline)
                        if let type = integration.type {
                            Text(type)
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                    }
                    Spacer()
                    Circle()
                        .fill(integration.isHealthy ? .green : .red)
                        .frame(width: 8, height: 8)
                    Text(integration.status ?? "unknown")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
        }
        .navigationTitle("Integrations")
        .navigationBarTitleDisplayMode(.inline)
        .refreshable { await fetchIntegrations() }
        .task { await fetchIntegrations() }
        .overlay { if isLoading && integrations.isEmpty { ProgressView() } }
    }

    private func fetchIntegrations() async {
        isLoading = true
        error = nil
        do {
            integrations = try await agentService.gateway.fetchIntegrations()
        } catch {
            self.error = error.localizedDescription
        }
        isLoading = false
    }
}

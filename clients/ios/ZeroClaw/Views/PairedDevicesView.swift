import SwiftUI

struct PairedDevicesView: View {
    @EnvironmentObject var agentService: AgentService

    @State private var devices: [PairedDevice] = []
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

            if devices.isEmpty && !isLoading {
                ContentUnavailableView("No Devices", systemImage: "iphone.slash", description: Text("No paired devices found."))
            }

            ForEach(devices) { device in
                HStack {
                    VStack(alignment: .leading, spacing: 2) {
                        Text(device.name ?? device.id)
                            .font(.subheadline)
                        if let lastSeen = device.lastSeen {
                            Text("Last seen: \(lastSeen)")
                                .font(.caption)
                                .foregroundStyle(.tertiary)
                        }
                    }
                    Spacer()
                    if device.paired == true {
                        Image(systemName: "checkmark.seal.fill")
                            .foregroundStyle(.green)
                    }
                }
                .swipeActions(edge: .trailing) {
                    Button(role: .destructive) {
                        revokeDevice(id: device.id)
                    } label: {
                        Label("Revoke", systemImage: "xmark.seal")
                    }
                }
            }
        }
        .navigationTitle("Paired Devices")
        .navigationBarTitleDisplayMode(.inline)
        .refreshable { await fetchDevices() }
        .task { await fetchDevices() }
        .overlay { if isLoading && devices.isEmpty { ProgressView() } }
    }

    private func fetchDevices() async {
        isLoading = true
        error = nil
        do {
            devices = try await agentService.gateway.fetchDevices()
        } catch {
            self.error = error.localizedDescription
        }
        isLoading = false
    }

    private func revokeDevice(id: String) {
        Task {
            do {
                try await agentService.gateway.revokeDevice(id: id)
                devices.removeAll { $0.id == id }
            } catch {
                self.error = error.localizedDescription
            }
        }
    }
}

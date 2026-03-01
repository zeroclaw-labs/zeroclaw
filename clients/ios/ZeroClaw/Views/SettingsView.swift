import SwiftUI

struct SettingsView: View {
    @EnvironmentObject var settingsManager: SettingsManager
    @EnvironmentObject var agentService: AgentService
    @Environment(\.dismiss) private var dismiss

    @State private var apiKeyInput = ""
    @State private var showApiKey = false
    @State private var pairingCode = ""
    @State private var isPairing = false
    @State private var pairingError: String?
    @State private var isLoadingRemote = false

    var body: some View {
        NavigationStack {
            Form {
                gatewaySection
                aiProviderSection
                if settingsManager.isGatewayConfigured {
                    managementSection
                }
                appearanceSection
                behaviorSection
                systemPromptSection
                aboutSection
            }
            .navigationTitle("Settings")
            .navigationBarTitleDisplayMode(.inline)
            .toolbarBackground(.visible, for: .navigationBar)
            .scrollContentBackground(.hidden)
            .background(Color(.systemGroupedBackground))
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Button {
                        dismiss()
                    } label: {
                        Image(systemName: "xmark")
                            .font(.footnote.weight(.semibold))
                            .foregroundStyle(.secondary)
                            .frame(width: 30, height: 30)
                            .contentShape(Rectangle())
                    }
                }
            }
            .onAppear {
                apiKeyInput = settingsManager.getApiKey() ?? ""
                if agentService.status.isActive {
                    isLoadingRemote = true
                    Task {
                        await agentService.loadRemoteSettings()
                        isLoadingRemote = false
                    }
                }
            }
            .onDisappear {
                if settingsManager.isGatewayConfigured {
                    Task { await agentService.pushSettings() }
                }
            }
            .presentationDetents([.large])
            .presentationDragIndicator(.visible)
        }
        .preferredColorScheme(settingsManager.colorScheme)
    }

    // MARK: - Sections

    private var gatewaySection: some View {
        Section {
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
            .onSubmit { agentService.reconfigure() }

            // Connection status
            HStack {
                if agentService.isRunningDiagnostics {
                    ProgressView()
                        .controlSize(.small)
                    Text(agentService.diagnosticMessage ?? "Checking...")
                        .foregroundStyle(.secondary)
                } else {
                    Circle()
                        .fill(agentService.status.color)
                        .frame(width: 8, height: 8)
                    Text(agentService.status.displayText)
                        .foregroundStyle(.secondary)
                }
                Spacer()
                if case .error = agentService.status, !agentService.isRunningDiagnostics {
                    Button {
                        agentService.reconfigure()
                    } label: {
                        Image(systemName: "arrow.clockwise")
                    }
                }
            }

            // Pairing
            if !settingsManager.isGatewayConfigured {
                HStack {
                    TextField("Pairing Code", text: $pairingCode)
                        .autocorrectionDisabled()
                        .textInputAutocapitalization(.never)
                        .font(.system(.body, design: .monospaced))

                    Button {
                        performPairing()
                    } label: {
                        if isPairing {
                            ProgressView()
                                .controlSize(.small)
                        } else {
                            Text("Pair")
                                .fontWeight(.medium)
                        }
                    }
                    .disabled(pairingCode.isEmpty || isPairing)
                }

                Label(
                    "Enter the pairing code shown in the gateway terminal",
                    systemImage: "key.horizontal"
                )
                .font(.caption)
                .foregroundStyle(.secondary)
            } else {
                HStack {
                    Label("Paired", systemImage: "checkmark.seal.fill")
                        .foregroundStyle(.green)
                    Spacer()
                    Button("Unpair", role: .destructive) {
                        settingsManager.deleteGatewayToken()
                        agentService.stop()
                        agentService.reconfigure()
                    }
                    .font(.caption)
                }
            }

            if let error = pairingError {
                Label(error, systemImage: "exclamationmark.triangle")
                    .font(.caption)
                    .foregroundStyle(.red)
            }
        } header: {
            Text("Gateway")
        }
    }

    private var aiProviderSection: some View {
        Section("AI Provider") {
            Picker("Provider", selection: $settingsManager.provider) {
                Text("Anthropic").tag("anthropic")
                Text("OpenAI").tag("openai")
                Text("Google").tag("google")
                Text("OpenRouter").tag("openrouter")
            }
            .onChange(of: settingsManager.provider) {
                let models = settingsManager.availableModels(for: settingsManager.provider)
                if !models.contains(settingsManager.model), let first = models.first {
                    settingsManager.model = first
                }
                if !isLoadingRemote {
                    Task { await agentService.pushSettings() }
                }
            }

            Picker("Model", selection: $settingsManager.model) {
                ForEach(settingsManager.availableModels(for: settingsManager.provider), id: \.self) { model in
                    Text(model).tag(model)
                }
            }
            .onChange(of: settingsManager.model) {
                if !isLoadingRemote {
                    Task { await agentService.pushSettings() }
                }
            }

            HStack {
                if showApiKey {
                    TextField("API Key", text: $apiKeyInput)
                        .textContentType(.password)
                        .autocorrectionDisabled()
                        .textInputAutocapitalization(.never)
                } else {
                    SecureField("API Key", text: $apiKeyInput)
                        .textContentType(.password)
                }

                Button {
                    showApiKey.toggle()
                } label: {
                    Image(systemName: showApiKey ? "eye.slash" : "eye")
                        .foregroundStyle(.secondary)
                }
                .buttonStyle(.plain)
            }
            .onChange(of: apiKeyInput) {
                if apiKeyInput.isEmpty {
                    settingsManager.deleteApiKey()
                } else {
                    settingsManager.setApiKey(apiKeyInput)
                }
            }

            Label("Stored securely in iOS Keychain", systemImage: "lock.shield")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
    }

    private var appearanceSection: some View {
        Section("Appearance") {
            Picker("Theme", selection: $settingsManager.appearance) {
                Text("System").tag("system")
                Text("Light").tag("light")
                Text("Dark").tag("dark")
            }
            .pickerStyle(.segmented)
        }
    }

    private var behaviorSection: some View {
        Section("Behavior") {
            Toggle("Auto-start on launch", isOn: $settingsManager.autoStart)
            Toggle("Notifications", isOn: $settingsManager.notificationsEnabled)
        }
    }

    private var systemPromptSection: some View {
        Section("System Prompt") {
            TextEditor(text: $settingsManager.systemPrompt)
                .frame(minHeight: 100)
                .font(.body)
        }
    }

    private var managementSection: some View {
        Section("Management") {
            NavigationLink("Memory") { MemoryView() }
            NavigationLink("Cron Jobs") { CronJobsView() }
            NavigationLink("Tools") { ToolsBrowserView() }
            NavigationLink("Usage & Cost") { CostView() }
            NavigationLink("Paired Devices") { PairedDevicesView() }
            NavigationLink("Integrations") { IntegrationsView() }
        }
    }

    private var aboutSection: some View {
        Section("About") {
            LabeledContent("App Version", value: appVersion)
            LabeledContent("ZeroClaw Core", value: "0.1.0")
        }
    }

    private var appVersion: String {
        Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String ?? "0.1.0"
    }

    // MARK: - Actions

    private func performPairing() {
        isPairing = true
        pairingError = nil

        Task {
            do {
                try await agentService.pair(code: pairingCode)
                pairingCode = ""
            } catch {
                pairingError = error.localizedDescription
            }
            isPairing = false
        }
    }

}

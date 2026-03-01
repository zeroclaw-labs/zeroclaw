import SwiftUI

struct SettingsView: View {
    @EnvironmentObject var settingsManager: SettingsManager
    @EnvironmentObject var agentService: AgentService
    @Environment(\.dismiss) private var dismiss

    @State private var apiKeyInput = ""
    @State private var showApiKey = false

    var body: some View {
        NavigationStack {
            Form {
                aiProviderSection
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
            }
            .presentationDetents([.large])
            .presentationDragIndicator(.visible)
        }
        .preferredColorScheme(settingsManager.colorScheme)
    }

    // MARK: - Sections

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
            }

            Picker("Model", selection: $settingsManager.model) {
                ForEach(settingsManager.availableModels(for: settingsManager.provider), id: \.self) { model in
                    Text(model).tag(model)
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

    private var aboutSection: some View {
        Section("About") {
            LabeledContent("App Version", value: appVersion)
            LabeledContent("ZeroClaw Core", value: "0.1.0")
        }
    }

    private var appVersion: String {
        Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String ?? "0.1.0"
    }
}

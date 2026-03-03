import SwiftUI

/// Settings screen for configuring the AI provider, API key, and behavior.
struct SettingsView: View {
    @EnvironmentObject var settings: SettingsStore
    @EnvironmentObject var agent: AgentManager
    @State private var showApiKey = false
    @State private var editedApiKey = ""
    @State private var editedProvider = ""
    @State private var editedModel = ""

    private let providers: [(id: String, name: String)] = [
        ("openrouter", "OpenRouter"),
        ("anthropic", "Anthropic"),
        ("openai", "OpenAI"),
        ("google", "Google Gemini"),
        ("ollama", "Ollama (Local)"),
    ]

    var body: some View {
        NavigationStack {
            Form {
                // Provider Section
                Section("AI Provider") {
                    Picker("Provider", selection: $editedProvider) {
                        ForEach(providers, id: \.id) { provider in
                            Text(provider.name).tag(provider.id)
                        }
                    }
                    .onChange(of: editedProvider) {
                        editedModel = defaultModel(for: editedProvider)
                    }

                    Picker("Model", selection: $editedModel) {
                        ForEach(models(for: editedProvider), id: \.id) { model in
                            Text(model.name).tag(model.id)
                        }
                    }

                    HStack {
                        if showApiKey {
                            TextField("API Key", text: $editedApiKey)
                                .textInputAutocapitalization(.never)
                                .autocorrectionDisabled()
                        } else {
                            SecureField("API Key", text: $editedApiKey)
                                .textInputAutocapitalization(.never)
                                .autocorrectionDisabled()
                        }

                        Button {
                            showApiKey.toggle()
                        } label: {
                            Image(systemName: showApiKey ? "eye.slash" : "eye")
                                .foregroundColor(.secondary)
                        }
                    }

                    Text("Your key is stored in iOS Keychain and never leaves this device.")
                        .font(.caption)
                        .foregroundColor(.secondary)
                }

                // Agent Section
                Section("Agent") {
                    HStack {
                        Text("Status")
                        Spacer()
                        statusBadge
                    }

                    Button(agent.status == .running ? "Restart Agent" : "Start Agent") {
                        agent.stop()
                        DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) {
                            agent.start(
                                provider: editedProvider,
                                apiKey: editedApiKey.isEmpty ? nil : editedApiKey
                            )
                        }
                    }

                    if agent.status == .running {
                        Button("Stop Agent", role: .destructive) {
                            agent.stop()
                        }
                    }
                }

                // About Section
                Section("About") {
                    HStack {
                        Text("MoA Version")
                        Spacer()
                        Text("0.1.0")
                            .foregroundColor(.secondary)
                    }
                    HStack {
                        Text("ZeroClaw Core")
                        Spacer()
                        Text("0.x.x")
                            .foregroundColor(.secondary)
                    }
                    HStack {
                        Text("Architecture")
                        Spacer()
                        Text("Local-first (in-process)")
                            .foregroundColor(.secondary)
                    }
                }
            }
            .navigationTitle("Settings")
            .onAppear {
                editedProvider = settings.provider
                editedApiKey = settings.apiKey
                editedModel = settings.model
            }
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Button("Save") {
                        settings.provider = editedProvider
                        settings.apiKey = editedApiKey
                        settings.model = editedModel
                    }
                    .fontWeight(.semibold)
                }
            }
        }
    }

    private var statusBadge: some View {
        HStack(spacing: 6) {
            Circle()
                .fill(statusColor)
                .frame(width: 8, height: 8)
            Text(statusText)
                .font(.subheadline)
                .foregroundColor(.secondary)
        }
    }

    private var statusColor: Color {
        switch agent.status {
        case .running: return .green
        case .starting: return .orange
        case .thinking: return .blue
        case .stopped: return .gray
        case .error: return .red
        }
    }

    private var statusText: String {
        switch agent.status {
        case .running: return "Running"
        case .starting: return "Starting..."
        case .thinking: return "Thinking..."
        case .stopped: return "Stopped"
        case .error: return "Error"
        }
    }

    // MARK: - Model Lists

    private struct ModelOption: Identifiable {
        let id: String
        let name: String
    }

    private func models(for provider: String) -> [ModelOption] {
        switch provider {
        case "anthropic":
            return [
                ModelOption(id: "claude-opus-4-5", name: "Claude Opus 4.5"),
                ModelOption(id: "claude-sonnet-4-5", name: "Claude Sonnet 4.5"),
                ModelOption(id: "claude-haiku-3-5", name: "Claude Haiku 3.5"),
            ]
        case "openai":
            return [
                ModelOption(id: "gpt-4o", name: "GPT-4o"),
                ModelOption(id: "gpt-4o-mini", name: "GPT-4o Mini"),
            ]
        case "google":
            return [
                ModelOption(id: "gemini-2.5-pro", name: "Gemini 2.5 Pro"),
                ModelOption(id: "gemini-2.5-flash", name: "Gemini 2.5 Flash"),
            ]
        case "openrouter":
            return [
                ModelOption(id: "auto", name: "Auto (recommended)"),
                ModelOption(id: "anthropic/claude-sonnet-4-5", name: "Claude Sonnet 4.5"),
                ModelOption(id: "openai/gpt-4o", name: "GPT-4o"),
            ]
        default:
            return [ModelOption(id: "auto", name: "Auto")]
        }
    }

    private func defaultModel(for provider: String) -> String {
        models(for: provider).first?.id ?? "auto"
    }
}

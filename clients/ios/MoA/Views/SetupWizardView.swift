import SwiftUI

/// First-time setup wizard.
/// Guides the user through: Language → Provider → API Key → Done.
struct SetupWizardView: View {
    @EnvironmentObject var settings: SettingsStore
    @EnvironmentObject var agent: AgentManager

    @State private var step = 0
    @State private var selectedProvider = "openrouter"
    @State private var apiKey = ""
    @State private var isSaving = false

    private let providers: [(id: String, name: String, desc: String)] = [
        ("openrouter", "OpenRouter", "Access 200+ models with one key"),
        ("anthropic", "Anthropic", "Claude models (Opus, Sonnet, Haiku)"),
        ("openai", "OpenAI", "GPT-4o, o1, and more"),
        ("google", "Google", "Gemini models"),
        ("ollama", "Ollama (Local)", "Free, runs on your device"),
    ]

    var body: some View {
        VStack(spacing: 0) {
            // Header
            VStack(spacing: 8) {
                Image(systemName: "brain.head.profile")
                    .font(.system(size: 48))
                    .foregroundStyle(
                        LinearGradient(
                            colors: [.blue, .purple],
                            startPoint: .topLeading,
                            endPoint: .bottomTrailing
                        )
                    )
                    .padding(.top, 40)

                Text("MoA")
                    .font(.largeTitle)
                    .fontWeight(.bold)

                Text("Master of AI")
                    .font(.subheadline)
                    .foregroundColor(.secondary)
            }
            .padding(.bottom, 24)

            // Step indicator
            HStack(spacing: 12) {
                ForEach(0..<3, id: \.self) { i in
                    Circle()
                        .fill(i <= step ? Color.blue : Color.gray.opacity(0.3))
                        .frame(width: 10, height: 10)
                }
            }
            .padding(.bottom, 32)

            // Content
            ScrollView {
                VStack(spacing: 20) {
                    switch step {
                    case 0:
                        welcomeStep
                    case 1:
                        providerStep
                    case 2:
                        apiKeyStep
                    default:
                        EmptyView()
                    }
                }
                .padding(.horizontal, 24)
            }

            Spacer()

            // Navigation
            HStack {
                if step > 0 {
                    Button("Back") { step -= 1 }
                        .buttonStyle(.bordered)
                }

                Spacer()

                Button(action: handleNext) {
                    if isSaving {
                        ProgressView()
                            .tint(.white)
                    } else {
                        Text(step == 2 ? "Start Chatting" : "Next")
                    }
                }
                .buttonStyle(.borderedProminent)
                .disabled(isSaving)
            }
            .padding(24)
        }
        .background(Color(.systemBackground))
    }

    // MARK: - Steps

    private var welcomeStep: some View {
        VStack(spacing: 16) {
            Text("Welcome!")
                .font(.title2)
                .fontWeight(.semibold)

            Text("MoA is your personal AI assistant that runs locally on your device. All messages stay private.")
                .multilineTextAlignment(.center)
                .foregroundColor(.secondary)

            VStack(alignment: .leading, spacing: 12) {
                featureRow(icon: "lock.shield.fill", title: "Private", desc: "Messages processed on your device")
                featureRow(icon: "bolt.fill", title: "Fast", desc: "No server round-trips for processing")
                featureRow(icon: "wrench.and.screwdriver.fill", title: "Powerful", desc: "Full AI agent with tools and memory")
            }
            .padding(.top, 16)
        }
    }

    private var providerStep: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("Choose your AI provider")
                .font(.title3)
                .fontWeight(.semibold)

            Text("You can change this later in Settings.")
                .font(.subheadline)
                .foregroundColor(.secondary)

            ForEach(providers, id: \.id) { provider in
                Button {
                    selectedProvider = provider.id
                } label: {
                    HStack {
                        Image(systemName: selectedProvider == provider.id ? "checkmark.circle.fill" : "circle")
                            .foregroundColor(selectedProvider == provider.id ? .blue : .gray)
                            .font(.title3)

                        VStack(alignment: .leading, spacing: 2) {
                            Text(provider.name)
                                .fontWeight(.medium)
                                .foregroundColor(.primary)
                            Text(provider.desc)
                                .font(.caption)
                                .foregroundColor(.secondary)
                        }

                        Spacer()
                    }
                    .padding(14)
                    .background(
                        RoundedRectangle(cornerRadius: 12)
                            .stroke(selectedProvider == provider.id ? Color.blue : Color.gray.opacity(0.3), lineWidth: selectedProvider == provider.id ? 2 : 1)
                    )
                }
            }
        }
    }

    private var apiKeyStep: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("Enter your API key")
                .font(.title3)
                .fontWeight(.semibold)

            Text("Your key is stored securely on this device only using iOS Keychain.")
                .font(.subheadline)
                .foregroundColor(.secondary)

            SecureField("sk-... or AIza...", text: $apiKey)
                .textFieldStyle(.roundedBorder)
                .autocorrectionDisabled()
                .textInputAutocapitalization(.never)

            Button("Skip (use credits)") {
                completeSetup(withKey: "")
            }
            .font(.subheadline)
            .foregroundColor(.secondary)
        }
    }

    // MARK: - Helpers

    private func featureRow(icon: String, title: String, desc: String) -> some View {
        HStack(spacing: 14) {
            Image(systemName: icon)
                .font(.title3)
                .foregroundColor(.blue)
                .frame(width: 32)

            VStack(alignment: .leading, spacing: 2) {
                Text(title).fontWeight(.medium)
                Text(desc)
                    .font(.caption)
                    .foregroundColor(.secondary)
            }
        }
    }

    private func handleNext() {
        switch step {
        case 0:
            step = 1
        case 1:
            if selectedProvider == "ollama" {
                completeSetup(withKey: "")
            } else {
                step = 2
            }
        case 2:
            completeSetup(withKey: apiKey.trimmingCharacters(in: .whitespacesAndNewlines))
        default:
            break
        }
    }

    private func completeSetup(withKey key: String) {
        isSaving = true
        settings.provider = selectedProvider
        settings.apiKey = key
        settings.setupComplete = true
        agent.start(
            provider: selectedProvider,
            apiKey: key.isEmpty ? nil : key
        )
        isSaving = false
    }
}

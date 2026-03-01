import Foundation
import SwiftUI

/// Manages persistent settings with encrypted storage via Keychain
/// and non-sensitive settings via UserDefaults.
final class SettingsManager: ObservableObject {
    @Published var provider: String {
        didSet { UserDefaults.standard.set(provider, forKey: Keys.provider) }
    }
    @Published var model: String {
        didSet { UserDefaults.standard.set(model, forKey: Keys.model) }
    }
    @Published var systemPrompt: String {
        didSet { UserDefaults.standard.set(systemPrompt, forKey: Keys.systemPrompt) }
    }
    @Published var autoStart: Bool {
        didSet { UserDefaults.standard.set(autoStart, forKey: Keys.autoStart) }
    }
    @Published var notificationsEnabled: Bool {
        didSet { UserDefaults.standard.set(notificationsEnabled, forKey: Keys.notifications) }
    }
    @Published var appearance: String {
        didSet { UserDefaults.standard.set(appearance, forKey: Keys.appearance) }
    }
    @Published var gatewayHost: String {
        didSet { UserDefaults.standard.set(gatewayHost, forKey: Keys.gatewayHost) }
    }
    @Published var gatewayPort: Int {
        didSet { UserDefaults.standard.set(gatewayPort, forKey: Keys.gatewayPort) }
    }

    var isGatewayConfigured: Bool {
        !gatewayHost.isEmpty && gatewayPort > 0 && getGatewayToken() != nil
    }

    init() {
        let defaults = UserDefaults.standard
        self.provider = defaults.string(forKey: Keys.provider) ?? "anthropic"
        self.model = defaults.string(forKey: Keys.model) ?? "claude-sonnet-4-5"
        self.systemPrompt = defaults.string(forKey: Keys.systemPrompt) ?? ""
        self.autoStart = defaults.bool(forKey: Keys.autoStart)
        self.notificationsEnabled = defaults.bool(forKey: Keys.notifications)
        self.appearance = defaults.string(forKey: Keys.appearance) ?? "system"
        self.gatewayHost = defaults.string(forKey: Keys.gatewayHost) ?? "127.0.0.1"
        self.gatewayPort = defaults.object(forKey: Keys.gatewayPort) as? Int ?? 42617
    }

    var colorScheme: ColorScheme? {
        switch appearance {
        case "light": .light
        case "dark": .dark
        default: nil
        }
    }

    // MARK: - API Key

    func getApiKey() -> String? {
        KeychainHelper.read(account: KeychainHelper.apiKeyAccount)
    }

    func setApiKey(_ key: String) {
        KeychainHelper.write(account: KeychainHelper.apiKeyAccount, value: key)
        objectWillChange.send()
    }

    func deleteApiKey() {
        KeychainHelper.delete(account: KeychainHelper.apiKeyAccount)
        objectWillChange.send()
    }

    // MARK: - Gateway Token

    func getGatewayToken() -> String? {
        KeychainHelper.read(account: KeychainHelper.gatewayTokenAccount)
    }

    func setGatewayToken(_ token: String) {
        KeychainHelper.write(account: KeychainHelper.gatewayTokenAccount, value: token)
        objectWillChange.send()
    }

    func deleteGatewayToken() {
        KeychainHelper.delete(account: KeychainHelper.gatewayTokenAccount)
        objectWillChange.send()
    }

    // MARK: - Provider Models

    func availableModels(for provider: String) -> [String] {
        switch provider {
        case "anthropic":
            ["claude-sonnet-4-5", "claude-haiku-4-5", "claude-opus-4-5"]
        case "openai":
            ["gpt-4o", "gpt-4o-mini", "o1", "o1-mini"]
        case "google":
            ["gemini-2.0-flash", "gemini-2.0-pro", "gemini-1.5-pro"]
        case "openrouter":
            ["auto", "anthropic/claude-sonnet-4-5", "openai/gpt-4o"]
        default:
            []
        }
    }

    // MARK: - Constants

    private enum Keys {
        static let provider = "zeroclaw_provider"
        static let model = "zeroclaw_model"
        static let systemPrompt = "zeroclaw_system_prompt"
        static let autoStart = "zeroclaw_auto_start"
        static let notifications = "zeroclaw_notifications"
        static let appearance = "zeroclaw_appearance"
        static let gatewayHost = "zeroclaw_gateway_host"
        static let gatewayPort = "zeroclaw_gateway_port"
    }
}

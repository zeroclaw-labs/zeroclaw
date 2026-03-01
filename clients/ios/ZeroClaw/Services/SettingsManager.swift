import Foundation
import Security
import SwiftUI

/// Manages persistent settings with encrypted API key storage via Keychain
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

    var isConfigured: Bool {
        let key = getApiKey()
        return key != nil && !key!.isEmpty
    }

    init() {
        let defaults = UserDefaults.standard
        self.provider = defaults.string(forKey: Keys.provider) ?? "anthropic"
        self.model = defaults.string(forKey: Keys.model) ?? "claude-sonnet-4-5"
        self.systemPrompt = defaults.string(forKey: Keys.systemPrompt) ?? ""
        self.autoStart = defaults.bool(forKey: Keys.autoStart)
        self.notificationsEnabled = defaults.bool(forKey: Keys.notifications)
        self.appearance = defaults.string(forKey: Keys.appearance) ?? "system"
    }

    var colorScheme: ColorScheme? {
        switch appearance {
        case "light": .light
        case "dark": .dark
        default: nil
        }
    }

    // MARK: - API Key (Keychain)

    func getApiKey() -> String? {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: KeychainConfig.service,
            kSecAttrAccount as String: KeychainConfig.apiKeyAccount,
            kSecReturnData as String: true,
            kSecMatchLimit as String: kSecMatchLimitOne,
        ]

        var result: AnyObject?
        let status = SecItemCopyMatching(query as CFDictionary, &result)

        guard status == errSecSuccess,
              let data = result as? Data,
              let key = String(data: data, encoding: .utf8) else {
            return nil
        }
        return key
    }

    func setApiKey(_ key: String) {
        let data = Data(key.utf8)

        // Delete existing entry first
        let deleteQuery: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: KeychainConfig.service,
            kSecAttrAccount as String: KeychainConfig.apiKeyAccount,
        ]
        SecItemDelete(deleteQuery as CFDictionary)

        // Add new entry
        let addQuery: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: KeychainConfig.service,
            kSecAttrAccount as String: KeychainConfig.apiKeyAccount,
            kSecValueData as String: data,
            kSecAttrAccessible as String: kSecAttrAccessibleWhenUnlockedThisDeviceOnly,
        ]
        SecItemAdd(addQuery as CFDictionary, nil)

        objectWillChange.send()
    }

    func deleteApiKey() {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: KeychainConfig.service,
            kSecAttrAccount as String: KeychainConfig.apiKeyAccount,
        ]
        SecItemDelete(query as CFDictionary)
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
    }

    private enum KeychainConfig {
        static let service = "ai.zeroclaw.ios"
        static let apiKeyAccount = "api_key"
    }
}

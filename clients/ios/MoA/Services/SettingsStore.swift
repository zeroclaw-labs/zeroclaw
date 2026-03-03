import Foundation
import SwiftUI
import Security

/// Persistent settings store using UserDefaults and iOS Keychain.
///
/// General settings are stored in UserDefaults.
/// API keys are stored in the iOS Keychain for security.
class SettingsStore: ObservableObject {
    @Published var setupComplete: Bool {
        didSet { UserDefaults.standard.set(setupComplete, forKey: "setupComplete") }
    }

    @Published var provider: String {
        didSet { UserDefaults.standard.set(provider, forKey: "provider") }
    }

    @Published var model: String {
        didSet { UserDefaults.standard.set(model, forKey: "model") }
    }

    @Published var apiKey: String {
        didSet { Self.saveToKeychain(key: "zeroclaw_api_key", value: apiKey) }
    }

    init() {
        self.setupComplete = UserDefaults.standard.bool(forKey: "setupComplete")
        self.provider = UserDefaults.standard.string(forKey: "provider") ?? "openrouter"
        self.model = UserDefaults.standard.string(forKey: "model") ?? "auto"
        self.apiKey = Self.loadFromKeychain(key: "zeroclaw_api_key") ?? ""
    }

    // MARK: - Keychain Operations

    private static let service = "ai.zeroclaw.moa"

    static func saveToKeychain(key: String, value: String) {
        let data = Data(value.utf8)

        // Delete existing
        let deleteQuery: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: key,
        ]
        SecItemDelete(deleteQuery as CFDictionary)

        guard !value.isEmpty else { return }

        // Add new
        let addQuery: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: key,
            kSecValueData as String: data,
            kSecAttrAccessible as String: kSecAttrAccessibleAfterFirstUnlock,
        ]
        SecItemAdd(addQuery as CFDictionary, nil)
    }

    static func loadFromKeychain(key: String) -> String? {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: key,
            kSecReturnData as String: true,
            kSecMatchLimit as String: kSecMatchLimitOne,
        ]

        var result: AnyObject?
        let status = SecItemCopyMatching(query as CFDictionary, &result)

        guard status == errSecSuccess, let data = result as? Data else {
            return nil
        }

        return String(data: data, encoding: .utf8)
    }
}

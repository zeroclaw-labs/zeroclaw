import AppIntents
import Foundation

/// Ask ZeroClaw a question via Siri or Shortcuts.
/// Uses HTTP POST /api/chat for a one-shot agent interaction.
struct AskZeroClawIntent: AppIntent {
    static var title: LocalizedStringResource = "Ask ZeroClaw"
    static var description: IntentDescription = "Send a message to your ZeroClaw agent and get a response."

    @Parameter(title: "Message")
    var message: String

    static var parameterSummary: some ParameterSummary {
        Summary("Ask ZeroClaw \(\.$message)")
    }

    func perform() async throws -> some IntentResult & ReturnsValue<String> & ProvidesDialog {
        let (client, configured) = await MainActor.run {
            let c = makeClient()
            return (c, c.isConfigured)
        }

        guard configured else {
            return .result(
                value: "Not configured",
                dialog: "ZeroClaw is not paired with a gateway. Open the app to set up."
            )
        }

        do {
            let response = try await client.chat(message: message)
            return .result(value: response.reply, dialog: "\(response.reply)")
        } catch {
            return .result(
                value: "Error",
                dialog: "Failed to reach ZeroClaw: \(error.localizedDescription)"
            )
        }
    }

    @MainActor
    private func makeClient() -> GatewayClient {
        let defaults = UserDefaults.standard
        let host = defaults.string(forKey: "zeroclaw_gateway_host") ?? "127.0.0.1"
        let port = defaults.object(forKey: "zeroclaw_gateway_port") as? Int ?? 42617
        let token = KeychainHelper.read(account: KeychainHelper.gatewayTokenAccount)

        let client = GatewayClient()
        client.configure(host: host, port: port, token: token)
        return client
    }
}

/// Check the current status of the ZeroClaw gateway.
struct CheckStatusIntent: AppIntent {
    static var title: LocalizedStringResource = "Check ZeroClaw Status"
    static var description: IntentDescription = "Check if the ZeroClaw gateway is running and accessible."

    func perform() async throws -> some IntentResult & ProvidesDialog {
        let defaults = UserDefaults.standard
        let host = defaults.string(forKey: "zeroclaw_gateway_host") ?? "127.0.0.1"
        let port = defaults.object(forKey: "zeroclaw_gateway_port") as? Int ?? 42617

        let client = await MainActor.run {
            let c = GatewayClient()
            c.configure(host: host, port: port, token: nil)
            return c
        }

        do {
            let healthy = try await client.healthCheck()
            if healthy {
                return .result(dialog: "ZeroClaw gateway at \(host):\(port) is running.")
            } else {
                return .result(dialog: "ZeroClaw gateway at \(host):\(port) is unreachable.")
            }
        } catch {
            return .result(dialog: "Cannot reach ZeroClaw gateway: \(error.localizedDescription)")
        }
    }
}

/// Toggle the ZeroClaw agent connection.
struct ToggleAgentIntent: AppIntent {
    static var title: LocalizedStringResource = "Toggle ZeroClaw"
    static var description: IntentDescription = "Connect or disconnect from the ZeroClaw agent."

    func perform() async throws -> some IntentResult & ProvidesDialog {
        let defaults = UserDefaults(suiteName: "group.ai.zeroclaw")
        let isConnected = defaults?.bool(forKey: "widget_connected") ?? false

        if isConnected {
            return .result(dialog: "ZeroClaw is connected. Use the app to disconnect.")
        } else {
            return .result(dialog: "ZeroClaw is disconnected. Open the app to connect.")
        }
    }
}


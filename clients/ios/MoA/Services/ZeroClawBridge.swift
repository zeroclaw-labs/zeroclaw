import Foundation

/// Swift wrapper around the ZeroClaw C-FFI bridge.
///
/// This file imports functions from the zeroclaw_bridge.h C header
/// (linked via the Bridging Header). The actual implementation is in
/// the zeroclaw_ios.a static library built from clients/ios-bridge/.
///
/// Usage:
///   // Start the gateway
///   ZeroClawBridgeWrapper.start(dataDir: "...", provider: "gemini", apiKey: "...")
///
///   // Send a message
///   let response = ZeroClawBridgeWrapper.sendMessage("Hello")
///
///   // Stop
///   ZeroClawBridgeWrapper.stop()
///
/// Note: The C functions (zeroclaw_start, zeroclaw_send_message, etc.)
/// are automatically available in Swift via the Bridging Header.
/// AgentManager uses them directly. This wrapper provides a
/// higher-level Swift-native interface for convenience.

enum ZeroClawBridgeWrapper {
    /// Start the in-process ZeroClaw gateway.
    /// - Returns: true on success
    static func start(dataDir: String, provider: String, apiKey: String?, port: UInt16 = 3000) -> Bool {
        let result = dataDir.withCString { dataDirPtr in
            provider.withCString { providerPtr in
                if let key = apiKey {
                    return key.withCString { keyPtr in
                        zeroclaw_start(dataDirPtr, providerPtr, keyPtr, port)
                    }
                } else {
                    return zeroclaw_start(dataDirPtr, providerPtr, nil, port)
                }
            }
        }
        return result == 0
    }

    /// Send a message and get the response.
    /// - Returns: Response string or nil on error
    static func sendMessage(_ message: String) -> String? {
        guard let responsePtr = message.withCString({ zeroclaw_send_message($0) }) else {
            return nil
        }
        let response = String(cString: responsePtr)
        zeroclaw_free_string(responsePtr)
        return response
    }

    /// Check if the gateway is running.
    static func isRunning() -> Bool {
        zeroclaw_get_status() == 1
    }

    /// Stop the gateway.
    static func stop() {
        zeroclaw_stop()
    }

    /// Set the auth token.
    static func setToken(_ token: String?) {
        if let token = token {
            token.withCString { zeroclaw_set_token($0) }
        } else {
            zeroclaw_set_token(nil)
        }
    }
}

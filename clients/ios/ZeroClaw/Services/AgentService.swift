import Foundation

// MARK: - Diagnostic Step

enum DiagnosticStep: Equatable {
    case pending, checking, passed, failed(String), skipped
}

// MARK: - Agent Service

/// Manages ZeroClaw agent lifecycle, message state, and gateway communication.
/// Uses event-based WebSocket for real-time chat with streaming support.
@MainActor
final class AgentService: ObservableObject {
    @Published private(set) var status: AgentStatus = .stopped
    @Published private(set) var messages: [ChatMessage] = []
    @Published private(set) var connectionError: String?
    @Published private(set) var isStreaming = false

    @Published private(set) var isRunningDiagnostics = false
    @Published private(set) var diagnosticMessage: String?
    @Published private(set) var pairingDiagnostic: DiagnosticStep = .pending
    @Published private(set) var reachabilityDiagnostic: DiagnosticStep = .pending
    @Published private(set) var connectionDiagnostic: DiagnosticStep = .pending

    let gateway = GatewayClient()

    private var settingsManager: SettingsManager?
    private var eventStreamTask: Task<Void, Never>?

    // Streaming state
    private var streamingMessageId: String?

    // Auto-reconnect
    private var reconnectTask: Task<Void, Never>?
    private var reconnectAttempts = 0
    private let maxReconnectAttempts = 10
    private let maxReconnectDelay: TimeInterval = 30

    // Persistence
    private(set) var sessionId = UUID().uuidString

    // MARK: - Configuration

    func configure(settings: SettingsManager) {
        guard settingsManager == nil else { return }
        settingsManager = settings
        gateway.configure(
            host: settings.gatewayHost,
            port: settings.gatewayPort,
            token: settings.getGatewayToken()
        )
        loadSession()
    }

    func reconfigure() {
        guard let settings = settingsManager else { return }
        stop()
        gateway.configure(
            host: settings.gatewayHost,
            port: settings.gatewayPort,
            token: settings.getGatewayToken()
        )
        Task { await runDiagnostics() }
    }

    // MARK: - Lifecycle

    func stop() {
        cancelReconnect()
        gateway.disconnectWebSocket()
        gateway.disconnectEventStream()
        eventStreamTask?.cancel()
        eventStreamTask = nil
        finalizeStreaming()
        status = .stopped
    }

    func sendMessage(_ content: String) {
        let trimmed = content.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return }

        guard gateway.isConnected else {
            handleError("Not connected to gateway")
            return
        }

        messages.append(.now(content: trimmed, role: "user"))
        status = .thinking
        persistSession()

        Task {
            do {
                try await gateway.sendMessage(trimmed)
            } catch {
                handleError(error.localizedDescription)
            }
        }
    }

    func clearMessages() {
        messages.removeAll()
        persistSession()
    }

    func newSession() {
        clearMessages()
        sessionId = UUID().uuidString
    }

    /// Pair with the gateway using a one-time code.
    func pair(code: String) async throws {
        guard let settings = settingsManager else { return }

        gateway.configure(
            host: settings.gatewayHost,
            port: settings.gatewayPort,
            token: nil
        )

        let token = try await gateway.pair(code: code)
        settings.setGatewayToken(token)
        reconfigure()
    }

    // MARK: - Diagnostics

    func runDiagnostics() async {
        guard !isRunningDiagnostics else { return }
        isRunningDiagnostics = true
        defer {
            isRunningDiagnostics = false
            diagnosticMessage = nil
        }

        pairingDiagnostic = .pending
        reachabilityDiagnostic = .pending
        connectionDiagnostic = .pending

        // 1. Pairing
        diagnosticMessage = "Checking pairing..."
        pairingDiagnostic = .checking

        guard settingsManager?.isGatewayConfigured == true else {
            pairingDiagnostic = .failed("Device not paired")
            reachabilityDiagnostic = .skipped
            connectionDiagnostic = .skipped
            status = .error(message: "Device not paired")
            return
        }
        pairingDiagnostic = .passed

        // 2. Reachability
        diagnosticMessage = "Checking reachability..."
        reachabilityDiagnostic = .checking
        do {
            guard try await gateway.healthCheck() else {
                reachabilityDiagnostic = .failed("Gateway not responding")
                connectionDiagnostic = .skipped
                status = .error(message: "Gateway not responding")
                return
            }
            reachabilityDiagnostic = .passed
        } catch {
            reachabilityDiagnostic = .failed("Gateway unreachable")
            connectionDiagnostic = .skipped
            status = .error(message: "Gateway unreachable")
            return
        }

        // 3. Connection
        diagnosticMessage = "Connecting..."
        connectionDiagnostic = .checking
        if !gateway.isConnected {
            connectWebSocket()
            for _ in 0..<50 {
                try? await Task.sleep(for: .milliseconds(100))
                if gateway.isConnected { break }
            }
        }

        if gateway.isConnected {
            connectionDiagnostic = .passed
            status = .running
        } else {
            connectionDiagnostic = .failed("Could not establish connection")
            status = .error(message: "Could not establish connection")
        }
    }

    // MARK: - Remote Settings Sync

    func loadRemoteSettings() async {
        guard gateway.isConfigured, let settings = settingsManager else { return }
        do {
            let response = try await gateway.getConfig()
            let toml = response.content

            if let provider = tomlValue(for: "default_provider", in: toml) {
                settings.provider = provider
            }
            if let model = tomlValue(for: "default_model", in: toml) {
                settings.model = model
            }
            if let prompt = tomlValue(for: "system_prompt", in: toml) {
                settings.systemPrompt = prompt
            }
        } catch {
            // Network error — local settings remain as fallback
        }
    }

    func pushSettings() async {
        guard gateway.isConfigured, let settings = settingsManager else { return }
        do {
            let response = try await gateway.getConfig()
            var toml = response.content

            toml = setTomlValue(for: "default_provider", value: settings.provider, in: toml)
            toml = setTomlValue(for: "default_model", value: settings.model, in: toml)

            if !settings.systemPrompt.isEmpty {
                toml = setTomlValue(for: "system_prompt", value: settings.systemPrompt, in: toml)
            }

            if let apiKey = settings.getApiKey(), !apiKey.isEmpty {
                toml = setTomlValue(for: "api_key", value: apiKey, in: toml)
            }

            try await gateway.updateConfig(toml)
        } catch {
            // Push failed — settings remain local only
        }
    }

    // MARK: - Private — Connection

    private func connectWebSocket() {
        status = .starting
        connectionError = nil
        cancelReconnect()

        gateway.connectWebSocket { [weak self] event in
            Task { @MainActor in
                self?.handleWebSocketEvent(event)
            }
        }

        startEventStream()
    }

    private func startEventStream() {
        eventStreamTask?.cancel()
        let stream = gateway.connectEventStream()
        eventStreamTask = Task { [weak self] in
            for await event in stream {
                guard let self else { break }
                await MainActor.run { self.handleGatewayEvent(event) }
            }
        }
    }

    // MARK: - Private — WebSocket Event Dispatch

    private func handleWebSocketEvent(_ event: WebSocketEvent) {
        switch event {
        case .connected:
            reconnectAttempts = 0
            cancelReconnect()
            status = .running

        case .history(let history):
            messages = history.map { .now(content: $0.content, role: $0.role) }
            persistSession()

        case .chunk(let content):
            handleChunk(content)

        case .toolCall(let name, let args):
            appendToolMessage(role: "tool_call", name: name, body: args)

        case .toolResult(let name, let output, let success):
            appendToolMessage(role: "tool_result", name: name, body: output, success: success)

        case .done(let response):
            handleDone(response)

        case .error(let message):
            handleError(message)

        case .disconnected:
            handleDisconnect()
        }
    }

    // MARK: - Private — Streaming

    private func handleChunk(_ content: String) {
        if let id = streamingMessageId,
           let index = messages.firstIndex(where: { $0.id == id }) {
            // Append to existing streaming message
            let existing = messages[index]
            messages[index] = ChatMessage(
                id: existing.id,
                content: existing.content + content,
                role: "assistant",
                timestampMs: existing.timestampMs
            )
        } else {
            // Start new streaming message
            let msg = ChatMessage.now(content: content, role: "assistant")
            streamingMessageId = msg.id
            messages.append(msg)
        }
        isStreaming = true
        status = .thinking
    }

    private func handleDone(_ fullResponse: String) {
        if let id = streamingMessageId,
           let index = messages.firstIndex(where: { $0.id == id }) {
            // Replace with complete response
            let existing = messages[index]
            messages[index] = ChatMessage(
                id: existing.id,
                content: fullResponse,
                role: "assistant",
                timestampMs: existing.timestampMs
            )
        } else {
            // No streaming was happening — append full response
            messages.append(.now(content: fullResponse, role: "assistant"))
        }
        finalizeStreaming()
        status = .running
        persistSession()
        updateSharedState()
        notifyIfBackgrounded(content: fullResponse)
    }

    private func finalizeStreaming() {
        streamingMessageId = nil
        isStreaming = false
    }

    // MARK: - Private — Tool Messages

    private func appendToolMessage(role: String, name: String, body: String, success: Bool = true) {
        var json: [String: Any] = ["name": name]
        if role == "tool_call" {
            json["args"] = body
        } else {
            json["output"] = body
            json["success"] = success
        }

        let content: String
        if let data = try? JSONSerialization.data(withJSONObject: json),
           let str = String(data: data, encoding: .utf8) {
            content = str
        } else {
            content = "\(name): \(body)"
        }

        messages.append(.now(content: content, role: role))
    }

    // MARK: - Private — Error & Disconnect

    private func handleError(_ error: String) {
        connectionError = error
        if status == .thinking {
            finalizeStreaming()
            messages.append(.now(content: "Error: \(error)", role: "assistant"))
            status = .running
        } else {
            status = .error(message: error)
        }
        persistSession()
    }

    private func handleDisconnect() {
        finalizeStreaming()
        guard status != .stopped else { return }

        status = .stopped
        connectionError = "Disconnected from gateway"
        if connectionDiagnostic == .passed {
            connectionDiagnostic = .failed("Connection lost")
        }
        scheduleReconnect()
    }

    // MARK: - Private — Auto-Reconnect

    private func scheduleReconnect() {
        guard reconnectAttempts < maxReconnectAttempts else { return }
        guard settingsManager?.isGatewayConfigured == true else { return }

        reconnectAttempts += 1
        let delay = min(pow(2.0, Double(reconnectAttempts - 1)), maxReconnectDelay)

        reconnectTask = Task { [weak self] in
            try? await Task.sleep(for: .seconds(delay))
            guard let self, !Task.isCancelled else { return }
            self.connectWebSocket()
        }
    }

    private func cancelReconnect() {
        reconnectTask?.cancel()
        reconnectTask = nil
    }

    // MARK: - Private — SSE Event Handler

    private func handleGatewayEvent(_ event: GatewayEvent) {
        switch event.type {
        case "agent_start":
            if status == .running { status = .thinking }
        case "agent_end":
            if status == .thinking { status = .running }
        case "error":
            connectionError = event.message
        default:
            break
        }
    }

    // MARK: - Private — Persistence

    private func persistSession() {
        ChatStore.shared.save(messages: messages, sessionId: sessionId)
    }

    private func loadSession() {
        let loaded = ChatStore.shared.load(sessionId: sessionId)
        if !loaded.isEmpty {
            messages = loaded
        }
    }

    // MARK: - Private — Notifications

    private func notifyIfBackgrounded(content: String) {
        guard let manager = settingsManager, manager.notificationsEnabled else { return }
        NotificationManager.shared.postMessageNotification(content: content)
    }

    // MARK: - Shared State (App Groups)

    static let appGroupID = "group.ai.zeroclaw"

    func updateSharedState() {
        guard let defaults = UserDefaults(suiteName: Self.appGroupID) else { return }
        defaults.set(status.displayText, forKey: "widget_status")
        defaults.set(status.isActive, forKey: "widget_connected")
        if let last = messages.last {
            defaults.set(last.content, forKey: "widget_last_message")
            defaults.set(last.role, forKey: "widget_last_role")
            defaults.set(last.timestampMs, forKey: "widget_last_timestamp")
        }
    }

    func processPendingSharedMessages() {
        guard let defaults = UserDefaults(suiteName: Self.appGroupID) else { return }
        guard let pending = defaults.stringArray(forKey: "shared_pending_messages"),
              !pending.isEmpty else { return }

        defaults.removeObject(forKey: "shared_pending_messages")

        for message in pending {
            sendMessage(message)
        }
    }

    // MARK: - Private — TOML Helpers

    private func tomlValue(for key: String, in toml: String) -> String? {
        for line in toml.components(separatedBy: "\n") {
            let trimmed = line.trimmingCharacters(in: .whitespaces)
            guard !trimmed.hasPrefix("#"), trimmed.hasPrefix(key) else { continue }
            let afterKey = String(trimmed.dropFirst(key.count)).trimmingCharacters(in: .whitespaces)
            guard afterKey.hasPrefix("=") else { continue }
            let raw = String(afterKey.dropFirst()).trimmingCharacters(in: .whitespaces)
            if raw.hasPrefix("\"") && raw.hasSuffix("\"") && raw.count >= 2 {
                return unescapeToml(String(raw.dropFirst().dropLast()))
            }
            return String(raw)
        }
        return nil
    }

    private func setTomlValue(for key: String, value: String, in toml: String) -> String {
        let escaped = escapeToml(value)
        let newLine = "\(key) = \"\(escaped)\""

        var lines = toml.components(separatedBy: "\n")
        for (i, line) in lines.enumerated() {
            let trimmed = line.trimmingCharacters(in: .whitespaces)
            guard !trimmed.hasPrefix("#"), trimmed.hasPrefix(key) else { continue }
            let afterKey = String(trimmed.dropFirst(key.count)).trimmingCharacters(in: .whitespaces)
            guard afterKey.hasPrefix("=") else { continue }
            lines[i] = newLine
            return lines.joined(separator: "\n")
        }

        var insertAt = lines.count
        for (i, line) in lines.enumerated() {
            if line.trimmingCharacters(in: .whitespaces).hasPrefix("[") {
                insertAt = i
                break
            }
        }
        lines.insert(newLine, at: insertAt)
        return lines.joined(separator: "\n")
    }

    private func escapeToml(_ s: String) -> String {
        var result = ""
        for c in s {
            switch c {
            case "\\": result += "\\\\"
            case "\"": result += "\\\""
            case "\n": result += "\\n"
            case "\t": result += "\\t"
            default: result.append(c)
            }
        }
        return result
    }

    private func unescapeToml(_ s: String) -> String {
        var result = ""
        var i = s.startIndex
        while i < s.endIndex {
            if s[i] == "\\" {
                let next = s.index(after: i)
                if next < s.endIndex {
                    switch s[next] {
                    case "n": result.append("\n")
                    case "t": result.append("\t")
                    case "\\": result.append("\\")
                    case "\"": result.append("\"")
                    default:
                        result.append("\\")
                        result.append(s[next])
                    }
                    i = s.index(after: next)
                } else {
                    result.append("\\")
                    i = s.index(after: i)
                }
            } else {
                result.append(s[i])
                i = s.index(after: i)
            }
        }
        return result
    }
}

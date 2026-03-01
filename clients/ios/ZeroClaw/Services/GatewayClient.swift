import Foundation

/// Pure-Swift networking client for the ZeroClaw gateway.
/// Uses URLSession for HTTP and URLSessionWebSocketTask for real-time chat.
/// No third-party dependencies.
@MainActor
final class GatewayClient: ObservableObject {
    @Published private(set) var isConnected = false

    private var webSocketTask: URLSessionWebSocketTask?
    private var eventStreamTask: Task<Void, Never>?
    private let session = URLSession(configuration: .default)

    private var baseURL: URL?
    private var bearerToken: String?

    // MARK: - Configuration

    func configure(host: String, port: Int, token: String?) {
        baseURL = URL(string: "http://\(host):\(port)")
        bearerToken = token
    }

    var isConfigured: Bool {
        baseURL != nil && bearerToken != nil && !(bearerToken?.isEmpty ?? true)
    }

    // MARK: - Pairing

    func pair(code: String) async throws -> String {
        guard let url = baseURL?.appendingPathComponent("pair") else {
            throw GatewayError.notConfigured
        }

        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue(code, forHTTPHeaderField: "X-Pairing-Code")

        let (data, response) = try await session.data(for: request)
        let status = (response as? HTTPURLResponse)?.statusCode ?? 0

        guard status == 200 else {
            let body = try? JSONDecoder().decode(ErrorResponse.self, from: data)
            if status == 429 {
                throw GatewayError.rateLimited(body?.error ?? "Too many attempts")
            }
            throw GatewayError.pairingFailed(body?.error ?? "Invalid pairing code")
        }

        let result = try JSONDecoder().decode(PairResponse.self, from: data)
        bearerToken = result.token
        return result.token
    }

    // MARK: - WebSocket Chat (Event-Based)

    /// Connect to the gateway WebSocket. All events dispatched via a single callback.
    func connectWebSocket(onEvent: @escaping (WebSocketEvent) -> Void) {
        guard let base = baseURL, let token = bearerToken else { return }

        let wsScheme = base.scheme == "https" ? "wss" : "ws"
        guard let wsURL = URL(string: "\(wsScheme)://\(base.host ?? ""):\(base.port ?? 42617)/ws/chat?token=\(token)") else {
            return
        }

        let task = session.webSocketTask(with: wsURL)
        webSocketTask = task
        task.resume()

        Task { [weak self] in
            await self?.receiveLoop(task: task, onEvent: onEvent)
        }
    }

    func sendMessage(_ content: String) async throws {
        guard let task = webSocketTask else {
            throw GatewayError.notConnected
        }

        let payload = try JSONEncoder().encode(WsSendMessage(type: "message", content: content))
        guard let jsonString = String(data: payload, encoding: .utf8) else {
            throw GatewayError.encodingError
        }

        try await task.send(.string(jsonString))
    }

    func disconnectWebSocket() {
        webSocketTask?.cancel(with: .normalClosure, reason: nil)
        webSocketTask = nil
        isConnected = false
    }

    // MARK: - HTTP API — Core

    func healthCheck() async throws -> Bool {
        let (_, response) = try await httpGet(path: "health", authenticated: false, timeout: 3)
        return (response as? HTTPURLResponse)?.statusCode == 200
    }

    func getStatus() async throws -> GatewayStatus {
        let (data, _) = try await httpGet(path: "api/status")
        return try JSONDecoder().decode(GatewayStatus.self, from: data)
    }

    func chat(message: String, context: [String]? = nil) async throws -> ChatResponse {
        var body: [String: Any] = ["message": message]
        if let context { body["context"] = context }
        return try await httpPost(path: "api/chat", body: body)
    }

    func getConfig() async throws -> GatewayConfigResponse {
        let (data, _) = try await httpGet(path: "api/config")
        return try JSONDecoder().decode(GatewayConfigResponse.self, from: data)
    }

    func updateConfig(_ toml: String) async throws {
        guard let url = baseURL?.appendingPathComponent("api/config") else {
            throw GatewayError.notConfigured
        }

        var request = URLRequest(url: url)
        request.httpMethod = "PUT"
        request.setValue("text/plain", forHTTPHeaderField: "Content-Type")
        addAuth(&request)
        request.httpBody = Data(toml.utf8)

        let (data, response) = try await session.data(for: request)
        try checkResponse(response, data: data)
    }

    // MARK: - HTTP API — Memory

    func fetchMemory(query: String? = nil) async throws -> [MemoryEntry] {
        var path = "api/memory"
        if let query, !query.isEmpty {
            path += "?query=\(query.addingPercentEncoding(withAllowedCharacters: .urlQueryAllowed) ?? query)"
        }
        let (data, _) = try await httpGet(path: path)

        // Try structured response first, then raw array
        if let response = try? JSONDecoder().decode(MemoryListResponse.self, from: data) {
            return response.items
        }
        return (try? JSONDecoder().decode([MemoryEntry].self, from: data)) ?? []
    }

    func storeMemory(key: String, content: String, category: String?) async throws {
        var body: [String: Any] = ["key": key, "content": content]
        if let category { body["category"] = category }
        let _: EmptyResponse = try await httpPost(path: "api/memory", body: body)
    }

    func deleteMemory(key: String) async throws {
        try await httpDelete(path: "api/memory/\(key)")
    }

    // MARK: - HTTP API — Cron

    func fetchCronJobs() async throws -> [CronJob] {
        let (data, _) = try await httpGet(path: "api/cron")
        if let response = try? JSONDecoder().decode(CronJobListResponse.self, from: data) {
            return response.items
        }
        return (try? JSONDecoder().decode([CronJob].self, from: data)) ?? []
    }

    func createCronJob(name: String, schedule: String, command: String) async throws {
        let body: [String: Any] = ["name": name, "schedule": schedule, "command": command]
        let _: EmptyResponse = try await httpPost(path: "api/cron", body: body)
    }

    func deleteCronJob(id: String) async throws {
        try await httpDelete(path: "api/cron/\(id)")
    }

    // MARK: - HTTP API — Tools

    func fetchTools() async throws -> [ToolInfo] {
        let (data, _) = try await httpGet(path: "api/tools")

        // Gateway may return tools as array of objects or wrapped
        if let tools = try? JSONDecoder().decode([ToolInfo].self, from: data) {
            return tools
        }

        // Try unwrapping from a "tools" key
        if let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
           let toolsArray = json["tools"] as? [[String: Any]] {
            return toolsArray.compactMap { dict -> ToolInfo? in
                guard let name = dict["name"] as? String else { return nil }
                return ToolInfo(name: name, description: dict["description"] as? String, category: dict["category"] as? String)
            }
        }
        return []
    }

    // MARK: - HTTP API — Cost

    func fetchCost() async throws -> CostSummary {
        let (data, _) = try await httpGet(path: "api/cost")
        return try JSONDecoder().decode(CostSummary.self, from: data)
    }

    // MARK: - HTTP API — Paired Devices

    func fetchDevices() async throws -> [PairedDevice] {
        let (data, _) = try await httpGet(path: "api/pairing/devices")
        if let response = try? JSONDecoder().decode(DeviceListResponse.self, from: data) {
            return response.items
        }
        return (try? JSONDecoder().decode([PairedDevice].self, from: data)) ?? []
    }

    func revokeDevice(id: String) async throws {
        try await httpDelete(path: "api/pairing/devices/\(id)")
    }

    // MARK: - HTTP API — Integrations

    func fetchIntegrations() async throws -> [Integration] {
        let (data, _) = try await httpGet(path: "api/integrations")
        if let response = try? JSONDecoder().decode(IntegrationListResponse.self, from: data) {
            return response.items
        }
        return (try? JSONDecoder().decode([Integration].self, from: data)) ?? []
    }

    // MARK: - SSE Event Stream

    func connectEventStream() -> AsyncStream<GatewayEvent> {
        AsyncStream { continuation in
            eventStreamTask = Task { [weak self] in
                guard let self,
                      let url = self.baseURL?.appendingPathComponent("api/events"),
                      let token = self.bearerToken else {
                    continuation.finish()
                    return
                }

                var request = URLRequest(url: url)
                request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
                request.setValue("text/event-stream", forHTTPHeaderField: "Accept")

                do {
                    let (bytes, _) = try await self.session.bytes(for: request)
                    for try await line in bytes.lines {
                        if Task.isCancelled { break }
                        guard line.hasPrefix("data: ") else { continue }
                        let json = String(line.dropFirst(6))
                        guard let data = json.data(using: .utf8) else { continue }

                        if let event = try? JSONDecoder().decode(GatewayEvent.self, from: data) {
                            continuation.yield(event)
                        }
                    }
                } catch {
                    // Stream ended or network error
                }

                continuation.finish()
            }
        }
    }

    func disconnectEventStream() {
        eventStreamTask?.cancel()
        eventStreamTask = nil
    }

    // MARK: - Private — WebSocket

    private func receiveLoop(
        task: URLSessionWebSocketTask,
        onEvent: @escaping (WebSocketEvent) -> Void
    ) async {
        var didConnect = false

        while task.state == .running {
            do {
                let message = try await task.receive()

                if !didConnect {
                    didConnect = true
                    await MainActor.run {
                        self.isConnected = true
                        onEvent(.connected)
                    }
                }

                switch message {
                case .string(let text):
                    guard let data = text.data(using: .utf8) else { continue }
                    await dispatchWsMessage(data: data, onEvent: onEvent)
                case .data(let data):
                    await dispatchWsMessage(data: data, onEvent: onEvent)
                @unknown default:
                    break
                }
            } catch {
                break
            }
        }

        await MainActor.run {
            self.isConnected = false
            onEvent(.disconnected)
        }
    }

    private func dispatchWsMessage(
        data: Data,
        onEvent: @escaping (WebSocketEvent) -> Void
    ) async {
        guard let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let type = json["type"] as? String else { return }

        await MainActor.run {
            switch type {
            case "history":
                if let msgs = json["messages"] as? [[String: String]] {
                    let history = msgs.compactMap { dict -> HistoryMessage? in
                        guard let role = dict["role"], let content = dict["content"] else { return nil }
                        return HistoryMessage(role: role, content: content)
                    }
                    onEvent(.history(history))
                }
            case "chunk":
                if let content = json["content"] as? String {
                    onEvent(.chunk(content))
                }
            case "tool_call":
                let name = json["name"] as? String ?? "tool"
                let argsData = (json["args"]).flatMap { try? JSONSerialization.data(withJSONObject: $0) }
                let argsStr = argsData.flatMap { String(data: $0, encoding: .utf8) } ?? "{}"
                onEvent(.toolCall(name: name, args: argsStr))
            case "tool_result":
                let name = json["name"] as? String ?? "tool"
                let output = json["output"] as? String ?? ""
                let success = json["success"] as? Bool ?? true
                onEvent(.toolResult(name: name, output: output, success: success))
            case "done":
                if let response = json["full_response"] as? String {
                    onEvent(.done(response))
                }
            case "error":
                let message = json["message"] as? String ?? "Unknown error"
                onEvent(.error(message))
            default:
                break
            }
        }
    }

    // MARK: - Private — HTTP Helpers

    private func httpGet(path: String, authenticated: Bool = true, timeout: TimeInterval = 10) async throws -> (Data, URLResponse) {
        guard let url = baseURL?.appendingPathComponent(path) else {
            throw GatewayError.notConfigured
        }

        var request = URLRequest(url: url)
        request.timeoutInterval = timeout
        if authenticated { addAuth(&request) }

        let (data, response) = try await session.data(for: request)
        if authenticated { try checkResponse(response, data: data) }
        return (data, response)
    }

    private func httpPost<T: Decodable>(path: String, body: [String: Any]) async throws -> T {
        guard let url = baseURL?.appendingPathComponent(path) else {
            throw GatewayError.notConfigured
        }

        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        addAuth(&request)
        request.httpBody = try JSONSerialization.data(withJSONObject: body)

        let (data, response) = try await session.data(for: request)
        try checkResponse(response, data: data)
        return try JSONDecoder().decode(T.self, from: data)
    }

    private func httpDelete(path: String) async throws {
        guard let url = baseURL?.appendingPathComponent(path) else {
            throw GatewayError.notConfigured
        }

        var request = URLRequest(url: url)
        request.httpMethod = "DELETE"
        addAuth(&request)

        let (data, response) = try await session.data(for: request)
        try checkResponse(response, data: data)
    }

    private func addAuth(_ request: inout URLRequest) {
        if let token = bearerToken {
            request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        }
    }

    private func checkResponse(_ response: URLResponse, data: Data) throws {
        let status = (response as? HTTPURLResponse)?.statusCode ?? 0
        guard (200...299).contains(status) else {
            let body = try? JSONDecoder().decode(ErrorResponse.self, from: data)
            switch status {
            case 401: throw GatewayError.unauthorized
            case 429: throw GatewayError.rateLimited(body?.error ?? "Rate limited")
            default: throw GatewayError.httpError(status: status, message: body?.error ?? "Request failed")
            }
        }
    }
}

// MARK: - Protocol Types

struct WsSendMessage: Encodable {
    let type: String
    let content: String
}

struct HistoryMessage {
    let role: String
    let content: String
}

struct PairResponse: Decodable {
    let paired: Bool
    let token: String
    let message: String?
}

struct ChatResponse: Decodable {
    let reply: String
    let model: String?
    let sessionId: String?

    enum CodingKeys: String, CodingKey {
        case reply, model
        case sessionId = "session_id"
    }
}

struct GatewayConfigResponse: Decodable {
    let format: String
    let content: String
}

struct GatewayStatus: Decodable {
    let provider: String?
    let model: String?
    let temperature: Double?
    let paired: Bool?
    let status: String?
}

struct GatewayEvent: Decodable {
    let type: String
    let provider: String?
    let model: String?
    let component: String?
    let message: String?
    let tool: String?
    let durationMs: Int?
    let timestamp: String?

    enum CodingKeys: String, CodingKey {
        case type, provider, model, component, message, tool, timestamp
        case durationMs = "duration_ms"
    }
}

struct ErrorResponse: Decodable {
    let error: String
}

struct EmptyResponse: Decodable {}

// MARK: - Errors

enum GatewayError: LocalizedError {
    case notConfigured
    case notConnected
    case encodingError
    case unauthorized
    case pairingFailed(String)
    case rateLimited(String)
    case httpError(status: Int, message: String)

    var errorDescription: String? {
        switch self {
        case .notConfigured: "Gateway not configured"
        case .notConnected: "WebSocket not connected"
        case .encodingError: "Failed to encode message"
        case .unauthorized: "Unauthorized — check your pairing token"
        case .pairingFailed(let msg): "Pairing failed: \(msg)"
        case .rateLimited(let msg): msg
        case .httpError(_, let msg): msg
        }
    }
}

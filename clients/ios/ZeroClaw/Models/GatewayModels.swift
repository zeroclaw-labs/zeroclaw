import Foundation

// MARK: - WebSocket Events

enum WebSocketEvent {
    case connected
    case history([HistoryMessage])
    case chunk(String)
    case toolCall(name: String, args: String)
    case toolResult(name: String, output: String, success: Bool)
    case done(String)
    case error(String)
    case disconnected
}

// MARK: - Memory

struct MemoryEntry: Identifiable, Decodable {
    let key: String
    let content: String
    let category: String?
    let createdAt: String?

    var id: String { key }

    enum CodingKeys: String, CodingKey {
        case key, content, category
        case createdAt = "created_at"
    }
}

struct MemoryListResponse: Decodable {
    let entries: [MemoryEntry]?
    let results: [MemoryEntry]?

    var items: [MemoryEntry] { entries ?? results ?? [] }
}

// MARK: - Cron Jobs

struct CronJob: Identifiable, Decodable {
    let id: String
    let name: String
    let schedule: String
    let command: String
    let enabled: Bool?
    let lastRun: String?
    let nextRun: String?

    enum CodingKeys: String, CodingKey {
        case id, name, schedule, command, enabled
        case lastRun = "last_run"
        case nextRun = "next_run"
    }
}

struct CronJobListResponse: Decodable {
    let jobs: [CronJob]?

    var items: [CronJob] { jobs ?? [] }
}

// MARK: - Tools

struct ToolInfo: Identifiable, Decodable {
    let name: String
    let description: String?
    let category: String?

    var id: String { name }
}

// MARK: - Cost

struct CostSummary: Decodable {
    let totalCost: Double?
    let totalTokens: Int?
    let totalRequests: Int?
    let breakdown: [CostBreakdown]?

    enum CodingKeys: String, CodingKey {
        case totalCost = "total_cost"
        case totalTokens = "total_tokens"
        case totalRequests = "total_requests"
        case breakdown
    }
}

struct CostBreakdown: Identifiable, Decodable {
    let provider: String?
    let model: String?
    let cost: Double?
    let tokens: Int?
    let requests: Int?

    var id: String { "\(provider ?? "")-\(model ?? "")" }
}

// MARK: - Paired Devices

struct PairedDevice: Identifiable, Decodable {
    let id: String
    let name: String?
    let lastSeen: String?
    let paired: Bool?

    enum CodingKeys: String, CodingKey {
        case id, name, paired
        case lastSeen = "last_seen"
    }
}

struct DeviceListResponse: Decodable {
    let devices: [PairedDevice]?

    var items: [PairedDevice] { devices ?? [] }
}

// MARK: - Integrations

struct Integration: Identifiable, Decodable {
    let name: String
    let type: String?
    let status: String?
    let enabled: Bool?

    var id: String { name }

    var isHealthy: Bool {
        status == "ok" || status == "connected" || status == "running"
    }
}

struct IntegrationListResponse: Decodable {
    let integrations: [Integration]?

    var items: [Integration] { integrations ?? [] }
}

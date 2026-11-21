import AppKit
import Foundation
import UserNotifications

typealias Object = [String: Any]

final class Reception: NSObject, UNUserNotificationCenterDelegate {
    func userNotificationCenter(_ center: UNUserNotificationCenter, willPresent notification: UNNotification,
                                withCompletionHandler complete: @escaping (UNNotificationPresentationOptions) -> Void) {
        complete([.banner, .list, .sound])
    }

    func userNotificationCenter(_ center: UNUserNotificationCenter, didReceive response: UNNotificationResponse,
                                withCompletionHandler complete: @escaping () -> Void) {
        defer { complete() }
        let data = response.notification.request.content.userInfo
        guard response.actionIdentifier == UNNotificationDefaultActionIdentifier,
              let origin = data["dashboard"] as? String, let alias = data["agent"] as? String,
              let destination = try? Destination(origin, alias),
              let url = try? destination.link(data["session"] as? String) else { return }
        NSWorkspace.shared.open(url)
    }
}

let fields: Object = [
    "title": ["type": "string", "maxLength": 120],
    "body": ["type": "string", "maxLength": 1000],
    "priority": ["type": "string", "enum": ["low", "normal", "high"], "default": "normal"],
    "session_id": ["type": "string", "description": "Optional dashboard conversation identifier"],
    "event_id": ["type": "string", "description": "Stable event identifier for repeat delivery"]
]

func notify(_ args: Object, destination: Destination) async throws -> String {
    guard let title = args["title"] as? String, !title.isEmpty, title.count <= 120,
          let body = args["body"] as? String, !body.isEmpty, body.count <= 1000,
          Set(args.keys).isSubset(of: Set(fields.keys)) else { throw Failure("Invalid title, body, or field.") }
    let priority = args["priority"] as? String ?? "normal"
    guard ["low", "normal", "high"].contains(priority),
          args["priority"] == nil || args["priority"] is String,
          args["session_id"] == nil || args["session_id"] is String,
          args["event_id"] == nil || args["event_id"] is String else { throw Failure("Invalid notification arguments.") }
    let session = args["session_id"] as? String
    _ = try destination.link(session)
    let event = args["event_id"] as? String ?? UUID().uuidString
    guard identifier(event) else { throw Failure("Invalid event identifier.") }
    let center = UNUserNotificationCenter.current()
    let settings = await center.notificationSettings()
    guard [.authorized, .provisional].contains(settings.authorizationStatus) else {
        throw Failure("Notifications require permission. Open ZeroClaw Notifications and enable alerts in System Settings.")
    }
    let requestID = [destination.origin.absoluteString, destination.agent, event].joined(separator: "|")
    let content = UNMutableNotificationContent()
    content.title = title
    content.body = body
    content.interruptionLevel = priority == "low" ? .passive : .active
    if priority == "high" { content.sound = .default }
    var data = ["dashboard": destination.origin.absoluteString, "agent": destination.agent]
    data["session"] = session
    content.userInfo = data
    let request = UNNotificationRequest(identifier: requestID, content: content, trigger: nil)
    if args["event_id"] != nil {
        let support = try FileManager.default.url(for: .applicationSupportDirectory, in: .userDomainMask,
                                                  appropriateFor: nil, create: true)
        let dir = support.appendingPathComponent("ZeroClaw Notifications", isDirectory: true)
        return try await deliverOnce(requestID, directory: dir) {
            if await center.deliveredNotifications().contains(where: { $0.request.identifier == requestID }) { return }
            try await center.add(request)
        }
    }
    try await center.add(request)
    return "Accepted by Notification Center."
}

func handle(_ input: Object, destination: Destination) async -> Object? {
    guard let id = input["id"], let method = input["method"] as? String else { return nil }
    let params = input["params"] as? Object ?? [:]
    var response: Object = ["id": id, "jsonrpc": "2.0"]
    switch method {
    case "initialize":
        let supported = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"]
        let version = params["protocolVersion"] as? String ?? ""
        response["result"] = ["protocolVersion": supported.contains(version) ? version : supported[0],
                              "serverInfo": ["name": "zeroclaw-macos-notifications", "version": "1.0"],
                              "capabilities": ["tools": [:] as Object]]
    case "ping":
        response["result"] = [:] as Object
    case "tools/list":
        response["result"] = ["tools": [["name": "system_notify", "description": "Notify the operator on this Mac.",
            "inputSchema": ["type": "object", "properties": fields, "required": ["title", "body"], "additionalProperties": false]]]]
    case "tools/call":
        guard params["name"] as? String == "system_notify", let args = params["arguments"] as? Object else {
            response["error"] = ["code": -32602, "message": "Unknown tool or invalid arguments."]
            return response
        }
        do {
            let message = try await notify(args, destination: destination)
            response["result"] = ["content": [["type": "text", "text": message]]]
        } catch {
            response["result"] = ["isError": true, "content": [["type": "text", "text": error.localizedDescription]]]
        }
    default:
        response["error"] = ["code": -32601, "message": "Unknown method."]
    }
    return response
}

func serve(_ destination: Destination) async throws {
    while let line = readLine() {
        let response: Object?
        do {
            let frame = try JSONSerialization.jsonObject(with: Data(line.utf8))
            if let input = frame as? Object, input["jsonrpc"] as? String == "2.0", input["method"] is String {
                response = await handle(input, destination: destination)
            } else {
                response = ["jsonrpc": "2.0", "id": NSNull(), "error": ["code": -32600, "message": "Invalid request."]]
            }
        } catch {
            response = ["jsonrpc": "2.0", "id": NSNull(), "error": ["code": -32700, "message": "Parse error."]]
        }
        if let response {
            var data = try JSONSerialization.data(withJSONObject: response)
            data.append(10)
            try FileHandle.standardOutput.write(contentsOf: data)
        }
    }
}

let application = NSApplication.shared
application.setActivationPolicy(.accessory)
let reception = Reception()
let center = UNUserNotificationCenter.current()
center.delegate = reception
let args = Array(CommandLine.arguments.dropFirst())
if args.first == "serve" {
    Task.detached {
        do {
            guard args.count == 3 else { throw Failure("Usage: serve DASHBOARD_ORIGIN AGENT_ALIAS") }
            try await serve(Destination(args[1], args[2]))
            exit(0)
        } catch {
            FileHandle.standardError.write(Data((error.localizedDescription + "\n").utf8))
            exit(1)
        }
    }
} else if args == ["status"] {
    center.getNotificationSettings { settings in
        print(settings.authorizationStatus.rawValue)
        exit(0)
    }
} else {
    Task {
        do { _ = try await center.requestAuthorization(options: [.alert, .sound]) }
        catch { NSLog("ZeroClaw notification authorization failed: %@", error.localizedDescription) }
    }
}
application.run()

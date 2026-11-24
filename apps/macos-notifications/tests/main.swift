import Foundation

let root = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
defer { try? FileManager.default.removeItem(at: root) }
let target = try Destination("http://127.0.0.1:42617", "operator")
let route = try target.link("conversation_2")
precondition(route.absoluteString == "http://127.0.0.1:42617/agent/operator?notify=1&session=conversation_2")
let defaultRoute = try target.link(nil)
precondition(defaultRoute.path == "/agent/operator" && defaultRoute.query == "notify=1")
let trailingSlash = try Destination("http://127.0.0.1:42617/", "operator")
precondition(trailingSlash.origin == target.origin)
let normalized = try Destination("http://LOCALHOST:80/", "operator")
precondition(normalized.origin.absoluteString == "http://localhost")
for origin in ["https://example.com", "file:///tmp", "http://u@localhost", "http://localhost/path", "http://localhost/?x"] {
    precondition((try? Destination(origin, "operator")) == nil)
}
precondition((try? Destination("http://localhost", "../other")) == nil)
precondition((try? target.link("a/b")) == nil)

var calls = 0
let accepted = try await deliverOnce("event", directory: root) { calls += 1 }
let duplicate = try await deliverOnce("event", directory: root) { calls += 1 }
precondition(calls == 1 && accepted == "Accepted by Notification Center." && duplicate == "Already delivered.")
do {
    _ = try await deliverOnce("retry", directory: root) { throw Failure("Delivery unavailable.") }
    fatalError("Expected delivery to fail")
} catch { }
_ = try await deliverOnce("retry", directory: root) { calls += 1 }
precondition(calls == 2)
print("Passed dashboard routes, origin and identifier validation, duplicate receipts, and failed-delivery retry.")

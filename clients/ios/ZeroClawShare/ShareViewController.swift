import UIKit
import Social
import UniformTypeIdentifiers

/// Share Extension that allows sharing text, URLs, and images to ZeroClaw.
/// Writes shared content to App Groups UserDefaults as a pending message queue.
/// The main app picks up pending messages on next foreground.
class ShareViewController: SLComposeServiceViewController {

    private let appGroupID = "group.ai.zeroclaw"

    override func isContentValid() -> Bool {
        !contentText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    override func didSelectPost() {
        var parts: [String] = []

        if let text = contentText, !text.isEmpty {
            parts.append(text)
        }

        let group = DispatchGroup()

        // Extract attachments
        if let items = extensionContext?.inputItems as? [NSExtensionItem] {
            for item in items {
                guard let providers = item.attachments else { continue }
                for provider in providers {
                    if provider.hasItemConformingToTypeIdentifier(UTType.url.identifier) {
                        group.enter()
                        provider.loadItem(forTypeIdentifier: UTType.url.identifier) { item, _ in
                            if let url = item as? URL {
                                parts.append(url.absoluteString)
                            }
                            group.leave()
                        }
                    } else if provider.hasItemConformingToTypeIdentifier(UTType.plainText.identifier) {
                        group.enter()
                        provider.loadItem(forTypeIdentifier: UTType.plainText.identifier) { item, _ in
                            if let text = item as? String, !text.isEmpty {
                                parts.append(text)
                            }
                            group.leave()
                        }
                    } else if provider.hasItemConformingToTypeIdentifier(UTType.image.identifier) {
                        group.enter()
                        provider.loadItem(forTypeIdentifier: UTType.image.identifier) { item, _ in
                            if let url = item as? URL {
                                parts.append("[Image: \(url.lastPathComponent)]")
                            }
                            group.leave()
                        }
                    }
                }
            }
        }

        group.notify(queue: .main) { [weak self] in
            self?.enqueueMessage(parts.joined(separator: "\n"))
            self?.extensionContext?.completeRequest(returningItems: [], completionHandler: nil)
        }
    }

    override func configurationItems() -> [Any]! {
        []
    }

    // MARK: - Private

    private func enqueueMessage(_ message: String) {
        guard !message.isEmpty,
              let defaults = UserDefaults(suiteName: appGroupID) else { return }

        var pending = defaults.stringArray(forKey: "shared_pending_messages") ?? []
        pending.append(message)
        defaults.set(pending, forKey: "shared_pending_messages")
    }
}

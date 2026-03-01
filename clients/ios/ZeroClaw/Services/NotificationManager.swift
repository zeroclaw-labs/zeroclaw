import Foundation
import UserNotifications
import UIKit

/// Manages local notifications for ZeroClaw.
/// Sends notifications for agent messages when the app is backgrounded,
/// and for connection status changes.
final class NotificationManager: NSObject, UNUserNotificationCenterDelegate {
    static let shared = NotificationManager()

    private override init() {
        super.init()
    }

    // MARK: - Categories

    static let messageCategory = "ZEROCLAW_MESSAGE"
    static let statusCategory = "ZEROCLAW_STATUS"
    static let errorCategory = "ZEROCLAW_ERROR"

    static let replyAction = "REPLY_ACTION"

    // MARK: - Setup

    func setup() {
        let center = UNUserNotificationCenter.current()
        center.delegate = self

        // Define actions
        let replyAction = UNTextInputNotificationAction(
            identifier: Self.replyAction,
            title: "Reply",
            textInputButtonTitle: "Send",
            textInputPlaceholder: "Message..."
        )

        let openAction = UNNotificationAction(
            identifier: "OPEN_ACTION",
            title: "Open",
            options: .foreground
        )

        // Define categories
        let messageCategory = UNNotificationCategory(
            identifier: Self.messageCategory,
            actions: [replyAction, openAction],
            intentIdentifiers: []
        )

        let statusCategory = UNNotificationCategory(
            identifier: Self.statusCategory,
            actions: [openAction],
            intentIdentifiers: []
        )

        let errorCategory = UNNotificationCategory(
            identifier: Self.errorCategory,
            actions: [openAction],
            intentIdentifiers: []
        )

        center.setNotificationCategories([messageCategory, statusCategory, errorCategory])
    }

    /// Request notification permission. Call on first launch.
    func requestPermission() {
        UNUserNotificationCenter.current().requestAuthorization(
            options: [.alert, .badge, .sound]
        ) { _, _ in }
    }

    // MARK: - Post Notifications

    /// Post a notification for an incoming agent message (only when backgrounded).
    func postMessageNotification(content: String) {
        guard isAppBackgrounded() else { return }

        let notification = UNMutableNotificationContent()
        notification.title = "ZeroClaw"
        notification.body = String(content.prefix(200))
        notification.categoryIdentifier = Self.messageCategory
        notification.sound = .default

        let request = UNNotificationRequest(
            identifier: "msg-\(UUID().uuidString)",
            content: notification,
            trigger: nil
        )

        UNUserNotificationCenter.current().add(request)
    }

    /// Post a notification for connection status changes.
    func postStatusNotification(title: String, body: String) {
        guard isAppBackgrounded() else { return }

        let notification = UNMutableNotificationContent()
        notification.title = title
        notification.body = body
        notification.categoryIdentifier = Self.statusCategory
        notification.sound = .default

        let request = UNNotificationRequest(
            identifier: "status-\(UUID().uuidString)",
            content: notification,
            trigger: nil
        )

        UNUserNotificationCenter.current().add(request)
    }

    /// Post a notification for errors.
    func postErrorNotification(message: String) {
        guard isAppBackgrounded() else { return }

        let notification = UNMutableNotificationContent()
        notification.title = "ZeroClaw Error"
        notification.body = message
        notification.categoryIdentifier = Self.errorCategory
        notification.sound = .default

        let request = UNNotificationRequest(
            identifier: "error-\(UUID().uuidString)",
            content: notification,
            trigger: nil
        )

        UNUserNotificationCenter.current().add(request)
    }

    // MARK: - UNUserNotificationCenterDelegate

    /// Handle notifications when app is in foreground — suppress them.
    func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        willPresent notification: UNNotification
    ) async -> UNNotificationPresentationOptions {
        []
    }

    /// Handle notification actions (reply, open).
    func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        didReceive response: UNNotificationResponse
    ) async {
        if response.actionIdentifier == Self.replyAction,
           let textResponse = response as? UNTextInputNotificationResponse {
            // Post reply to the agent via notification center
            NotificationCenter.default.post(
                name: .zeroClawNotificationReply,
                object: nil,
                userInfo: ["message": textResponse.userText]
            )
        }
    }

    // MARK: - Private

    private func isAppBackgrounded() -> Bool {
        guard Thread.isMainThread else { return true }
        return UIApplication.shared.applicationState != .active
    }
}

extension Notification.Name {
    static let zeroClawNotificationReply = Notification.Name("zeroClawNotificationReply")
}

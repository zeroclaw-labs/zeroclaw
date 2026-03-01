import Foundation
import BackgroundTasks

/// Manages background task scheduling for periodic gateway health checks.
/// Uses BGTaskScheduler with a 15-minute minimum interval.
final class BackgroundTaskManager {
    static let shared = BackgroundTaskManager()

    static let healthCheckIdentifier = "ai.zeroclaw.healthcheck"

    private init() {}

    /// Register background task handlers. Call from app init.
    func registerTasks() {
        BGTaskScheduler.shared.register(
            forTaskWithIdentifier: Self.healthCheckIdentifier,
            using: nil
        ) { task in
            guard let refreshTask = task as? BGAppRefreshTask else {
                task.setTaskCompleted(success: false)
                return
            }
            self.handleHealthCheck(task: refreshTask)
        }
    }

    /// Schedule the next health check. Call after each check completes.
    func scheduleHealthCheck() {
        let request = BGAppRefreshTaskRequest(identifier: Self.healthCheckIdentifier)
        request.earliestBeginDate = Date(timeIntervalSinceNow: 15 * 60) // 15 minutes

        do {
            try BGTaskScheduler.shared.submit(request)
        } catch {
            // Scheduling can fail if the user has disabled background refresh
        }
    }

    // MARK: - Private

    private func handleHealthCheck(task: BGAppRefreshTask) {
        // Schedule the next check before doing work
        scheduleHealthCheck()

        let checkTask = Task {
            await performHealthCheck()
        }

        task.expirationHandler = {
            checkTask.cancel()
        }

        Task {
            await checkTask.value
            task.setTaskCompleted(success: true)
        }
    }

    private func performHealthCheck() async {
        let defaults = UserDefaults.standard
        let host = defaults.string(forKey: "zeroclaw_gateway_host") ?? "127.0.0.1"
        let port = defaults.object(forKey: "zeroclaw_gateway_port") as? Int ?? 42617
        let token = KeychainHelper.read(account: KeychainHelper.gatewayTokenAccount)

        let client = await MainActor.run {
            let c = GatewayClient()
            c.configure(host: host, port: port, token: token)
            return c
        }

        do {
            let healthy = try await client.healthCheck()
            if !healthy {
                NotificationManager.shared.postStatusNotification(
                    title: "ZeroClaw Gateway",
                    body: "Gateway is unreachable"
                )
            }
        } catch {
            NotificationManager.shared.postStatusNotification(
                title: "ZeroClaw Gateway",
                body: "Health check failed: \(error.localizedDescription)"
            )
        }
    }
}

import Cocoa
import WebKit

// MARK: - Gateway Process Manager

class GatewayManager {
    private var process: Process?
    private(set) var port: UInt16 = 0
    private(set) var pairingCode: String = ""

    func start(binary: String, completion: @escaping (Bool) -> Void) {
        let proc = Process()
        proc.executableURL = URL(fileURLWithPath: binary)
        proc.arguments = ["gateway", "-p", "0"]

        let pipe = Pipe()
        proc.standardOutput = pipe
        proc.standardError = pipe

        let handle = pipe.fileHandleForReading
        var buffer = ""
        var resolved = false

        handle.readabilityHandler = { [weak self] fh in
            guard let self = self else { return }
            let data = fh.availableData
            guard !data.isEmpty, let chunk = String(data: data, encoding: .utf8) else { return }
            buffer += chunk

            if !resolved {
                if let range = buffer.range(of: "listening on http://127.0.0.1:") {
                    let after = buffer[range.upperBound...]
                    if let end = after.firstIndex(where: { !$0.isNumber }) {
                        let portStr = String(after[after.startIndex..<end])
                        if let p = UInt16(portStr) {
                            self.port = p
                        }
                    }
                }
                if let range = buffer.range(of: "│  ") {
                    let after = buffer[range.upperBound...]
                    if let end = after.range(of: "  │") {
                        let code = String(after[after.startIndex..<end.lowerBound]).trimmingCharacters(in: .whitespaces)
                        if code.count == 6, code.allSatisfy({ $0.isNumber }) {
                            self.pairingCode = code
                        }
                    }
                }
                if self.port > 0 {
                    resolved = true
                    DispatchQueue.main.async { completion(true) }
                }
            }
        }

        do {
            try proc.run()
            self.process = proc
            DispatchQueue.main.asyncAfter(deadline: .now() + 15) {
                if !resolved {
                    resolved = true
                    completion(false)
                }
            }
        } catch {
            print("Failed to launch gateway: \(error)")
            completion(false)
        }
    }

    func stop() {
        process?.terminate()
        process = nil
    }
}

// MARK: - App Delegate

class AppDelegate: NSObject, NSApplicationDelegate {
    var window: NSWindow!
    var webView: WKWebView!
    let gateway = GatewayManager()

    func applicationDidFinishLaunching(_ notification: Notification) {
        let binaryPath = findBinary()
        guard let binary = binaryPath else {
            showError("zeroclaw binary not found.\nRun: cargo build --release")
            return
        }

        setupWindow()
        showLoading()

        gateway.start(binary: binary) { [weak self] success in
            guard let self = self else { return }
            if success {
                let url = URL(string: "http://127.0.0.1:\(self.gateway.port)")!
                self.webView.load(URLRequest(url: url))
                self.window.title = "MoA — 127.0.0.1:\(self.gateway.port)"
            } else {
                self.showError("Gateway failed to start.")
            }
        }
    }

    func applicationWillTerminate(_ notification: Notification) {
        gateway.stop()
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
        return true
    }

    private func findBinary() -> String? {
        let bundleMacOS = Bundle.main.bundlePath + "/Contents/MacOS/zeroclaw-engine"
        let candidates = [
            bundleMacOS,
            ProcessInfo.processInfo.environment["MOA_BINARY"],
            FileManager.default.currentDirectoryPath + "/target/release/zeroclaw",
            NSHomeDirectory() + "/Documents/MoA_new/target/release/zeroclaw",
        ].compactMap { $0 }

        for path in candidates {
            if FileManager.default.isExecutableFile(atPath: path) {
                return path
            }
        }
        return nil
    }

    private func setupWindow() {
        let config = WKWebViewConfiguration()
        config.preferences.setValue(true, forKey: "developerExtrasEnabled")

        webView = WKWebView(frame: .zero, configuration: config)
        webView.customUserAgent = "MoA-Desktop/1.0"

        let screenRect = NSScreen.main?.visibleFrame ?? NSRect(x: 0, y: 0, width: 1200, height: 800)
        let width: CGFloat = min(1280, screenRect.width * 0.85)
        let height: CGFloat = min(900, screenRect.height * 0.85)
        let x = screenRect.midX - width / 2
        let y = screenRect.midY - height / 2

        window = NSWindow(
            contentRect: NSRect(x: x, y: y, width: width, height: height),
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered,
            defer: false
        )
        window.title = "MoA - Mixture of Agents"
        window.minSize = NSSize(width: 800, height: 600)
        window.contentView = webView
        window.makeKeyAndOrderFront(nil)
        window.center()

        window.titlebarAppearsTransparent = true
        window.backgroundColor = NSColor(red: 0.07, green: 0.07, blue: 0.10, alpha: 1.0)
    }

    private func showLoading() {
        let html = """
        <!DOCTYPE html>
        <html>
        <head><style>
            body {
                margin: 0; display: flex; align-items: center; justify-content: center;
                height: 100vh; background: #111118; color: #e0e0e0;
                font-family: -apple-system, BlinkMacSystemFont, sans-serif;
            }
            .container { text-align: center; }
            .spinner {
                width: 40px; height: 40px; margin: 0 auto 20px;
                border: 3px solid #333; border-top-color: #4f8cff;
                border-radius: 50%; animation: spin 0.8s linear infinite;
            }
            @keyframes spin { to { transform: rotate(360deg); } }
            h2 { font-weight: 500; font-size: 18px; margin: 0 0 8px; }
            p { font-size: 13px; color: #888; margin: 0; }
        </style></head>
        <body>
            <div class="container">
                <div class="spinner"></div>
                <h2>MoA Starting...</h2>
                <p>Preparing gateway server</p>
            </div>
        </body>
        </html>
        """
        webView.loadHTMLString(html, baseURL: nil)
    }

    private func showError(_ message: String) {
        let alert = NSAlert()
        alert.messageText = "MoA Error"
        alert.informativeText = message
        alert.alertStyle = .critical
        alert.addButton(withTitle: "Quit")
        alert.runModal()
        NSApp.terminate(nil)
    }
}

// MARK: - Entry Point

let app = NSApplication.shared
let delegate = AppDelegate()
app.delegate = delegate
app.setActivationPolicy(.regular)
app.activate(ignoringOtherApps: true)
app.run()

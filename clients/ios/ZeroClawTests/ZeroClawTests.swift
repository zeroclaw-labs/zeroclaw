import XCTest
@testable import ZeroClaw

final class ZeroClawTests: XCTestCase {

    // MARK: - ChatMessage

    func testChatMessageUserRole() {
        let message = ChatMessage(
            id: "test_1",
            content: "Hello",
            role: "user",
            timestampMs: 0
        )
        XCTAssertTrue(message.isUser)
    }

    func testChatMessageAssistantRole() {
        let message = ChatMessage(
            id: "test_2",
            content: "Response",
            role: "assistant",
            timestampMs: 0
        )
        XCTAssertFalse(message.isUser)
    }

    // MARK: - AgentStatus

    func testAgentStatusDisplayText() {
        XCTAssertEqual(AgentStatus.stopped.displayText, "Stopped")
        XCTAssertEqual(AgentStatus.running.displayText, "Running")
        XCTAssertEqual(AgentStatus.starting.displayText, "Starting...")
        XCTAssertEqual(AgentStatus.thinking.displayText, "Thinking...")
    }

    func testAgentStatusIsActive() {
        XCTAssertFalse(AgentStatus.stopped.isActive)
        XCTAssertTrue(AgentStatus.running.isActive)
        XCTAssertTrue(AgentStatus.thinking.isActive)
        XCTAssertFalse(AgentStatus.error(message: "test").isActive)
    }

    // MARK: - SettingsManager

    func testSettingsManagerDefaults() {
        let settings = SettingsManager()
        XCTAssertEqual(settings.provider, "anthropic")
        XCTAssertEqual(settings.model, "claude-sonnet-4-5")
        XCTAssertFalse(settings.autoStart)
    }

    func testAvailableModelsForProvider() {
        let settings = SettingsManager()
        let anthropicModels = settings.availableModels(for: "anthropic")
        XCTAssertTrue(anthropicModels.contains("claude-sonnet-4-5"))

        let openaiModels = settings.availableModels(for: "openai")
        XCTAssertTrue(openaiModels.contains("gpt-4o"))

        let unknownModels = settings.availableModels(for: "unknown_provider")
        XCTAssertTrue(unknownModels.isEmpty)
    }
}

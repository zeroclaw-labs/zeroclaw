import SwiftUI

/// Main chat interface.
/// Messages are sent to the local ZeroClaw gateway running in-process.
struct ChatView: View {
    @EnvironmentObject var agent: AgentManager
    @State private var inputText = ""
    @FocusState private var isInputFocused: Bool

    var body: some View {
        NavigationStack {
            VStack(spacing: 0) {
                // Messages
                ScrollViewReader { scrollProxy in
                    ScrollView {
                        LazyVStack(spacing: 12) {
                            if agent.messages.isEmpty {
                                emptyState
                            } else {
                                ForEach(agent.messages) { message in
                                    MessageBubble(message: message)
                                        .id(message.id)
                                }

                                if agent.isThinking {
                                    ThinkingBubble()
                                        .id("thinking")
                                }
                            }
                        }
                        .padding()
                    }
                    .onChange(of: agent.messages.count) {
                        withAnimation {
                            if let last = agent.messages.last {
                                scrollProxy.scrollTo(last.id, anchor: .bottom)
                            }
                        }
                    }
                    .onChange(of: agent.isThinking) {
                        if agent.isThinking {
                            withAnimation {
                                scrollProxy.scrollTo("thinking", anchor: .bottom)
                            }
                        }
                    }
                }

                Divider()

                // Input bar
                inputBar
            }
            .navigationTitle("MoA")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarLeading) {
                    statusBadge
                }
                ToolbarItem(placement: .topBarTrailing) {
                    Button {
                        agent.clearMessages()
                    } label: {
                        Image(systemName: "trash")
                    }
                    .disabled(agent.messages.isEmpty)
                }
            }
        }
    }

    // MARK: - Empty State

    private var emptyState: some View {
        VStack(spacing: 16) {
            Spacer()
                .frame(height: 80)

            Image(systemName: "brain.head.profile")
                .font(.system(size: 56))
                .foregroundStyle(
                    LinearGradient(
                        colors: [.blue, .purple],
                        startPoint: .topLeading,
                        endPoint: .bottomTrailing
                    )
                )

            Text("MoA")
                .font(.title)
                .fontWeight(.bold)

            Text("Your local AI assistant")
                .font(.subheadline)
                .foregroundColor(.secondary)

            if agent.status == .stopped {
                Button("Start Agent") {
                    agent.start(
                        provider: UserDefaults.standard.string(forKey: "provider") ?? "openrouter",
                        apiKey: nil
                    )
                }
                .buttonStyle(.borderedProminent)
                .padding(.top, 8)
            } else if agent.status == .starting {
                ProgressView("Starting AI engine...")
                    .padding(.top, 8)
            } else {
                Text("Send a message to start chatting")
                    .font(.subheadline)
                    .foregroundColor(.secondary)
            }
        }
        .frame(maxWidth: .infinity)
    }

    // MARK: - Input Bar

    private var inputBar: some View {
        HStack(spacing: 12) {
            TextField("Message MoA...", text: $inputText, axis: .vertical)
                .textFieldStyle(.roundedBorder)
                .lineLimit(1...4)
                .focused($isInputFocused)
                .disabled(agent.status != .running && agent.status != .stopped)
                .onSubmit { sendMessage() }

            Button(action: sendMessage) {
                Image(systemName: "arrow.up.circle.fill")
                    .font(.title2)
                    .foregroundColor(canSend ? .blue : .gray)
            }
            .disabled(!canSend)
        }
        .padding(.horizontal)
        .padding(.vertical, 10)
        .background(.bar)
    }

    // MARK: - Status

    private var statusBadge: some View {
        HStack(spacing: 6) {
            Circle()
                .fill(statusColor)
                .frame(width: 8, height: 8)

            Text(statusText)
                .font(.caption)
                .foregroundColor(.secondary)
        }
    }

    private var statusColor: Color {
        switch agent.status {
        case .running: return .green
        case .starting: return .orange
        case .thinking: return .blue
        case .stopped: return .gray
        case .error: return .red
        }
    }

    private var statusText: String {
        switch agent.status {
        case .running: return "Running"
        case .starting: return "Starting"
        case .thinking: return "Thinking"
        case .stopped: return "Stopped"
        case .error: return "Error"
        }
    }

    private var canSend: Bool {
        !inputText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        && !agent.isThinking
    }

    // MARK: - Actions

    private func sendMessage() {
        let text = inputText.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty, !agent.isThinking else { return }
        inputText = ""
        agent.send(message: text)
    }
}

// MARK: - Message Bubble

struct MessageBubble: View {
    let message: ChatMessage

    var body: some View {
        HStack {
            if message.role == .user { Spacer(minLength: 60) }

            VStack(alignment: message.role == .user ? .trailing : .leading, spacing: 4) {
                Text(message.content)
                    .padding(12)
                    .background(bubbleColor)
                    .foregroundColor(textColor)
                    .clipShape(RoundedRectangle(cornerRadius: 16))

                Text(message.timestamp, style: .time)
                    .font(.caption2)
                    .foregroundColor(.secondary)
            }

            if message.role != .user { Spacer(minLength: 60) }
        }
    }

    private var bubbleColor: Color {
        switch message.role {
        case .user: return .blue
        case .assistant: return Color(.systemGray5)
        case .error: return .red.opacity(0.2)
        }
    }

    private var textColor: Color {
        switch message.role {
        case .user: return .white
        case .assistant: return .primary
        case .error: return .red
        }
    }
}

// MARK: - Thinking Indicator

struct ThinkingBubble: View {
    @State private var dotIndex = 0
    let timer = Timer.publish(every: 0.4, on: .main, in: .common).autoconnect()

    var body: some View {
        HStack {
            HStack(spacing: 4) {
                ForEach(0..<3, id: \.self) { i in
                    Circle()
                        .fill(Color.secondary)
                        .frame(width: 8, height: 8)
                        .opacity(i == dotIndex ? 1.0 : 0.3)
                }
            }
            .padding(12)
            .background(Color(.systemGray5))
            .clipShape(RoundedRectangle(cornerRadius: 16))

            Spacer()
        }
        .onReceive(timer) { _ in
            dotIndex = (dotIndex + 1) % 3
        }
    }
}

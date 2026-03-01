import SwiftUI

struct ContentView: View {
    @EnvironmentObject var agentService: AgentService
    @EnvironmentObject var settingsManager: SettingsManager
    @State private var showSettings = false
    @State private var showStatusDetail = false
    @State private var showScrollToBottom = false

    var body: some View {
        NavigationStack {
            Group {
                if !settingsManager.isGatewayConfigured {
                    PairingOnboardingView(showSettings: $showSettings)
                } else if agentService.messages.isEmpty {
                    EmptyStateView()
                } else {
                    chatMessageList
                }
            }
            .safeAreaInset(edge: .bottom) {
                if settingsManager.isGatewayConfigured {
                    VStack(spacing: 0) {
                        if agentService.isStreaming {
                            streamingIndicator
                        }
                        ChatInputView()
                    }
                }
            }
            .navigationTitle("ZeroClaw")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarLeading) {
                    Button {
                        showStatusDetail = true
                    } label: {
                        StatusIndicatorView(status: agentService.status)
                    }
                    .glassEffect(.identity)
                }
                ToolbarItem(placement: .topBarTrailing) {
                    Button {
                        showSettings = true
                    } label: {
                        Image(systemName: "gearshape")
                    }
                    .glassEffect(.identity)
                }
            }
            .sheet(isPresented: $showSettings) {
                SettingsView()
            }
            .sheet(isPresented: $showStatusDetail) {
                StatusDetailView()
            }
        }
    }

    private var chatMessageList: some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(spacing: 12) {
                    ForEach(agentService.messages) { message in
                        ChatMessageView(message: message)
                            .id(message.id)
                    }
                }
                .padding(.horizontal)
                .padding(.top, 8)
                .padding(.bottom, 8)
            }
            .overlay(alignment: .bottomTrailing) {
                if showScrollToBottom {
                    scrollToBottomButton(proxy: proxy)
                }
            }
            .onChange(of: agentService.messages.count) {
                scrollToLast(proxy: proxy)
            }
            .onChange(of: agentService.isStreaming) {
                if agentService.isStreaming {
                    scrollToLast(proxy: proxy)
                }
            }
        }
    }

    private func scrollToBottomButton(proxy: ScrollViewProxy) -> some View {
        Button {
            scrollToLast(proxy: proxy)
            UIImpactFeedbackGenerator(style: .light).impactOccurred()
        } label: {
            Image(systemName: "chevron.down.circle.fill")
                .font(.title2)
                .foregroundStyle(.zeroClawOrange)
                .padding(12)
        }
        .transition(.scale.combined(with: .opacity))
    }

    private func scrollToLast(proxy: ScrollViewProxy) {
        guard let lastId = agentService.messages.last?.id else { return }
        withAnimation {
            proxy.scrollTo(lastId, anchor: .bottom)
        }
    }

    private var streamingIndicator: some View {
        HStack(spacing: 6) {
            ProgressView()
                .controlSize(.mini)
            Text("Thinking...")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
        .padding(.vertical, 4)
    }
}

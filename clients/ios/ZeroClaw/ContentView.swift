import SwiftUI

struct ContentView: View {
    @EnvironmentObject var agentService: AgentService
    @EnvironmentObject var settingsManager: SettingsManager
    @State private var showSettings = false

    var body: some View {
        NavigationStack {
            Group {
                if agentService.messages.isEmpty {
                    EmptyStateView()
                } else {
                    chatMessageList
                }
            }
            .safeAreaInset(edge: .bottom) {
                ChatInputView()
            }
            .navigationTitle("ZeroClaw")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarLeading) {
                    StatusIndicatorView(status: agentService.status)
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
            .onChange(of: agentService.messages.count) {
                if let lastMessage = agentService.messages.last {
                    withAnimation {
                        proxy.scrollTo(lastMessage.id, anchor: .bottom)
                    }
                }
            }
        }
    }
}

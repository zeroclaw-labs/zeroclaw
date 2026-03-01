import SwiftUI

struct ChatInputView: View {
    @EnvironmentObject var agentService: AgentService
    @State private var inputText = ""
    @FocusState private var isInputFocused: Bool

    var body: some View {
        HStack(spacing: 10) {
            TextField("Message ZeroClaw...", text: $inputText, axis: .vertical)
                .textInputAutocapitalization(.sentences)
                .autocorrectionDisabled()
                .lineLimit(1...5)
                .focused($isInputFocused)
                .padding(.horizontal, 14)
                .padding(.vertical, 10)

            Button(action: sendMessage) {
                Image(systemName: "arrow.up.circle.fill")
                    .font(.system(size: 30))
                    .foregroundStyle(canSend ? .zeroClawOrange : Color(.systemGray4))
            }
            .disabled(!canSend)
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 8)
        .glassEffect(.regular, in: RoundedRectangle(cornerRadius: 24))
        .padding(.horizontal, 12)
        .padding(.bottom, 4)
    }

    private var canSend: Bool {
        !inputText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            && agentService.status == .running
    }

    private func sendMessage() {
        let text = inputText.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return }
        agentService.sendMessage(text)
        inputText = ""
    }
}

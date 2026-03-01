import SwiftUI

struct CostView: View {
    @EnvironmentObject var agentService: AgentService

    @State private var summary: CostSummary?
    @State private var isLoading = false
    @State private var error: String?

    var body: some View {
        List {
            if let error {
                Section {
                    Label(error, systemImage: "exclamationmark.triangle")
                        .foregroundStyle(.red)
                        .font(.caption)
                }
            }

            if let summary {
                Section("Overview") {
                    LabeledContent("Total Cost") {
                        Text(String(format: "$%.4f", summary.totalCost ?? 0))
                            .foregroundStyle(.zeroClawOrange)
                            .fontWeight(.semibold)
                    }
                    if let tokens = summary.totalTokens {
                        LabeledContent("Total Tokens", value: formatNumber(tokens))
                    }
                    if let requests = summary.totalRequests {
                        LabeledContent("Total Requests", value: formatNumber(requests))
                    }
                }

                if let breakdown = summary.breakdown, !breakdown.isEmpty {
                    Section("By Model") {
                        ForEach(breakdown) { entry in
                            VStack(alignment: .leading, spacing: 2) {
                                HStack {
                                    Text(entry.model ?? entry.provider ?? "Unknown")
                                        .font(.subheadline)
                                    Spacer()
                                    Text(String(format: "$%.4f", entry.cost ?? 0))
                                        .font(.subheadline.monospacedDigit())
                                        .foregroundStyle(.secondary)
                                }
                                HStack {
                                    if let tokens = entry.tokens {
                                        Text("\(formatNumber(tokens)) tokens")
                                    }
                                    if let requests = entry.requests {
                                        Text("\(formatNumber(requests)) requests")
                                    }
                                }
                                .font(.caption)
                                .foregroundStyle(.tertiary)
                            }
                        }
                    }
                }
            } else if !isLoading {
                ContentUnavailableView("No Usage Data", systemImage: "chart.bar", description: Text("Cost tracking data is not available."))
            }
        }
        .navigationTitle("Usage & Cost")
        .navigationBarTitleDisplayMode(.inline)
        .refreshable { await fetchCost() }
        .task { await fetchCost() }
        .overlay { if isLoading && summary == nil { ProgressView() } }
    }

    private func fetchCost() async {
        isLoading = true
        error = nil
        do {
            summary = try await agentService.gateway.fetchCost()
        } catch {
            self.error = error.localizedDescription
        }
        isLoading = false
    }

    private func formatNumber(_ n: Int) -> String {
        let formatter = NumberFormatter()
        formatter.numberStyle = .decimal
        return formatter.string(from: NSNumber(value: n)) ?? "\(n)"
    }
}

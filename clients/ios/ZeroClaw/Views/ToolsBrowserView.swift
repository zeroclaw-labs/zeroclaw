import SwiftUI

struct ToolsBrowserView: View {
    @EnvironmentObject var agentService: AgentService

    @State private var tools: [ToolInfo] = []
    @State private var searchText = ""
    @State private var isLoading = false
    @State private var error: String?

    private var filtered: [ToolInfo] {
        guard !searchText.isEmpty else { return tools }
        return tools.filter {
            $0.name.localizedCaseInsensitiveContains(searchText)
            || ($0.description?.localizedCaseInsensitiveContains(searchText) ?? false)
            || ($0.category?.localizedCaseInsensitiveContains(searchText) ?? false)
        }
    }

    private var grouped: [(String, [ToolInfo])] {
        Dictionary(grouping: filtered) { $0.category ?? "Other" }
            .sorted { $0.key < $1.key }
    }

    var body: some View {
        List {
            if let error {
                Section {
                    Label(error, systemImage: "exclamationmark.triangle")
                        .foregroundStyle(.red)
                        .font(.caption)
                }
            }

            if tools.isEmpty && !isLoading {
                ContentUnavailableView("No Tools", systemImage: "wrench.and.screwdriver", description: Text("No tools registered on the gateway."))
            }

            ForEach(grouped, id: \.0) { category, categoryTools in
                Section(category) {
                    ForEach(categoryTools) { tool in
                        VStack(alignment: .leading, spacing: 2) {
                            Text(tool.name)
                                .font(.system(.subheadline, design: .monospaced))
                            if let desc = tool.description {
                                Text(desc)
                                    .font(.caption)
                                    .foregroundStyle(.secondary)
                                    .lineLimit(2)
                            }
                        }
                    }
                }
            }
        }
        .navigationTitle("Tools (\(tools.count))")
        .navigationBarTitleDisplayMode(.inline)
        .searchable(text: $searchText, prompt: "Search tools")
        .refreshable { await fetchTools() }
        .task { await fetchTools() }
        .overlay { if isLoading && tools.isEmpty { ProgressView() } }
    }

    private func fetchTools() async {
        isLoading = true
        error = nil
        do {
            tools = try await agentService.gateway.fetchTools()
        } catch {
            self.error = error.localizedDescription
        }
        isLoading = false
    }
}

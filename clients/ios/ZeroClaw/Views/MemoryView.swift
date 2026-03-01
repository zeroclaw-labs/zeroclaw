import SwiftUI

struct MemoryView: View {
    @EnvironmentObject var agentService: AgentService

    @State private var entries: [MemoryEntry] = []
    @State private var searchQuery = ""
    @State private var isLoading = false
    @State private var error: String?
    @State private var showAddSheet = false

    var body: some View {
        List {
            if let error {
                Section {
                    Label(error, systemImage: "exclamationmark.triangle")
                        .foregroundStyle(.red)
                        .font(.caption)
                }
            }

            if entries.isEmpty && !isLoading {
                ContentUnavailableView("No Memories", systemImage: "brain", description: Text("The agent's knowledge base is empty."))
            }

            ForEach(entries) { entry in
                VStack(alignment: .leading, spacing: 4) {
                    HStack {
                        Text(entry.key)
                            .font(.headline)
                        Spacer()
                        if let cat = entry.category {
                            Text(cat)
                                .font(.caption2)
                                .padding(.horizontal, 6)
                                .padding(.vertical, 2)
                                .background(Color(.tertiarySystemFill))
                                .clipShape(Capsule())
                        }
                    }
                    Text(entry.content)
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                        .lineLimit(3)
                }
                .swipeActions(edge: .trailing) {
                    Button(role: .destructive) {
                        deleteEntry(key: entry.key)
                    } label: {
                        Label("Delete", systemImage: "trash")
                    }
                }
            }
        }
        .navigationTitle("Memory")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                Button { showAddSheet = true } label: {
                    Image(systemName: "plus")
                }
            }
        }
        .sheet(isPresented: $showAddSheet) {
            AddMemorySheet { key, content, category in
                await addEntry(key: key, content: content, category: category)
            }
        }
        .searchable(text: $searchQuery, prompt: "Search memories")
        .refreshable { await fetchEntries() }
        .task { await fetchEntries() }
        .onChange(of: searchQuery) { Task { await fetchEntries() } }
        .overlay { if isLoading && entries.isEmpty { ProgressView() } }
    }

    private func fetchEntries() async {
        isLoading = true
        error = nil
        do {
            let query = searchQuery.isEmpty ? nil : searchQuery
            entries = try await agentService.gateway.fetchMemory(query: query)
        } catch {
            self.error = error.localizedDescription
        }
        isLoading = false
    }

    private func deleteEntry(key: String) {
        Task {
            do {
                try await agentService.gateway.deleteMemory(key: key)
                entries.removeAll { $0.key == key }
            } catch {
                self.error = error.localizedDescription
            }
        }
    }

    private func addEntry(key: String, content: String, category: String?) async {
        do {
            try await agentService.gateway.storeMemory(key: key, content: content, category: category)
            await fetchEntries()
        } catch {
            self.error = error.localizedDescription
        }
    }
}

// MARK: - Add Memory Sheet

private struct AddMemorySheet: View {
    @Environment(\.dismiss) private var dismiss

    let onSave: (String, String, String?) async -> Void

    @State private var key = ""
    @State private var content = ""
    @State private var category = ""

    var body: some View {
        NavigationStack {
            Form {
                TextField("Key", text: $key)
                    .autocorrectionDisabled()
                    .textInputAutocapitalization(.never)
                Section("Content") {
                    TextEditor(text: $content)
                        .frame(minHeight: 100)
                }
                TextField("Category (optional)", text: $category)
                    .autocorrectionDisabled()
                    .textInputAutocapitalization(.never)
            }
            .navigationTitle("Add Memory")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Save") {
                        Task {
                            await onSave(key, content, category.isEmpty ? nil : category)
                            dismiss()
                        }
                    }
                    .disabled(key.isEmpty || content.isEmpty)
                }
            }
        }
        .presentationDetents([.medium])
    }
}

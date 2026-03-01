import SwiftUI

struct CronJobsView: View {
    @EnvironmentObject var agentService: AgentService

    @State private var jobs: [CronJob] = []
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

            if jobs.isEmpty && !isLoading {
                ContentUnavailableView("No Scheduled Tasks", systemImage: "clock.badge.questionmark", description: Text("No cron jobs configured."))
            }

            ForEach(jobs) { job in
                VStack(alignment: .leading, spacing: 4) {
                    HStack {
                        Text(job.name)
                            .font(.headline)
                        Spacer()
                        if job.enabled == false {
                            Text("Disabled")
                                .font(.caption2)
                                .foregroundStyle(.secondary)
                        }
                    }
                    Text(job.schedule)
                        .font(.system(.caption, design: .monospaced))
                        .foregroundStyle(.zeroClawOrange)
                    Text(job.command)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(2)
                    if let next = job.nextRun {
                        Text("Next: \(next)")
                            .font(.caption2)
                            .foregroundStyle(.tertiary)
                    }
                }
                .swipeActions(edge: .trailing) {
                    Button(role: .destructive) {
                        deleteJob(id: job.id)
                    } label: {
                        Label("Delete", systemImage: "trash")
                    }
                }
            }
        }
        .navigationTitle("Scheduled Tasks")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                Button { showAddSheet = true } label: {
                    Image(systemName: "plus")
                }
            }
        }
        .sheet(isPresented: $showAddSheet) {
            AddCronSheet { name, schedule, command in
                await addJob(name: name, schedule: schedule, command: command)
            }
        }
        .refreshable { await fetchJobs() }
        .task { await fetchJobs() }
        .overlay { if isLoading && jobs.isEmpty { ProgressView() } }
    }

    private func fetchJobs() async {
        isLoading = true
        error = nil
        do {
            jobs = try await agentService.gateway.fetchCronJobs()
        } catch {
            self.error = error.localizedDescription
        }
        isLoading = false
    }

    private func deleteJob(id: String) {
        Task {
            do {
                try await agentService.gateway.deleteCronJob(id: id)
                jobs.removeAll { $0.id == id }
            } catch {
                self.error = error.localizedDescription
            }
        }
    }

    private func addJob(name: String, schedule: String, command: String) async {
        do {
            try await agentService.gateway.createCronJob(name: name, schedule: schedule, command: command)
            await fetchJobs()
        } catch {
            self.error = error.localizedDescription
        }
    }
}

// MARK: - Add Cron Sheet

private struct AddCronSheet: View {
    @Environment(\.dismiss) private var dismiss

    let onSave: (String, String, String) async -> Void

    @State private var name = ""
    @State private var schedule = ""
    @State private var command = ""

    var body: some View {
        NavigationStack {
            Form {
                TextField("Name", text: $name)
                TextField("Schedule (cron expression)", text: $schedule)
                    .font(.system(.body, design: .monospaced))
                    .autocorrectionDisabled()
                    .textInputAutocapitalization(.never)
                Section("Command") {
                    TextEditor(text: $command)
                        .font(.system(.body, design: .monospaced))
                        .frame(minHeight: 80)
                }
            }
            .navigationTitle("Add Task")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Save") {
                        Task {
                            await onSave(name, schedule, command)
                            dismiss()
                        }
                    }
                    .disabled(name.isEmpty || schedule.isEmpty || command.isEmpty)
                }
            }
        }
        .presentationDetents([.medium])
    }
}

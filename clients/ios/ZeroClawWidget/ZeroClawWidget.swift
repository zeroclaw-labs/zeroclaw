import WidgetKit
import SwiftUI

/// Timeline provider that reads shared state from App Groups UserDefaults.
struct ZeroClawTimelineProvider: TimelineProvider {
    private let defaults = UserDefaults(suiteName: "group.ai.zeroclaw")

    func placeholder(in context: Context) -> ZeroClawEntry {
        ZeroClawEntry(
            date: Date(),
            isConnected: false,
            statusText: "ZeroClaw",
            lastMessage: nil,
            lastRole: nil
        )
    }

    func getSnapshot(in context: Context, completion: @escaping (ZeroClawEntry) -> Void) {
        completion(currentEntry())
    }

    func getTimeline(in context: Context, completion: @escaping (Timeline<ZeroClawEntry>) -> Void) {
        let entry = currentEntry()
        // Refresh in 15 minutes
        let nextUpdate = Calendar.current.date(byAdding: .minute, value: 15, to: Date()) ?? Date()
        let timeline = Timeline(entries: [entry], policy: .after(nextUpdate))
        completion(timeline)
    }

    private func currentEntry() -> ZeroClawEntry {
        ZeroClawEntry(
            date: Date(),
            isConnected: defaults?.bool(forKey: "widget_connected") ?? false,
            statusText: defaults?.string(forKey: "widget_status") ?? "Disconnected",
            lastMessage: defaults?.string(forKey: "widget_last_message"),
            lastRole: defaults?.string(forKey: "widget_last_role")
        )
    }
}

struct ZeroClawEntry: TimelineEntry {
    let date: Date
    let isConnected: Bool
    let statusText: String
    let lastMessage: String?
    let lastRole: String?
}

struct ZeroClawWidgetEntryView: View {
    var entry: ZeroClawEntry
    @Environment(\.widgetFamily) var family

    var body: some View {
        switch family {
        case .systemSmall:
            smallWidget
        default:
            mediumWidget
        }
    }

    private var smallWidget: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Circle()
                    .fill(entry.isConnected ? .green : .red)
                    .frame(width: 8, height: 8)
                Text("ZeroClaw")
                    .font(.caption.bold())
            }

            Text(entry.statusText)
                .font(.caption2)
                .foregroundStyle(.secondary)

            Spacer()

            if let message = entry.lastMessage {
                Text(message)
                    .font(.caption2)
                    .lineLimit(3)
                    .foregroundStyle(.secondary)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding()
    }

    private var mediumWidget: some View {
        HStack(spacing: 12) {
            VStack(alignment: .leading, spacing: 6) {
                HStack {
                    Circle()
                        .fill(entry.isConnected ? .green : .red)
                        .frame(width: 8, height: 8)
                    Text("ZeroClaw")
                        .font(.subheadline.bold())
                }

                Text(entry.statusText)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }

            if let message = entry.lastMessage {
                Divider()
                VStack(alignment: .leading, spacing: 4) {
                    if let role = entry.lastRole {
                        Text(role == "user" ? "You" : "Agent")
                            .font(.caption2.bold())
                            .foregroundStyle(.secondary)
                    }
                    Text(message)
                        .font(.caption)
                        .lineLimit(4)
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding()
    }
}

@main
struct ZeroClawWidget: Widget {
    let kind = "ZeroClawWidget"

    var body: some WidgetConfiguration {
        StaticConfiguration(
            kind: kind,
            provider: ZeroClawTimelineProvider()
        ) { entry in
            ZeroClawWidgetEntryView(entry: entry)
                .containerBackground(.fill.tertiary, for: .widget)
        }
        .configurationDisplayName("ZeroClaw")
        .description("View agent status and recent messages.")
        .supportedFamilies([.systemSmall, .systemMedium])
    }
}

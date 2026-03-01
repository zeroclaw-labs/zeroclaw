import SwiftUI

/// Renders markdown content with support for code blocks, inline formatting, and paragraphs.
struct MarkdownView: View {
    let content: String

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            ForEach(Array(parseBlocks(content).enumerated()), id: \.offset) { _, block in
                if block.isCode {
                    CodeBlockView(language: block.language, code: block.text)
                } else if !block.text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                    textView(for: block.text)
                }
            }
        }
    }

    @ViewBuilder
    private func textView(for text: String) -> some View {
        if let attributed = try? AttributedString(markdown: text, options: .init(interpretedSyntax: .inlineOnlyPreservingWhitespace)) {
            Text(attributed)
        } else {
            Text(text)
        }
    }
}

// MARK: - Code Block View

struct CodeBlockView: View {
    let language: String?
    let code: String

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            // Header with language label and copy button
            HStack {
                Text(language ?? "code")
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                Spacer()
                Button {
                    UIPasteboard.general.string = code
                } label: {
                    Image(systemName: "doc.on.doc")
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                }
                .buttonStyle(.plain)
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 6)

            Divider()

            // Code content
            ScrollView(.horizontal, showsIndicators: false) {
                Text(code)
                    .font(.system(.caption, design: .monospaced))
                    .textSelection(.enabled)
                    .padding(12)
            }
        }
        .background(Color(.tertiarySystemBackground))
        .clipShape(RoundedRectangle(cornerRadius: 10))
    }
}

// MARK: - Markdown Parser

private struct ContentBlock {
    let isCode: Bool
    let language: String?
    let text: String
}

private func parseBlocks(_ content: String) -> [ContentBlock] {
    var blocks: [ContentBlock] = []
    var current = ""
    var inCode = false
    var codeLang: String?

    for line in content.components(separatedBy: "\n") {
        if line.trimmingCharacters(in: .whitespaces).hasPrefix("```") {
            if inCode {
                // Close code block
                blocks.append(ContentBlock(isCode: true, language: codeLang, text: current))
                current = ""
                inCode = false
                codeLang = nil
            } else {
                // Open code block — flush text
                if !current.isEmpty {
                    blocks.append(ContentBlock(isCode: false, language: nil, text: current))
                    current = ""
                }
                let lang = line.trimmingCharacters(in: .whitespaces).dropFirst(3).trimmingCharacters(in: .whitespaces)
                codeLang = lang.isEmpty ? nil : lang
                inCode = true
            }
        } else {
            if !current.isEmpty { current += "\n" }
            current += line
        }
    }

    // Flush remaining
    if !current.isEmpty {
        blocks.append(ContentBlock(isCode: inCode, language: codeLang, text: current))
    }

    return blocks
}

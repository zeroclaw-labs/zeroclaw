//! Outbound message chunking: split agent text into Discord's 2000-character
//! message limit, with a paragraph- and code-fence-aware multi-message mode.

use super::types::DISCORD_MAX_MESSAGE_LENGTH;

/// Split a message into chunks that respect Discord's 2000-character limit.
/// Tries to split at word boundaries when possible.
pub(crate) fn split_message_for_discord(message: &str) -> Vec<String> {
    if message.chars().count() <= DISCORD_MAX_MESSAGE_LENGTH {
        return vec![message.to_string()];
    }

    let mut chunks = Vec::new();
    let mut remaining = message;

    while !remaining.is_empty() {
        // Find the byte offset for the 2000th character boundary.
        // If there are fewer than 2000 chars left, we can emit the tail directly.
        let hard_split = remaining
            .char_indices()
            .nth(DISCORD_MAX_MESSAGE_LENGTH)
            .map_or(remaining.len(), |(idx, _)| idx);

        let chunk_end = if hard_split == remaining.len() {
            hard_split
        } else {
            // Try to find a good break point (newline, then space)
            let search_area = &remaining[..hard_split];

            // Prefer splitting at newline
            if let Some(pos) = search_area.rfind('\n') {
                // Don't split if the newline is too close to the end
                if search_area[..pos].chars().count() >= DISCORD_MAX_MESSAGE_LENGTH / 2 {
                    pos + 1
                } else {
                    // Try space as fallback
                    search_area.rfind(' ').map_or(hard_split, |space| space + 1)
                }
            } else if let Some(pos) = search_area.rfind(' ') {
                pos + 1
            } else {
                // Hard split at the limit
                hard_split
            }
        };

        chunks.push(remaining[..chunk_end].to_string());
        remaining = &remaining[chunk_end..];
    }

    chunks
}

/// Split a message into multiple logical chunks at paragraph boundaries for
/// multi-message delivery. Respects code fences — never splits inside a
/// fenced code block. Falls back to [`split_message_for_discord`] for any
/// segment that exceeds `max_len`.
pub(crate) fn split_message_for_discord_multi(content: &str, max_len: usize) -> Vec<String> {
    if content.is_empty() {
        return vec![];
    }

    // Gather paragraph-level segments, respecting code fences.
    let mut segments: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut in_fence = false;

    for line in content.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            in_fence = !in_fence;
        }

        // If we hit a blank line outside a fence, that's a paragraph break.
        if line.is_empty() && !in_fence && !current.is_empty() {
            segments.push(current.trim_end().to_string());
            current.clear();
            continue;
        }

        if !current.is_empty() {
            current.push('\n');
        }
        current.push_str(line);
    }
    if !current.is_empty() {
        segments.push(current.trim_end().to_string());
    }

    // Now coalesce small segments and split oversized ones.
    let mut chunks: Vec<String> = Vec::new();

    for segment in segments {
        if segment.chars().count() > max_len {
            // This segment (possibly a large code fence) exceeds the limit.
            // Fall back to the word-boundary splitter.
            let sub_chunks = split_message_for_discord(&segment);
            chunks.extend(sub_chunks);
        } else {
            chunks.push(segment);
        }
    }

    if chunks.is_empty() {
        vec![content.to_string()]
    } else {
        chunks
    }
}

/// Choose the chunks to deliver for an outbound Discord message.
///
/// `split_message_for_discord_multi` returns an empty vec for empty input
/// (its paragraph splitter has no segments to emit); the non-multi
/// splitter returns `vec![""]`. When MultiMessage stream mode hands
/// `send()` a paragraph that collapses to empty text after marker strip,
/// the chunk loop would iterate zero times and silently skip an attached
/// file upload. Force a single empty chunk in exactly that case so the
/// multipart POST fires.
pub(crate) fn chunks_for_send(
    content: &str,
    stream_mode: zeroclaw_config::schema::StreamMode,
    max_len: usize,
    has_local_files: bool,
) -> Vec<String> {
    let mut chunks = match stream_mode {
        zeroclaw_config::schema::StreamMode::MultiMessage => {
            split_message_for_discord_multi(content, max_len)
        }
        _ => split_message_for_discord(content),
    };
    if chunks.is_empty() && has_local_files {
        chunks.push(String::new());
    }
    chunks
}

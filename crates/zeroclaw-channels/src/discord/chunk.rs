//! Outbound message chunking: split agent text into Discord's 2000-character
//! message limit, with a paragraph- and code-fence-aware multi-message mode.
//! Also fits a message's embeds to Discord's structural limits (see
//! [`budget_embeds`]).

use super::embed::DiscordEmbed;
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

// ─────────────────────────────────────────────────────────────────────────────
// Embed budgeting: clamp a message's embeds to Discord's documented limits.
//
// Length overruns on a single text field (title/description/field/footer/author)
// are trimmed with an ellipsis silently — graceful degradation, no warning.
// Structural overflow (too many embeds, too many fields, or a total character
// count Discord would reject outright) drops the excess and reports `true`, so
// the caller can surface a ⚠️ reaction the way a dropped media marker does.
// ─────────────────────────────────────────────────────────────────────────────

const EMBED_MAX_TITLE: usize = 256;
const EMBED_MAX_DESCRIPTION: usize = 4096;
const EMBED_MAX_FIELD_NAME: usize = 256;
const EMBED_MAX_FIELD_VALUE: usize = 1024;
const EMBED_MAX_FOOTER_TEXT: usize = 2048;
const EMBED_MAX_AUTHOR_NAME: usize = 256;
const EMBED_MAX_FIELDS: usize = 25;
const EMBED_MAX_PER_MESSAGE: usize = 10;
/// Discord caps the *sum* of all characters across one embed (and across the
/// whole message's embeds) at 6000.
const EMBED_MAX_TOTAL_CHARS: usize = 6000;

/// Clamp `embeds` to Discord's limits in place. Returns `true` when a
/// structural limit forced something to be dropped (so the caller fires ⚠️);
/// silent length trims alone return `false`.
pub(crate) fn budget_embeds(embeds: &mut Vec<DiscordEmbed>) -> bool {
    let mut overflow = false;

    if embeds.len() > EMBED_MAX_PER_MESSAGE {
        embeds.truncate(EMBED_MAX_PER_MESSAGE);
        overflow = true;
    }

    for embed in embeds.iter_mut() {
        if let Some(title) = embed.title.as_mut() {
            truncate_chars(title, EMBED_MAX_TITLE);
        }
        if let Some(description) = embed.description.as_mut() {
            truncate_chars(description, EMBED_MAX_DESCRIPTION);
        }
        if let Some(footer) = embed.footer.as_mut() {
            truncate_chars(&mut footer.text, EMBED_MAX_FOOTER_TEXT);
        }
        if let Some(author) = embed.author.as_mut() {
            truncate_chars(&mut author.name, EMBED_MAX_AUTHOR_NAME);
        }
        if embed.fields.len() > EMBED_MAX_FIELDS {
            embed.fields.truncate(EMBED_MAX_FIELDS);
            overflow = true;
        }
        for field in embed.fields.iter_mut() {
            truncate_chars(&mut field.name, EMBED_MAX_FIELD_NAME);
            truncate_chars(&mut field.value, EMBED_MAX_FIELD_VALUE);
        }
    }

    // Discord caps the character SUM across *all* of a message's embeds at 6000.
    // Drop whole trailing embeds until the message total fits; if a single
    // surviving embed still overflows (one embed alone can exceed 6000 via a
    // full description + footer, or many fields), shrink it to fit — shave the
    // description, then drop trailing fields. Title+footer+author alone can't
    // exceed 2560, so this always converges.
    while total_embed_chars(embeds) > EMBED_MAX_TOTAL_CHARS {
        if embeds.len() > 1 {
            embeds.pop();
            overflow = true;
            continue;
        }
        let embed = &mut embeds[0];
        let excess = embed_char_count(embed).saturating_sub(EMBED_MAX_TOTAL_CHARS);
        if let Some(description) = embed.description.as_mut() {
            let target = description.chars().count().saturating_sub(excess);
            truncate_chars(description, target);
        }
        while embed_char_count(embed) > EMBED_MAX_TOTAL_CHARS && !embed.fields.is_empty() {
            embed.fields.pop();
        }
        overflow = true;
        break;
    }

    overflow
}

/// Number of characters Discord counts toward an embed's 6000-char budget:
/// title, description, every field name+value, footer text, and author name.
fn embed_char_count(embed: &DiscordEmbed) -> usize {
    let mut count = 0;
    count += embed.title.as_ref().map_or(0, |s| s.chars().count());
    count += embed.description.as_ref().map_or(0, |s| s.chars().count());
    count += embed.footer.as_ref().map_or(0, |f| f.text.chars().count());
    count += embed.author.as_ref().map_or(0, |a| a.name.chars().count());
    for field in &embed.fields {
        count += field.name.chars().count() + field.value.chars().count();
    }
    count
}

fn total_embed_chars(embeds: &[DiscordEmbed]) -> usize {
    embeds.iter().map(embed_char_count).sum()
}

/// Truncate `s` to at most `max` characters, appending an ellipsis when it had
/// to cut (the ellipsis counts toward `max`). A char-boundary-safe trim.
fn truncate_chars(s: &mut String, max: usize) {
    if s.chars().count() <= max {
        return;
    }
    if max == 0 {
        s.clear();
        return;
    }
    let keep = max - 1;
    let mut out: String = s.chars().take(keep).collect();
    out.push('…');
    *s = out;
}

#[cfg(test)]
mod embed_budget_tests {
    use super::super::embed::{DiscordEmbed, EmbedField, EmbedFooter};
    use super::{budget_embeds, embed_char_count};

    #[test]
    fn description_over_limit_is_trimmed_without_flagging_overflow() {
        let mut embeds = vec![DiscordEmbed {
            description: Some("x".repeat(5000)),
            ..Default::default()
        }];
        let overflow = budget_embeds(&mut embeds);
        assert!(
            !overflow,
            "a graceful length trim is not structural overflow"
        );
        let desc = embeds[0].description.as_ref().unwrap();
        assert_eq!(desc.chars().count(), 4096);
        assert!(desc.ends_with('…'));
    }

    #[test]
    fn more_than_25_fields_drops_extras_and_flags_overflow() {
        let fields: Vec<EmbedField> = (0..30)
            .map(|i| EmbedField {
                name: format!("n{i}"),
                value: "v".to_string(),
                inline: false,
            })
            .collect();
        let mut embeds = vec![DiscordEmbed {
            fields,
            ..Default::default()
        }];
        assert!(budget_embeds(&mut embeds));
        assert_eq!(embeds[0].fields.len(), 25);
    }

    #[test]
    fn more_than_10_embeds_drops_extras_and_flags_overflow() {
        let mut embeds: Vec<DiscordEmbed> = (0..15)
            .map(|i| DiscordEmbed {
                title: Some(format!("t{i}")),
                ..Default::default()
            })
            .collect();
        assert!(budget_embeds(&mut embeds));
        assert_eq!(embeds.len(), 10);
    }

    #[test]
    fn total_char_budget_drops_trailing_embeds() {
        let big = || DiscordEmbed {
            description: Some("x".repeat(4000)),
            ..Default::default()
        };
        let mut embeds = vec![big(), big()];
        assert!(budget_embeds(&mut embeds));
        assert_eq!(embeds.len(), 1);
    }

    #[test]
    fn single_embed_with_oversized_fields_is_shrunk_to_fit() {
        // The overflow lives entirely in fields (no description to shave), so
        // the budgeter must drop trailing fields to fit the 6000 cap — else the
        // embed reaches Discord oversized and the whole message 400s.
        let fields: Vec<EmbedField> = (0..25)
            .map(|i| EmbedField {
                name: format!("{i:0<256}"),
                value: "v".repeat(1024),
                inline: false,
            })
            .collect();
        let mut embeds = vec![DiscordEmbed {
            fields,
            ..Default::default()
        }];
        assert!(budget_embeds(&mut embeds));
        assert!(
            embed_char_count(&embeds[0]) <= 6000,
            "single embed must fit the 6000 message-wide cap, got {}",
            embed_char_count(&embeds[0])
        );
    }

    #[test]
    fn single_embed_total_over_6000_trims_description() {
        let mut embeds = vec![DiscordEmbed {
            description: Some("d".repeat(5000)),
            footer: Some(EmbedFooter {
                text: "f".repeat(2000),
                icon_url: None,
            }),
            ..Default::default()
        }];
        assert!(budget_embeds(&mut embeds));
        let total = embeds[0].description.as_ref().unwrap().chars().count()
            + embeds[0].footer.as_ref().unwrap().text.chars().count();
        assert!(total <= 6000, "embed total {total} must fit 6000");
    }

    #[test]
    fn within_limits_is_left_untouched() {
        let mut embeds = vec![DiscordEmbed {
            title: Some("T".to_string()),
            description: Some("body".to_string()),
            ..Default::default()
        }];
        let before = embeds.clone();
        assert!(!budget_embeds(&mut embeds));
        assert_eq!(embeds, before);
    }
}

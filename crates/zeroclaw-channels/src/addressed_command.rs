//! Helpers for channel messages that address runtime slash commands to the bot.
//!
//! This is a transitional normalization helper, not a channel protocol parser.
//! Callers must supply channel-native address candidates from the channel
//! implementation. Do not add Matrix/Discord/Mattermost/etc. mention discovery
//! here; the long-term command-ingress trait belongs in `zeroclaw-commands`
//! and should be implemented by the channel-bound objects that already know
//! their platform syntax. This helper should disappear when that trait exists.

pub(crate) fn strip_leading_addressed_command<I, S>(body: &str, addresses: I) -> Option<&str>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let trimmed = body.trim_start();
    for address in addresses {
        let candidate = address.as_ref().trim();
        if candidate.is_empty() {
            continue;
        }
        if let Some(command) = strip_address_candidate(trimmed, candidate) {
            return Some(command);
        }
    }
    None
}

fn strip_address_candidate<'a>(trimmed: &'a str, candidate: &str) -> Option<&'a str> {
    let rest = strip_prefix_ignore_ascii_case(trimmed, candidate)?;
    let rest = strip_address_separator(rest)?;
    let command = rest.trim_start();
    command.starts_with('/').then_some(command)
}

fn strip_prefix_ignore_ascii_case<'a>(value: &'a str, prefix: &str) -> Option<&'a str> {
    let head = value.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then_some(&value[prefix.len()..])
}

fn strip_address_separator(rest: &str) -> Option<&str> {
    let mut chars = rest.chars();
    match chars.next()? {
        ':' | ',' => Some(chars.as_str()),
        c if c.is_whitespace() => Some(rest),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::strip_leading_addressed_command;

    #[test]
    fn strips_common_channel_mention_command_shapes() {
        assert_eq!(
            strip_leading_addressed_command("@bot /goal status", ["@bot"]),
            Some("/goal status")
        );
        assert_eq!(
            strip_leading_addressed_command("@bot: /goal status", ["@bot"]),
            Some("/goal status")
        );
        assert_eq!(
            strip_leading_addressed_command("@bot,/goal status", ["@bot"]),
            Some("/goal status")
        );
        assert_eq!(
            strip_leading_addressed_command("<@U123> /goal status", ["<@U123>"]),
            Some("/goal status")
        );
        assert_eq!(
            strip_leading_addressed_command("Nick: /goal status", ["nick"]),
            Some("/goal status")
        );
    }

    #[test]
    fn rejects_non_commands_and_prefix_collisions() {
        assert_eq!(
            strip_leading_addressed_command("@bot hello /goal", ["@bot"]),
            None
        );
        assert_eq!(
            strip_leading_addressed_command("@bot-helper: /goal status", ["@bot"]),
            None
        );
        assert_eq!(
            strip_leading_addressed_command("@other: /goal status", ["@bot"]),
            None
        );
        assert_eq!(
            strip_leading_addressed_command("/goal status", ["@bot"]),
            None
        );
    }
}

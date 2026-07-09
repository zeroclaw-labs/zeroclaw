use serde_json::{Map, Value};

/// A Discord rich embed. Every field is optional per the Discord API;
/// [`to_api`](DiscordEmbed::to_api) emits only the populated ones, so an empty
/// embed serializes to `{}` and a content-only message never grows an
/// `"embeds"` key (the EPIC A byte-identity invariant).
#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct DiscordEmbed {
    pub(crate) title: Option<String>,
    pub(crate) description: Option<String>,
    pub(crate) url: Option<String>,
    pub(crate) color: Option<u32>,
    /// ISO-8601 timestamp string (Discord renders it in the embed footer line).
    pub(crate) timestamp: Option<String>,
    pub(crate) footer: Option<EmbedFooter>,
    pub(crate) image: Option<EmbedMedia>,
    pub(crate) thumbnail: Option<EmbedMedia>,
    pub(crate) author: Option<EmbedAuthor>,
    pub(crate) fields: Vec<EmbedField>,
}

#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct EmbedFooter {
    pub(crate) text: String,
    pub(crate) icon_url: Option<String>,
}

/// An embed image or thumbnail — Discord only reads the `url` for these.
#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct EmbedMedia {
    pub(crate) url: String,
}

#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct EmbedAuthor {
    pub(crate) name: String,
    pub(crate) url: Option<String>,
    pub(crate) icon_url: Option<String>,
}

#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct EmbedField {
    pub(crate) name: String,
    pub(crate) value: String,
    pub(crate) inline: bool,
}

impl DiscordEmbed {
    /// Serialize to the Discord embed-object JSON, dropping every absent field
    /// (`None`/empty) so the wire payload carries only what the author set.
    pub(crate) fn to_api(&self) -> Value {
        let mut obj = Map::new();
        if let Some(title) = &self.title {
            obj.insert("title".to_string(), Value::String(title.clone()));
        }
        if let Some(description) = &self.description {
            obj.insert(
                "description".to_string(),
                Value::String(description.clone()),
            );
        }
        if let Some(url) = &self.url {
            obj.insert("url".to_string(), Value::String(url.clone()));
        }
        if let Some(color) = self.color {
            obj.insert("color".to_string(), Value::Number(color.into()));
        }
        if let Some(timestamp) = &self.timestamp {
            obj.insert("timestamp".to_string(), Value::String(timestamp.clone()));
        }
        if let Some(footer) = &self.footer {
            let mut footer_obj = Map::new();
            footer_obj.insert("text".to_string(), Value::String(footer.text.clone()));
            if let Some(icon_url) = &footer.icon_url {
                footer_obj.insert("icon_url".to_string(), Value::String(icon_url.clone()));
            }
            obj.insert("footer".to_string(), Value::Object(footer_obj));
        }
        if let Some(image) = &self.image {
            obj.insert("image".to_string(), media_json(&image.url));
        }
        if let Some(thumbnail) = &self.thumbnail {
            obj.insert("thumbnail".to_string(), media_json(&thumbnail.url));
        }
        if let Some(author) = &self.author {
            let mut author_obj = Map::new();
            author_obj.insert("name".to_string(), Value::String(author.name.clone()));
            if let Some(url) = &author.url {
                author_obj.insert("url".to_string(), Value::String(url.clone()));
            }
            if let Some(icon_url) = &author.icon_url {
                author_obj.insert("icon_url".to_string(), Value::String(icon_url.clone()));
            }
            obj.insert("author".to_string(), Value::Object(author_obj));
        }
        if !self.fields.is_empty() {
            let fields: Vec<Value> = self
                .fields
                .iter()
                .map(|field| {
                    let mut field_obj = Map::new();
                    field_obj.insert("name".to_string(), Value::String(field.name.clone()));
                    field_obj.insert("value".to_string(), Value::String(field.value.clone()));
                    field_obj.insert("inline".to_string(), Value::Bool(field.inline));
                    Value::Object(field_obj)
                })
                .collect();
            obj.insert("fields".to_string(), Value::Array(fields));
        }
        Value::Object(obj)
    }

    /// True when no field carries content — `markers::spec_to_embed` uses it to
    /// drop a spec that parsed but would render as an empty box.
    pub(crate) fn is_empty(&self) -> bool {
        self.title.is_none()
            && self.description.is_none()
            && self.url.is_none()
            && self.color.is_none()
            && self.timestamp.is_none()
            && self.footer.is_none()
            && self.image.is_none()
            && self.thumbnail.is_none()
            && self.author.is_none()
            && self.fields.is_empty()
    }
}

fn media_json(url: &str) -> Value {
    let mut obj = Map::new();
    obj.insert("url".to_string(), Value::String(url.to_string()));
    Value::Object(obj)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_embed_serializes_to_empty_object() {
        assert_eq!(DiscordEmbed::default().to_api(), serde_json::json!({}));
    }

    #[test]
    fn to_api_drops_absent_fields() {
        let embed = DiscordEmbed {
            title: Some("Title".to_string()),
            description: Some("Body".to_string()),
            ..Default::default()
        };
        assert_eq!(
            embed.to_api(),
            serde_json::json!({ "title": "Title", "description": "Body" })
        );
    }

    #[test]
    fn to_api_emits_every_populated_field() {
        let embed = DiscordEmbed {
            title: Some("T".to_string()),
            description: Some("D".to_string()),
            url: Some("https://example.com".to_string()),
            color: Some(0x5865F2),
            timestamp: Some("2026-06-17T00:00:00Z".to_string()),
            footer: Some(EmbedFooter {
                text: "F".to_string(),
                icon_url: Some("https://example.com/f.png".to_string()),
            }),
            image: Some(EmbedMedia {
                url: "https://example.com/i.png".to_string(),
            }),
            thumbnail: Some(EmbedMedia {
                url: "https://example.com/t.png".to_string(),
            }),
            author: Some(EmbedAuthor {
                name: "A".to_string(),
                url: Some("https://example.com/a".to_string()),
                icon_url: Some("https://example.com/a.png".to_string()),
            }),
            fields: vec![
                EmbedField {
                    name: "n1".to_string(),
                    value: "v1".to_string(),
                    inline: true,
                },
                EmbedField {
                    name: "n2".to_string(),
                    value: "v2".to_string(),
                    inline: false,
                },
            ],
        };
        assert_eq!(
            embed.to_api(),
            serde_json::json!({
                "title": "T",
                "description": "D",
                "url": "https://example.com",
                "color": 0x5865F2,
                "timestamp": "2026-06-17T00:00:00Z",
                "footer": { "text": "F", "icon_url": "https://example.com/f.png" },
                "image": { "url": "https://example.com/i.png" },
                "thumbnail": { "url": "https://example.com/t.png" },
                "author": {
                    "name": "A",
                    "url": "https://example.com/a",
                    "icon_url": "https://example.com/a.png"
                },
                "fields": [
                    { "name": "n1", "value": "v1", "inline": true },
                    { "name": "n2", "value": "v2", "inline": false }
                ]
            })
        );
    }

    #[test]
    fn footer_without_icon_omits_icon_url() {
        let embed = DiscordEmbed {
            footer: Some(EmbedFooter {
                text: "only text".to_string(),
                icon_url: None,
            }),
            ..Default::default()
        };
        assert_eq!(
            embed.to_api(),
            serde_json::json!({ "footer": { "text": "only text" } })
        );
    }
}

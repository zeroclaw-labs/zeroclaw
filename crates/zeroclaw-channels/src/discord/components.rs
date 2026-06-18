//! Discord message components — the interactive action rows that fill
//! `DiscordOutgoing.components`: their data shapes, ergonomic builders, and
//! serialization to Discord's component JSON. A message carries up to 5 action
//! rows; each row holds up to 5 buttons, or exactly one select menu. Every
//! interactive (non-link) component routes through a [`CustomId`] so the inbound
//! dispatch (EPIC B Phase 2) can recognize and route the click. Modal text
//! inputs are added with modal handling (Phase 3) — a message action row never
//! holds them.

// The builder API below is constructed starting EPIC B Phase 4 (buttoned
// approval emits the first real buttons); the `to_api` serializers are already
// live via `DiscordOutgoing::to_rest_json`. Until a phase wires a caller the
// builders/variants have no in-crate use outside tests — lifted per phase.
#![allow(dead_code)]

use serde_json::{Value, json};

use super::custom_id::CustomId;

/// Discord's per-message and per-row component limits.
const MAX_ROWS_PER_MESSAGE: usize = 5;
const MAX_BUTTONS_PER_ROW: usize = 5;

/// A button's visual style. `Link` is modelled separately
/// ([`DiscordComponent::LinkButton`]) because it carries a URL instead of a
/// `custom_id`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ButtonStyle {
    Primary,
    Secondary,
    Success,
    Danger,
}

impl ButtonStyle {
    fn wire(self) -> u64 {
        match self {
            ButtonStyle::Primary => 1,
            ButtonStyle::Secondary => 2,
            ButtonStyle::Success => 3,
            ButtonStyle::Danger => 4,
        }
    }
}

/// One choice in a string-select menu.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SelectOption {
    pub(crate) label: String,
    pub(crate) value: String,
    pub(crate) description: Option<String>,
    pub(crate) default: bool,
}

/// An interactive component inside an action row.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum DiscordComponent {
    /// type 2 — a button that routes a click via its `custom_id`.
    Button {
        style: ButtonStyle,
        label: String,
        custom_id: CustomId,
        disabled: bool,
    },
    /// type 2 / style 5 — a link button that opens a URL (no `custom_id`, never
    /// dispatched back to the bot).
    LinkButton {
        label: String,
        url: String,
        disabled: bool,
    },
    /// type 3 — a string-select menu that routes a selection via its `custom_id`.
    StringSelect {
        custom_id: CustomId,
        options: Vec<SelectOption>,
        placeholder: Option<String>,
        min_values: u8,
        max_values: u8,
        disabled: bool,
    },
}

impl DiscordComponent {
    /// Serialize to Discord's component object. `None` when a routing
    /// `custom_id` can't be encoded (over Discord's 100-char limit) — the
    /// component is dropped rather than 400-ing the whole message; the row
    /// serializer logs the drop.
    fn to_api(&self) -> Option<Value> {
        match self {
            DiscordComponent::Button {
                style,
                label,
                custom_id,
                disabled,
            } => {
                let mut obj = json!({
                    "type": 2,
                    "style": style.wire(),
                    "label": label,
                    "custom_id": custom_id.encode()?,
                });
                if *disabled {
                    obj["disabled"] = json!(true);
                }
                Some(obj)
            }
            DiscordComponent::LinkButton {
                label,
                url,
                disabled,
            } => {
                let mut obj = json!({
                    "type": 2,
                    "style": 5,
                    "label": label,
                    "url": url,
                });
                if *disabled {
                    obj["disabled"] = json!(true);
                }
                Some(obj)
            }
            DiscordComponent::StringSelect {
                custom_id,
                options,
                placeholder,
                min_values,
                max_values,
                disabled,
            } => {
                let opts: Vec<Value> = options
                    .iter()
                    .map(|o| {
                        let mut ov = json!({ "label": o.label, "value": o.value });
                        if let Some(d) = &o.description {
                            ov["description"] = json!(d);
                        }
                        if o.default {
                            ov["default"] = json!(true);
                        }
                        ov
                    })
                    .collect();
                let mut obj = json!({
                    "type": 3,
                    "custom_id": custom_id.encode()?,
                    "options": opts,
                    "min_values": min_values,
                    "max_values": max_values,
                });
                if let Some(p) = placeholder {
                    obj["placeholder"] = json!(p);
                }
                if *disabled {
                    obj["disabled"] = json!(true);
                }
                Some(obj)
            }
        }
    }

    /// Whether this component occupies a whole action row (a select) vs. packing
    /// with others (a button). Used by [`action_row`] validation.
    fn is_select(&self) -> bool {
        matches!(self, DiscordComponent::StringSelect { .. })
    }
}

/// type 1 — an action row holding up to 5 buttons, or exactly one select menu.
#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct DiscordActionRow {
    pub(crate) components: Vec<DiscordComponent>,
}

impl DiscordActionRow {
    /// Serialize to `{ "type": 1, "components": [...] }`, dropping any component
    /// whose `custom_id` won't encode. `None` when the row would be empty (every
    /// component dropped, or it was built empty) so the caller omits it — Discord
    /// rejects an empty action row.
    pub(crate) fn to_api(&self) -> Option<Value> {
        let rendered: Vec<Value> = self
            .components
            .iter()
            .filter_map(|c| {
                let api = c.to_api();
                if api.is_none() {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                        "dropping component with un-encodable custom_id (over 100 chars)"
                    );
                }
                api
            })
            .collect();
        if rendered.is_empty() {
            return None;
        }
        Some(json!({ "type": 1, "components": rendered }))
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Builders — the ergonomic surface the orchestrator/agent paths use to attach
// components to a `DiscordOutgoing`.
// ─────────────────────────────────────────────────────────────────────────────

/// A button that dispatches back to the bot via `custom_id`.
pub(crate) fn button(
    style: ButtonStyle,
    label: impl Into<String>,
    custom_id: CustomId,
) -> DiscordComponent {
    DiscordComponent::Button {
        style,
        label: label.into(),
        custom_id,
        disabled: false,
    }
}

/// A link button that opens `url` (never dispatched).
pub(crate) fn link_button(label: impl Into<String>, url: impl Into<String>) -> DiscordComponent {
    DiscordComponent::LinkButton {
        label: label.into(),
        url: url.into(),
        disabled: false,
    }
}

/// A single-choice string-select menu.
pub(crate) fn string_select(
    custom_id: CustomId,
    options: Vec<SelectOption>,
    placeholder: Option<String>,
) -> DiscordComponent {
    DiscordComponent::StringSelect {
        custom_id,
        options,
        placeholder,
        min_values: 1,
        max_values: 1,
        disabled: false,
    }
}

/// Pack components into an action row, enforcing Discord's row rules: a select
/// occupies a row alone; buttons pack up to 5. Over-capacity rows are truncated
/// (with a log) rather than 400-ing the send.
pub(crate) fn action_row(components: Vec<DiscordComponent>) -> DiscordActionRow {
    let has_select = components.iter().any(DiscordComponent::is_select);
    let capped = if has_select {
        // A select must be the only component in its row.
        components.into_iter().take(1).collect()
    } else if components.len() > MAX_BUTTONS_PER_ROW {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            "action row exceeds 5 buttons; truncating"
        );
        components.into_iter().take(MAX_BUTTONS_PER_ROW).collect()
    } else {
        components
    };
    DiscordActionRow { components: capped }
}

/// Cap a set of action rows to Discord's per-message limit of 5.
pub(crate) fn cap_rows(rows: Vec<DiscordActionRow>) -> Vec<DiscordActionRow> {
    if rows.len() > MAX_ROWS_PER_MESSAGE {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            "message exceeds 5 action rows; truncating"
        );
        rows.into_iter().take(MAX_ROWS_PER_MESSAGE).collect()
    } else {
        rows
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Modals (opened via interaction response type 9; submitted as type 5)
// ─────────────────────────────────────────────────────────────────────────────

/// A modal text-input style (Discord component type-4 `style`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TextInputStyle {
    Short,
    Paragraph,
}

impl TextInputStyle {
    fn wire(self) -> u64 {
        match self {
            TextInputStyle::Short => 1,
            TextInputStyle::Paragraph => 2,
        }
    }
}

/// One text-input field in a modal (Discord component type 4). `custom_id` here
/// is the *field* id echoed back on submit — an arbitrary label, NOT a `zc1`
/// routing token (the routing token is the modal's own `custom_id`).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ModalField {
    pub(crate) custom_id: String,
    pub(crate) label: String,
    pub(crate) style: TextInputStyle,
    pub(crate) required: bool,
    pub(crate) placeholder: Option<String>,
    pub(crate) min_length: Option<u16>,
    pub(crate) max_length: Option<u16>,
}

/// A modal opened in response to a button/slash interaction (response type 9).
/// The modal's `custom_id` is the `zc1` routing token its submit (type 5)
/// dispatches on; each field becomes its own action row (Discord requires
/// exactly one text input per modal row, ≤5 rows).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DiscordModal {
    pub(crate) custom_id: CustomId,
    pub(crate) title: String,
    pub(crate) fields: Vec<ModalField>,
}

impl DiscordModal {
    /// Serialize to the `data` object of a type-9 (MODAL) interaction response.
    /// `None` when the routing `custom_id` won't encode (over 100 chars).
    pub(crate) fn to_api(&self) -> Option<Value> {
        let rows: Vec<Value> = self
            .fields
            .iter()
            .take(MAX_ROWS_PER_MESSAGE)
            .map(|f| {
                let mut input = json!({
                    "type": 4,
                    "custom_id": f.custom_id,
                    "label": f.label,
                    "style": f.style.wire(),
                    "required": f.required,
                });
                if let Some(p) = &f.placeholder {
                    input["placeholder"] = json!(p);
                }
                if let Some(m) = f.min_length {
                    input["min_length"] = json!(m);
                }
                if let Some(m) = f.max_length {
                    input["max_length"] = json!(m);
                }
                json!({ "type": 1, "components": [input] })
            })
            .collect();
        Some(json!({
            "custom_id": self.custom_id.encode()?,
            "title": self.title,
            "components": rows,
        }))
    }
}

/// Extract the submitted fields of a type-5 (modal submit) interaction payload
/// from `data.components[][]` as `(field_custom_id, value)` pairs, in order.
pub(crate) fn extract_modal_fields(d: &Value) -> Vec<(String, String)> {
    d.get("data")
        .and_then(|x| x.get("components"))
        .and_then(|c| c.as_array())
        .map(|rows| {
            rows.iter()
                .filter_map(|r| r.get("components").and_then(|c| c.as_array()))
                .flatten()
                .filter_map(|input| {
                    let id = input.get("custom_id")?.as_str()?.to_string();
                    let value = input
                        .get("value")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    Some((id, value))
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn button_row_serializes_to_discord_shape() {
        let row = action_row(vec![
            button(
                ButtonStyle::Success,
                "Approve",
                CustomId::new("approve", "i1"),
            ),
            button(ButtonStyle::Danger, "Deny", CustomId::new("deny", "i1")),
        ]);
        let api = row.to_api().unwrap();
        assert_eq!(api["type"], json!(1));
        let comps = api["components"].as_array().unwrap();
        assert_eq!(comps.len(), 2);
        assert_eq!(comps[0]["type"], json!(2));
        assert_eq!(comps[0]["style"], json!(3));
        assert_eq!(comps[0]["label"], json!("Approve"));
        assert_eq!(comps[0]["custom_id"], json!("zc1|approve|i1"));
        // disabled omitted when false
        assert!(comps[0].get("disabled").is_none());
    }

    #[test]
    fn link_button_emits_url_not_custom_id() {
        let row = action_row(vec![link_button("Docs", "https://example.com")]);
        let api = row.to_api().unwrap();
        let btn = &api["components"][0];
        assert_eq!(btn["style"], json!(5));
        assert_eq!(btn["url"], json!("https://example.com"));
        assert!(btn.get("custom_id").is_none());
    }

    #[test]
    fn select_serializes_and_takes_its_own_row() {
        let row = action_row(vec![
            string_select(
                CustomId::new("pick", "menu1"),
                vec![SelectOption {
                    label: "One".into(),
                    value: "1".into(),
                    description: Some("first".into()),
                    default: false,
                }],
                Some("Choose".into()),
            ),
            // A button alongside a select must be dropped (select owns the row).
            button(ButtonStyle::Primary, "x", CustomId::new("x", "x")),
        ]);
        assert_eq!(row.components.len(), 1, "select takes the row alone");
        let api = row.to_api().unwrap();
        assert_eq!(api["components"][0]["type"], json!(3));
        assert_eq!(api["components"][0]["placeholder"], json!("Choose"));
        assert_eq!(
            api["components"][0]["options"][0]["description"],
            json!("first")
        );
    }

    #[test]
    fn button_row_truncates_past_five() {
        let row = action_row(
            (0..7)
                .map(|i| {
                    button(
                        ButtonStyle::Secondary,
                        format!("b{i}"),
                        CustomId::new("k", i.to_string()),
                    )
                })
                .collect(),
        );
        assert_eq!(row.components.len(), 5);
    }

    #[test]
    fn empty_row_serializes_to_none() {
        assert!(DiscordActionRow::default().to_api().is_none());
    }

    #[test]
    fn component_with_unencodable_custom_id_is_dropped() {
        // A custom_id whose arg blows the 100-char limit drops just that button.
        let row = action_row(vec![
            button(ButtonStyle::Primary, "ok", CustomId::new("k", "y")),
            button(
                ButtonStyle::Primary,
                "bad",
                CustomId::new("k", "x".repeat(200)),
            ),
        ]);
        let api = row.to_api().unwrap();
        assert_eq!(api["components"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn cap_rows_limits_to_five() {
        let rows = cap_rows((0..8).map(|_| DiscordActionRow::default()).collect());
        assert_eq!(rows.len(), 5);
    }

    #[test]
    fn modal_serializes_to_type9_data() {
        let modal = DiscordModal {
            custom_id: CustomId::new("report", "i1"),
            title: "Report".into(),
            fields: vec![ModalField {
                custom_id: "reason".into(),
                label: "Reason".into(),
                style: TextInputStyle::Paragraph,
                required: true,
                placeholder: Some("why?".into()),
                min_length: None,
                max_length: Some(500),
            }],
        };
        let api = modal.to_api().unwrap();
        assert_eq!(api["custom_id"], json!("zc1|report|i1"));
        assert_eq!(api["title"], json!("Report"));
        let row = &api["components"][0];
        assert_eq!(row["type"], json!(1));
        let input = &row["components"][0];
        assert_eq!(input["type"], json!(4)); // text input
        assert_eq!(input["custom_id"], json!("reason"));
        assert_eq!(input["style"], json!(2)); // paragraph
        assert_eq!(input["max_length"], json!(500));
        assert!(input.get("min_length").is_none());
    }

    #[test]
    fn extract_modal_fields_reads_submitted_values_in_order() {
        let payload = json!({
            "data": { "custom_id": "zc1|report|i1", "components": [
                { "type": 1, "components": [{ "type": 4, "custom_id": "reason", "value": "spam" }] },
                { "type": 1, "components": [{ "type": 4, "custom_id": "detail", "value": "lots" }] },
            ]}
        });
        assert_eq!(
            extract_modal_fields(&payload),
            vec![
                ("reason".to_string(), "spam".to_string()),
                ("detail".to_string(), "lots".to_string()),
            ]
        );
        // A submit with no components yields no fields.
        assert!(extract_modal_fields(&json!({ "data": { "custom_id": "x" } })).is_empty());
    }
}

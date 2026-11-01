//! One registry owns payload reachability, erasure classification and decoding.
//! Codec names are persisted wire identifiers; changing this table must not
//! reinterpret existing bytes or turn an unknown codec into an erasable leaf.
use super::*;

type Validator = fn(&[u8]) -> Result<(), AppendError>;

#[derive(Clone, Copy)]
pub(super) enum Codec {
    ContentLeaf,
    Structure(Option<Validator>),
}

// A generic structure still receives JSON parsing and reference traversal in
// `nested_refs`. Typed structures additionally validate their registered schema.
const REGISTRY: &[(&str, Codec)] = &[
    ("session.text/v1", Codec::ContentLeaf),
    ("session.conversation/v1", Codec::ContentLeaf),
    ("session.reasoning/v1", Codec::ContentLeaf),
    ("session.bytes/v1", Codec::ContentLeaf),
    ("session.attachment-reference/v1", Codec::ContentLeaf),
    ("session.error-detail/v1", Codec::ContentLeaf),
    ("session.locator/v1", Codec::ContentLeaf),
    ("session.tool-arguments/v1", Codec::ContentLeaf),
    ("session.tool-result/v1", Codec::ContentLeaf),
    (
        "session.content/v1",
        Codec::Structure(Some(validate_content)),
    ),
    (
        "session.response/v1",
        Codec::Structure(Some(validate::<ProviderResponsePayload>)),
    ),
    (
        "session.provider-observation/v1",
        Codec::Structure(Some(validate::<ProviderObservationPayload>)),
    ),
    (
        "session.tool-observation/v1",
        Codec::Structure(Some(validate::<ToolObservationPayload>)),
    ),
    (
        "session.diagnostic/v1",
        Codec::Structure(Some(validate::<ProviderDiagnostic>)),
    ),
    (
        "session.approval-resolution/v1",
        Codec::Structure(Some(validate::<ApprovalResolution>)),
    ),
    (
        "session.approval-reply/v1",
        Codec::Structure(Some(validate_reply)),
    ),
    (
        "session.turn-completion/v1",
        Codec::Structure(Some(validate::<TurnCompletion>)),
    ),
    ("session.request/v1", Codec::Structure(None)),
    ("session.context/v1", Codec::Structure(None)),
    ("session.file-registration/v1", Codec::Structure(None)),
    ("session.ingress/v1", Codec::Structure(None)),
    ("session.execution-binding/v1", Codec::Structure(None)),
    ("session.authority/v1", Codec::Structure(None)),
    ("session.route/v1", Codec::Structure(None)),
    ("session.transform/v1", Codec::Structure(None)),
    ("session.control/v1", Codec::Structure(None)),
    ("session.evidence/v1", Codec::Structure(None)),
    ("session.tool-call/v1", Codec::Structure(None)),
];

pub(super) fn lookup(name: &str) -> Option<Codec> {
    REGISTRY
        .iter()
        .find(|(codec, _)| *codec == name)
        .map(|(_, codec)| *codec)
}

pub(super) fn content_leaf_names() -> impl Iterator<Item = &'static str> {
    REGISTRY
        .iter()
        .filter_map(|(name, codec)| matches!(codec, Codec::ContentLeaf).then_some(*name))
}

fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, AppendError> {
    serde_json::from_slice(bytes).map_err(|error| {
        AppendError::InvalidReference(format!("invalid structural payload: {error}"))
    })
}

fn validate<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<(), AppendError> {
    let _: T = decode(bytes)?;
    Ok(())
}

fn validate_content(bytes: &[u8]) -> Result<(), AppendError> {
    let content: ContentPayload = decode(bytes)?;
    MessageId::parse(content.message_id.as_str())?;
    for part in content.parts {
        if let RecordedContentPart::File { file_id } = part
            && file_id.is_empty()
        {
            return Err(AppendError::InvalidReference("empty FileId".into()));
        }
    }
    Ok(())
}

fn validate_reply(bytes: &[u8]) -> Result<(), AppendError> {
    let reply: ApprovalReplyPayload = decode(bytes)?;
    if reply.replacement_ref.as_ref().is_some_and(|reference| {
        reply.decision != ApprovalReplyDecision::Replace || reference.codec != "session.text/v1"
    }) {
        return Err(AppendError::InvalidReference(
            "invalid approval replacement reference".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registrations_are_unique_and_unknown_versions_stay_opaque() {
        let mut names = HashSet::new();
        for (name, _) in REGISTRY {
            assert!(names.insert(name), "duplicate codec {name}");
            assert!(lookup(name).is_some());
        }
        assert_eq!(content_leaf_names().count(), 9);
        for name in ["session.future/v1", "session.text/v2", ""] {
            assert!(lookup(name).is_none());
            assert!(!content_leaf(name));
            if !name.is_empty() {
                let record = PayloadRecord::json(name, &Value::Null).expect("future payload");
                let payload = StoredPayload {
                    reference: StoredPayloadRef::from(&record),
                    body: Some(record.body),
                };
                assert!(matches!(
                    nested_refs(&payload),
                    Err(AppendError::UnresolvedReachability(_))
                ));
            }
        }
    }

    #[test]
    fn registered_schemas_and_reference_traversal_keep_their_boundaries() {
        let leaf = PayloadRecord::json("session.text/v1", &"body").expect("leaf");
        let reference = StoredPayloadRef::from(&leaf);
        let body = serde_json::json!({"child": reference});
        for (name, codec) in REGISTRY {
            let record = PayloadRecord::json(name, &body).expect("record");
            let payload = StoredPayload {
                reference: StoredPayloadRef::from(&record),
                body: Some(record.body),
            };
            match codec {
                Codec::ContentLeaf => assert!(
                    nested_refs(&payload)
                        .expect("leaf remains opaque")
                        .is_empty()
                ),
                Codec::Structure(None) => assert_eq!(
                    nested_refs(&payload).expect("reference traversal"),
                    vec![reference.clone()]
                ),
                Codec::Structure(Some(_)) => assert!(
                    matches!(nested_refs(&payload), Err(AppendError::InvalidReference(_))),
                    "{name} requires its typed schema"
                ),
            }
        }
        let content = PayloadRecord::json(
            "session.content/v1",
            &ContentPayload {
                message_id: MessageId::new(),
                parts: vec![RecordedContentPart::Text {
                    text_ref: reference.clone(),
                }],
            },
        )
        .expect("content");
        assert_eq!(
            nested_refs(&StoredPayload {
                reference: StoredPayloadRef::from(&content),
                body: Some(content.body),
            })
            .expect("typed content traverses text"),
            vec![reference]
        );
    }
}

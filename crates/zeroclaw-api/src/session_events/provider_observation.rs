//! Rebuildable protocol-coordinate and component-offset admission index.
use super::index::OverlayMap;
use super::payloads::StoredPayload;
use super::*;
use crate::model_provider::ProviderContentPosition;
use std::collections::HashMap;

/// Order segments only when their coordinates establish a complete order.
/// Mixing item-level and part-level coordinates within one item is ambiguous.
pub fn order_provider_segments<T>(
    segments: &mut [T],
    position: impl Fn(&T) -> Option<&ProviderContentPosition>,
) -> bool {
    let mut shapes = HashMap::new();
    for segment in segments.iter() {
        let Some(coordinate) = position(segment) else {
            return false;
        };
        let indexed = coordinate.content_index.is_some();
        if shapes
            .insert(coordinate.output_index, indexed)
            .is_some_and(|previous| previous != indexed)
        {
            return false;
        }
    }
    segments.sort_by(|left, right| position(left).cmp(&position(right)));
    true
}

#[derive(Clone, PartialEq, Eq, Hash)]
enum BlockKey {
    Protocol(ProviderContentPosition),
    Legacy(usize),
}
type ComponentKey = (InvocationId, BlockKey, String);

#[derive(Clone)]
struct LegacyBlock {
    descriptor: StoredPayloadRef,
    finalized: bool,
}

#[derive(Clone)]
struct Segment {
    position: Option<ProviderContentPosition>,
    reference: StoredPayloadRef,
}

#[derive(Default)]
pub(super) struct ObservationIndex {
    components: HashMap<ComponentKey, u64>,
    segments: HashMap<(InvocationId, usize), Segment>,
    lengths: HashMap<InvocationId, usize>,
    legacy: HashMap<(InvocationId, usize), LegacyBlock>,
}
pub(super) struct ObservationDelta {
    components: HashMap<ComponentKey, u64>,
    segments: HashMap<(InvocationId, usize), Segment>,
    lengths: HashMap<InvocationId, usize>,
    legacy: HashMap<(InvocationId, usize), LegacyBlock>,
}

fn decode<T: serde::de::DeserializeOwned>(
    payloads: &OverlayMap<'_, PayloadId, StoredPayload>,
    reference: &StoredPayloadRef,
    codec: &str,
) -> Result<T, AppendError> {
    if reference.codec != codec {
        return Err(AppendError::InvalidReference(
            "provider observation codec mismatch".into(),
        ));
    }
    let row = payloads.get(&reference.payload_id).ok_or_else(|| {
        AppendError::InvalidReference("provider observation payload missing".into())
    })?;
    serde_json::from_slice(
        row.body
            .as_deref()
            .ok_or_else(|| AppendError::Corrupt("provider observation structure missing".into()))?,
    )
    .map_err(|error| AppendError::InvalidReference(error.to_string()))
}

impl ObservationIndex {
    pub(super) fn observe_committed(
        &mut self,
        events: &[SessionEvent],
        payloads: &HashMap<PayloadId, StoredPayload>,
    ) -> Result<(), AppendError> {
        let delta = self.plan_inner(events, &OverlayMap::new(payloads), false)?;
        self.commit(delta);
        Ok(())
    }
    pub(super) fn commit(&mut self, delta: ObservationDelta) {
        self.components.extend(delta.components);
        self.segments.extend(delta.segments);
        self.lengths.extend(delta.lengths);
        self.legacy.extend(delta.legacy);
    }
    pub(super) fn plan(
        &self,
        events: &[SessionEvent],
        payloads: &OverlayMap<'_, PayloadId, StoredPayload>,
    ) -> Result<ObservationDelta, AppendError> {
        self.plan_inner(events, payloads, true)
    }
    fn plan_inner(
        &self,
        events: &[SessionEvent],
        payloads: &OverlayMap<'_, PayloadId, StoredPayload>,
        strict: bool,
    ) -> Result<ObservationDelta, AppendError> {
        let mut components = OverlayMap::new(&self.components);
        let mut segments = OverlayMap::new(&self.segments);
        let mut lengths = OverlayMap::new(&self.lengths);
        let mut legacy = OverlayMap::new(&self.legacy);
        for event in events {
            let Some(invocation) = &event.invocation_id else {
                continue;
            };
            if let EventData::ProviderResponseProgressObserved { observation_ref } =
                &event.body.data
            {
                let observation: ProviderObservationPayload =
                    decode(payloads, observation_ref, "session.provider-observation/v1")?;
                for operation in observation.operations {
                    let (position, reference) = match operation {
                        ProviderObservation::ProtocolSegmentObserved {
                            position,
                            component: declared,
                            byte_offset,
                            segment_ref,
                        } => {
                            let block: ResponseBlock =
                                decode(payloads, &segment_ref, "session.evidence/v1")?;
                            let (kind, bytes) = block.component();
                            if declared != kind {
                                return Err(AppendError::InvalidReference(
                                    "provider component kind mismatch".into(),
                                ));
                            }
                            let key = (
                                invocation.clone(),
                                BlockKey::Protocol(position.clone()),
                                declared,
                            );
                            let expected = components.get(&key).copied().unwrap_or(0);
                            if byte_offset != expected {
                                return Err(AppendError::InvalidTransition(
                                    "provider component gap or overlap".into(),
                                ));
                            }
                            let end = expected.checked_add(bytes.byte_len).ok_or_else(|| {
                                AppendError::InvalidReference(
                                    "provider component offset overflow".into(),
                                )
                            })?;
                            components.insert(key, end);
                            (Some(position), segment_ref)
                        }
                        ProviderObservation::BlockOpened {
                            block_index,
                            descriptor_ref,
                        } => {
                            let key = (invocation.clone(), block_index);
                            if strict && legacy.get(&key).is_some() {
                                return Err(AppendError::InvalidTransition(
                                    "provider block already opened".into(),
                                ));
                            }
                            let _: ResponseBlock =
                                decode(payloads, &descriptor_ref, "session.evidence/v1")?;
                            legacy.insert(
                                key,
                                LegacyBlock {
                                    descriptor: descriptor_ref,
                                    finalized: false,
                                },
                            );
                            continue;
                        }
                        ProviderObservation::BytesAppended {
                            block_index,
                            component: declared,
                            byte_offset,
                            bytes_ref,
                        } => {
                            let key = (invocation.clone(), block_index);
                            let Some(open) = legacy.get(&key) else {
                                if strict {
                                    return Err(AppendError::InvalidTransition(
                                        "provider bytes require opened block".into(),
                                    ));
                                }
                                continue;
                            };
                            if strict && open.finalized {
                                return Err(AppendError::InvalidTransition(
                                    "provider bytes after finalized block".into(),
                                ));
                            }
                            let block: ResponseBlock =
                                decode(payloads, &open.descriptor, "session.evidence/v1")?;
                            let (kind, expected_bytes) = block.component();
                            let kind = if kind == "reasoning_finalized" {
                                "reasoning"
                            } else {
                                kind
                            };
                            if strict && (declared != kind || &bytes_ref != expected_bytes) {
                                return Err(AppendError::InvalidReference(
                                    "provider bytes do not match opened descriptor".into(),
                                ));
                            }
                            let component_key =
                                (invocation.clone(), BlockKey::Legacy(block_index), declared);
                            let expected = components.get(&component_key).copied().unwrap_or(0);
                            if strict && byte_offset != expected {
                                return Err(AppendError::InvalidTransition(
                                    "provider component gap or overlap".into(),
                                ));
                            }
                            let end =
                                byte_offset.checked_add(bytes_ref.byte_len).ok_or_else(|| {
                                    AppendError::InvalidReference(
                                        "provider component offset overflow".into(),
                                    )
                                })?;
                            components.insert(component_key, end);
                            continue;
                        }
                        ProviderObservation::BlockFinalized {
                            block_index,
                            finalized_block_ref,
                        } => {
                            let key = (invocation.clone(), block_index);
                            if let Some(open) = legacy.get_mut(&key) {
                                if strict
                                    && (open.finalized || open.descriptor != finalized_block_ref)
                                {
                                    return Err(AppendError::InvalidTransition(
                                        "provider block finalized twice or descriptor changed"
                                            .into(),
                                    ));
                                }
                                let block: ResponseBlock =
                                    decode(payloads, &finalized_block_ref, "session.evidence/v1")?;
                                let (kind, bytes) = block.component();
                                let kind = if kind == "reasoning_finalized" {
                                    "reasoning"
                                } else {
                                    kind
                                };
                                if strict
                                    && !matches!(kind, "tool_call" | "locator")
                                    && components
                                        .get(&(
                                            invocation.clone(),
                                            BlockKey::Legacy(block_index),
                                            kind.into(),
                                        ))
                                        .copied()
                                        .unwrap_or(0)
                                        != bytes.byte_len
                                {
                                    return Err(AppendError::InvalidTransition(
                                        "provider finalized component is incomplete".into(),
                                    ));
                                }
                                open.finalized = true;
                            } else if strict {
                                return Err(AppendError::InvalidTransition(
                                    "provider finalization requires opened block".into(),
                                ));
                            }
                            (None, finalized_block_ref)
                        }
                        _ => continue,
                    };
                    let count = lengths.get(invocation).copied().unwrap_or(0) + 1;
                    if count > 16_384 {
                        return Err(AppendError::InvalidReference(
                            "provider segment limit exceeded".into(),
                        ));
                    }
                    segments.insert(
                        (invocation.clone(), count),
                        Segment {
                            position,
                            reference,
                        },
                    );
                    lengths.insert(invocation.clone(), count);
                }
            }
            let response = match &event.body.data {
                EventData::ProviderAttemptCompleted { response_ref, .. } => Some(response_ref),
                EventData::ProviderAttemptFailed {
                    partial_response_ref,
                    ..
                }
                | EventData::InvocationOutcomeUnknown {
                    observed_partial_ref: partial_response_ref,
                    ..
                } => partial_response_ref.as_ref(),
                _ => None,
            };
            if strict
                && matches!(event.body.data, EventData::ProviderAttemptCompleted { .. })
                && legacy
                    .iter()
                    .any(|((owner, _), block)| owner == invocation && !block.finalized)
            {
                return Err(AppendError::InvalidTransition(
                    "provider completed with unfinalized block".into(),
                ));
            }
            if let (Some(response), Some(count)) = (
                response.filter(|reference| reference.codec == "session.response/v1"),
                lengths.get(invocation),
            ) {
                let response: ProviderResponsePayload =
                    decode(payloads, response, "session.response/v1")?;
                let mut ordered = (1..=*count)
                    .map(|ordinal| {
                        segments.get(&(invocation.clone(), ordinal)).ok_or_else(|| {
                            AppendError::Corrupt("provider observation segment index gap".into())
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let sorted =
                    order_provider_segments(&mut ordered, |segment| segment.position.as_ref());
                let positioned = ordered.iter().any(|segment| segment.position.is_some());
                let expected: Vec<ResponseBlock> = ordered
                    .iter()
                    .map(|segment| decode(payloads, &segment.reference, "session.evidence/v1"))
                    .collect::<Result<_, _>>()?;
                if (positioned && response.blocks != expected)
                    || (!positioned && !response.blocks.starts_with(&expected))
                {
                    // Older writers sorted mixed coordinate shapes. Preserve
                    // those committed facts without admitting new such output.
                    if !strict
                        && !sorted
                        && ordered.iter().all(|segment| segment.position.is_some())
                    {
                        ordered.sort_by(|left, right| left.position.cmp(&right.position));
                        let legacy_expected: Vec<ResponseBlock> = ordered
                            .iter()
                            .map(|segment| {
                                decode(payloads, &segment.reference, "session.evidence/v1")
                            })
                            .collect::<Result<_, _>>()?;
                        if response.blocks == legacy_expected {
                            continue;
                        }
                    }
                    return Err(AppendError::InvalidReference(
                        "provider response order or segment references mismatch".into(),
                    ));
                }
            }
        }
        Ok(ObservationDelta {
            components: components.into_changes(),
            segments: segments.into_changes(),
            lengths: lengths.into_changes(),
            legacy: legacy.into_changes(),
        })
    }
}

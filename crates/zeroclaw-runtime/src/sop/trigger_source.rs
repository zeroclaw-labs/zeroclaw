//! One matcher per SOP trigger source. `SopTrigger::behavior` is the only place
//! that names every variant for matching; field specs are generated separately
//! by the `TriggerFields` derive, so neither matching nor the authoring
//! registry hand-lists a source. Add a variant and the compiler forces its arm
//! here (matching) and the derive supplies its fields.

use super::engine::{
    amqp_routing_key_matches, calendar_trigger_matches, channel_trigger_topic_matches,
    filesystem_event_listed, filesystem_path_matches, mqtt_topic_matches,
};
use super::types::{FilesystemEventKind, SopEvent, SopTrigger};
use crate::sop::condition::evaluate_condition;

/// Whether a trigger fires for an event whose source class already equals the
/// trigger's own source. Impls only check their topic/payload shape.
pub(crate) trait TriggerBehavior {
    fn matches(&self, event: &SopEvent) -> bool;
}

fn condition_holds(condition: &Option<String>, event: &SopEvent) -> bool {
    match condition {
        Some(cond) => evaluate_condition(cond, event.payload.as_deref()),
        None => true,
    }
}

struct Mqtt<'a> {
    topic: &'a str,
    condition: &'a Option<String>,
}
impl TriggerBehavior for Mqtt<'_> {
    fn matches(&self, event: &SopEvent) -> bool {
        event
            .topic
            .as_deref()
            .is_some_and(|t| mqtt_topic_matches(self.topic, t))
            && condition_holds(self.condition, event)
    }
}

struct Amqp<'a> {
    routing_key: &'a str,
    condition: &'a Option<String>,
}
impl TriggerBehavior for Amqp<'_> {
    fn matches(&self, event: &SopEvent) -> bool {
        event
            .topic
            .as_deref()
            .is_some_and(|t| amqp_routing_key_matches(self.routing_key, t))
            && condition_holds(self.condition, event)
    }
}

struct Webhook<'a> {
    path: &'a str,
}
impl TriggerBehavior for Webhook<'_> {
    fn matches(&self, event: &SopEvent) -> bool {
        event.topic.as_deref().is_some_and(|t| t == self.path)
    }
}

struct Cron<'a> {
    expression: &'a str,
}
impl TriggerBehavior for Cron<'_> {
    fn matches(&self, event: &SopEvent) -> bool {
        event.topic.as_deref().is_some_and(|t| t == self.expression)
    }
}

struct Peripheral<'a> {
    board: &'a str,
    signal: &'a str,
    condition: &'a Option<String>,
}
impl TriggerBehavior for Peripheral<'_> {
    fn matches(&self, event: &SopEvent) -> bool {
        event
            .topic
            .as_deref()
            .is_some_and(|t| t == format!("{}/{}", self.board, self.signal))
            && condition_holds(self.condition, event)
    }
}

struct Filesystem<'a> {
    path: &'a str,
    events: &'a [FilesystemEventKind],
    condition: &'a Option<String>,
}
impl TriggerBehavior for Filesystem<'_> {
    fn matches(&self, event: &SopEvent) -> bool {
        let path_match = event
            .topic
            .as_deref()
            .is_some_and(|t| filesystem_path_matches(self.path, t));
        if !path_match {
            return false;
        }
        if !self.events.is_empty()
            && !filesystem_event_listed(self.events, event.payload.as_deref())
        {
            return false;
        }
        condition_holds(self.condition, event)
    }
}

struct Calendar<'a> {
    calendar_source: &'a str,
    calendar_ids: &'a [String],
    condition: &'a Option<String>,
}
impl TriggerBehavior for Calendar<'_> {
    fn matches(&self, event: &SopEvent) -> bool {
        calendar_trigger_matches(self.calendar_source, self.calendar_ids, event)
            && condition_holds(self.condition, event)
    }
}

struct Channel<'a> {
    channel: &'a str,
    alias: &'a Option<String>,
    condition: &'a Option<String>,
}
impl TriggerBehavior for Channel<'_> {
    fn matches(&self, event: &SopEvent) -> bool {
        channel_trigger_topic_matches(self.channel, self.alias.as_deref(), event.topic.as_deref())
            && condition_holds(self.condition, event)
    }
}

struct Manual;
impl TriggerBehavior for Manual {
    fn matches(&self, _event: &SopEvent) -> bool {
        true
    }
}

impl SopTrigger {
    /// Project this trigger onto its matcher. The single exhaustive match over
    /// `SopTrigger` for matching: adding a variant fails to compile here until
    /// its matcher is supplied.
    pub(crate) fn behavior(&self) -> Box<dyn TriggerBehavior + '_> {
        match self {
            Self::Mqtt { topic, condition } => Box::new(Mqtt { topic, condition }),
            Self::Amqp {
                routing_key,
                condition,
            } => Box::new(Amqp {
                routing_key,
                condition,
            }),
            Self::Webhook { path } => Box::new(Webhook { path }),
            Self::Cron { expression } => Box::new(Cron { expression }),
            Self::Peripheral {
                board,
                signal,
                condition,
            } => Box::new(Peripheral {
                board,
                signal,
                condition,
            }),
            Self::Filesystem {
                path,
                events,
                condition,
            } => Box::new(Filesystem {
                path,
                events,
                condition,
            }),
            Self::Calendar {
                calendar_source,
                calendar_ids,
                condition,
            } => Box::new(Calendar {
                calendar_source,
                calendar_ids,
                condition,
            }),
            Self::Channel {
                channel,
                alias,
                condition,
            } => Box::new(Channel {
                channel,
                alias,
                condition,
            }),
            Self::Manual => Box::new(Manual),
        }
    }
}

use zeroclaw_runtime::flow::{Localizable, Outcome};

const COMPLETED_ID: &str = "onboard-flow-completed";
const CANCELLED_ID: &str = "onboard-flow-cancelled";
const FAILED_ID: &str = "onboard-flow-failed";

#[must_use]
pub fn outcome_message(outcome: &Outcome) -> Localizable {
    match outcome {
        Outcome::Cancelled => Localizable::new(CANCELLED_ID),
        Outcome::Completed { configured } => {
            let items = configured
                .iter()
                .map(|item| format!("{}:{}", item.layer, item.instance))
                .collect::<Vec<_>>()
                .join(", ");
            Localizable::new(COMPLETED_ID).with_arg("items", items)
        }
        Outcome::Failed {
            layer,
            instance,
            reason,
        } => Localizable::new(FAILED_ID)
            .with_arg("layer", layer)
            .with_arg("instance", instance)
            .with_arg("reason", reason),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_runtime::flow::ConfiguredItem;

    #[test]
    fn completed_maps_to_its_id_with_items_arg() {
        let descriptor = outcome_message(&Outcome::Completed {
            configured: vec![ConfiguredItem {
                layer: "channel".into(),
                instance: "home".into(),
            }],
        });
        assert_eq!(descriptor.message_id, COMPLETED_ID);
        assert!(
            descriptor
                .args
                .iter()
                .any(|(name, value)| name == "items" && value.contains("channel:home"))
        );
    }

    #[test]
    fn cancelled_maps_to_its_id() {
        let descriptor = outcome_message(&Outcome::Cancelled);
        assert_eq!(descriptor.message_id, CANCELLED_ID);
        assert!(descriptor.args.is_empty());
    }

    #[test]
    fn failed_maps_to_its_id_with_named_args() {
        let descriptor = outcome_message(&Outcome::Failed {
            layer: "channel".into(),
            instance: "home".into(),
            reason: "bad token".into(),
        });
        assert_eq!(descriptor.message_id, FAILED_ID);
        assert!(
            descriptor
                .args
                .iter()
                .any(|(name, value)| name == "reason" && value == "bad token")
        );
    }
}

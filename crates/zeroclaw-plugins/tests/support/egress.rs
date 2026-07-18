use zeroclaw_plugins::egress::{EgressHostService, EgressPolicy, EgressPolicyResolver};

pub fn egress_service() -> EgressHostService {
    EgressHostService::new(EgressPolicyResolver::new(|_| {
        EgressPolicy::new([], [], [], 16)
    }))
}

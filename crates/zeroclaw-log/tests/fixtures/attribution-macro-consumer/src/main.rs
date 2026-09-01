use zeroclaw_log_attribution_macro_support::FixtureAttributable;

fn main() {
    let _span = zeroclaw_log::attribution_span!(&FixtureAttributable);
}

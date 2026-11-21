fn main() {
    print!(
        "{}",
        zeroclaw_config::migration::generate(5, &Default::default()).unwrap()
    );
}

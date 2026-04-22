use zeroclaw::config::schema::Config;

#[tokio::test]
async fn test_print_feishu_config() {
    std::env::set_var("ZEROCLAW_DIR", "/Users/mario/.zeroclaw");
    std::env::set_var("ZEROCLAW_WORKSPACE", "/Users/mario/.zeroclaw/workspace");
    let config = Config::load_or_init().await.unwrap();
    println!(
        "FEISHU IS SOME: {}",
        config.channels_config.feishu.is_some()
    );
    println!("LARK IS SOME: {}", config.channels_config.lark.is_some());
    panic!("FORCE FAIL TO SEE OUTPUT");
}

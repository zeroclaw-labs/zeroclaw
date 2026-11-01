use zeroclaw_config::schema::{Config, XmppConfig, XmppProtocol};

#[test]
fn xmpp_disabled_partial_config_is_safe_but_enabled_requires_credentials() {
    let mut config = Config::default();
    config
        .channels
        .xmpp
        .insert("private".into(), XmppConfig::default());
    assert!(config.validate().is_ok());
    assert!(!config.channels.has_any_enabled());
    config.channels.xmpp.get_mut("private").unwrap().enabled = true;
    let error = config
        .validate()
        .expect_err("enabled partial listener must fail");
    assert!(error.to_string().contains("channels.xmpp.private.username"));
}

#[test]
fn xmpp_options_round_trip_and_password_is_a_secret() {
    let text = r#"
schema_version = 3
[channels.xmpp.private]
username = "bot@example.org"
server = "xmpp.example.org"
port = 5222
protocol = "starttls"
password = "fixture-only"
omemo_enabled = true
omemo_trusted_devices = ["alice@example.org 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"]
"#;
    let config: Config = toml::from_str(text).unwrap();
    let channel = &config.channels.xmpp["private"];
    assert_eq!(channel.protocol, XmppProtocol::Starttls);
    assert_eq!(channel.port, 5222);
    assert!(channel.omemo_enabled);
    assert!(Config::prop_is_secret("channels.xmpp.private.password"));
    let restored: Config = toml::from_str(&toml::to_string(&config).unwrap()).unwrap();
    assert_eq!(
        restored.channels.xmpp["private"].omemo_trusted_devices,
        channel.omemo_trusted_devices
    );
    assert!(toml::from_str::<Config>(&text.replace("starttls", "plaintext")).is_err());
}

#[test]
fn xmpp_executable_path_is_not_a_writable_channel_property() {
    let mut config = Config::default();
    config
        .channels
        .xmpp
        .insert("private".into(), XmppConfig::default());
    assert!(
        config
            .set_prop(
                "channels.xmpp.private.python_path",
                "/tmp/untrusted-executable"
            )
            .is_err()
    );
    let text = toml::to_string(&config).unwrap();
    assert!(!text.contains("python_path"));
    let legacy: Config = toml::from_str("schema_version = 3\n[channels.xmpp.private]\npython_path = '/tmp/ignored-legacy-setting'\n").unwrap();
    assert!(!toml::to_string(&legacy).unwrap().contains("python_path"));
}

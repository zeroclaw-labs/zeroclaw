use fluent::{FluentArgs, FluentBundle, FluentResource};
use zeroclaw_config::schema::Config;

pub fn text(config: &Config, key: &str, args: &[(&str, &str)]) -> String {
    let locale = config
        .locale
        .as_deref()
        .map(normalize_locale)
        .unwrap_or("en");
    format_message(ftl_source(locale), locale, key, args)
        .or_else(|| format_message(ftl_source("en"), "en", key, args))
        .unwrap_or_else(|| format!("{{{key}}}"))
}

fn normalize_locale(locale: &str) -> &str {
    match locale {
        "es" | "es-ES" | "es_MX" | "es-MX" => "es",
        "fr" | "fr-FR" | "fr_CA" | "fr-CA" => "fr",
        "ja" | "ja-JP" => "ja",
        "zh" | "zh_CN" | "zh-CN" | "zh-Hans" => "zh-CN",
        _ => "en",
    }
}

fn ftl_source(locale: &str) -> &'static str {
    match locale {
        "es" => include_str!("../../../zeroclaw-runtime/locales/es/cli.ftl"),
        "fr" => include_str!("../../../zeroclaw-runtime/locales/fr/cli.ftl"),
        "ja" => include_str!("../../../zeroclaw-runtime/locales/ja/cli.ftl"),
        "zh-CN" => include_str!("../../../zeroclaw-runtime/locales/zh-CN/cli.ftl"),
        _ => include_str!("../../../zeroclaw-runtime/locales/en/cli.ftl"),
    }
}

fn format_message(
    ftl_source: &str,
    locale: &str,
    key: &str,
    args: &[(&str, &str)],
) -> Option<String> {
    let resource =
        FluentResource::try_new(ftl_source.to_string()).unwrap_or_else(|(resource, _)| resource);
    let language_identifier = locale.parse().unwrap_or_else(|_| "en".parse().unwrap());
    let mut bundle = FluentBundle::new(vec![language_identifier]);
    bundle.set_use_isolating(false);
    let _ = bundle.add_resource(resource);

    let message = bundle.get_message(key)?;
    let pattern = message.value()?;
    let mut fluent_args = FluentArgs::new();
    for (name, value) in args {
        fluent_args.set(*name, *value);
    }
    let mut errors = vec![];
    let value = bundle.format_pattern(pattern, Some(&fluent_args), &mut errors);
    errors.is_empty().then(|| value.into_owned())
}

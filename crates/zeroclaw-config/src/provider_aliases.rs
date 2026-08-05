//! ModelProvider alias functions used by config validation.
//! These are extracted from the model_providers module to break the circular
//! dependency between config and model_providers.

pub fn is_glm_global_alias(name: &str) -> bool {
    matches!(name, "glm" | "zhipu" | "glm-global" | "zhipu-global")
}

pub fn is_glm_cn_alias(name: &str) -> bool {
    matches!(name, "glm-cn" | "zhipu-cn" | "bigmodel")
}

pub fn is_glm_alias(name: &str) -> bool {
    is_glm_global_alias(name) || is_glm_cn_alias(name)
}

pub fn is_zai_global_alias(name: &str) -> bool {
    matches!(name, "zai" | "z.ai" | "zai-global" | "z.ai-global")
}

pub fn is_zai_cn_alias(name: &str) -> bool {
    matches!(name, "zai-cn" | "z.ai-cn")
}

pub fn is_zai_alias(name: &str) -> bool {
    is_zai_global_alias(name) || is_zai_cn_alias(name)
}

pub fn is_minimax_intl_alias(name: &str) -> bool {
    matches!(
        name,
        "minimax"
            | "minimax-intl"
            | "minimax-io"
            | "minimax-global"
            | "minimax-oauth"
            | "minimax-portal"
            | "minimax-oauth-global"
            | "minimax-portal-global"
    )
}

pub fn is_minimax_cn_alias(name: &str) -> bool {
    matches!(
        name,
        "minimax-cn" | "minimaxi" | "minimax-oauth-cn" | "minimax-portal-cn"
    )
}

pub fn is_minimax_alias(name: &str) -> bool {
    is_minimax_intl_alias(name) || is_minimax_cn_alias(name)
}

pub fn is_moonshot_intl_alias(name: &str) -> bool {
    matches!(
        name,
        "moonshot-intl" | "moonshot-global" | "kimi-intl" | "kimi-global"
    )
}

pub fn is_moonshot_cn_alias(name: &str) -> bool {
    matches!(name, "moonshot" | "kimi" | "moonshot-cn" | "kimi-cn")
}

pub fn is_moonshot_alias(name: &str) -> bool {
    is_moonshot_intl_alias(name) || is_moonshot_cn_alias(name)
}

pub fn is_qwen_cn_alias(name: &str) -> bool {
    matches!(name, "qwen" | "dashscope" | "qwen-cn" | "dashscope-cn")
}

pub fn is_qwen_intl_alias(name: &str) -> bool {
    matches!(
        name,
        "qwen-intl" | "dashscope-intl" | "qwen-international" | "dashscope-international"
    )
}

pub fn is_qwen_us_alias(name: &str) -> bool {
    matches!(name, "qwen-us" | "dashscope-us")
}

pub fn is_qwen_oauth_alias(name: &str) -> bool {
    matches!(name, "qwen-code" | "qwen-oauth" | "qwen_oauth")
}

pub fn is_bailian_alias(name: &str) -> bool {
    matches!(name, "bailian" | "aliyun-bailian" | "aliyun")
}

pub fn is_qwen_alias(name: &str) -> bool {
    is_qwen_cn_alias(name)
        || is_qwen_intl_alias(name)
        || is_qwen_us_alias(name)
        || is_qwen_oauth_alias(name)
}

pub fn is_qianfan_alias(name: &str) -> bool {
    matches!(name, "qianfan" | "baidu")
}

pub fn is_doubao_alias(name: &str) -> bool {
    matches!(name, "doubao" | "volcengine" | "ark" | "doubao-cn")
}

pub fn canonical_china_provider_name(name: &str) -> Option<&'static str> {
    if is_qwen_alias(name) {
        Some("qwen")
    } else if is_glm_alias(name) {
        Some("glm")
    } else if is_moonshot_alias(name) {
        Some("moonshot")
    } else if is_minimax_alias(name) {
        Some("minimax")
    } else if is_zai_alias(name) {
        Some("zai")
    } else if is_qianfan_alias(name) {
        Some("qianfan")
    } else if is_doubao_alias(name) {
        Some("doubao")
    } else if is_bailian_alias(name) {
        Some("bailian")
    } else {
        None
    }
}

pub fn family_honors_wire_api(family: &str) -> bool {
    matches!(family, "openai" | "llamacpp" | "custom")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glm_aliases_split_global_and_cn() {
        for name in ["glm", "zhipu", "glm-global", "zhipu-global"] {
            assert!(is_glm_global_alias(name), "{name} is a GLM global alias");
            assert!(!is_glm_cn_alias(name), "{name} is not a GLM CN alias");
            assert!(is_glm_alias(name));
        }
        for name in ["glm-cn", "zhipu-cn", "bigmodel"] {
            assert!(is_glm_cn_alias(name), "{name} is a GLM CN alias");
            assert!(
                !is_glm_global_alias(name),
                "{name} is not a GLM global alias"
            );
            assert!(is_glm_alias(name));
        }
        assert!(!is_glm_alias("gpt-4o"));
    }

    #[test]
    fn zai_aliases_split_global_and_cn() {
        for name in ["zai", "z.ai", "zai-global", "z.ai-global"] {
            assert!(is_zai_global_alias(name));
            assert!(is_zai_alias(name));
        }
        for name in ["zai-cn", "z.ai-cn"] {
            assert!(is_zai_cn_alias(name));
            assert!(is_zai_alias(name));
        }
        assert!(!is_zai_alias("openai"));
    }

    #[test]
    fn minimax_aliases_split_intl_and_cn() {
        for name in [
            "minimax",
            "minimax-intl",
            "minimax-io",
            "minimax-global",
            "minimax-oauth",
            "minimax-portal",
            "minimax-oauth-global",
            "minimax-portal-global",
        ] {
            assert!(is_minimax_intl_alias(name));
            assert!(is_minimax_alias(name));
        }
        for name in [
            "minimax-cn",
            "minimaxi",
            "minimax-oauth-cn",
            "minimax-portal-cn",
        ] {
            assert!(is_minimax_cn_alias(name));
            assert!(is_minimax_alias(name));
        }
        assert!(!is_minimax_alias("minimax-unknown"));
    }

    #[test]
    fn moonshot_aliases_split_intl_and_cn() {
        for name in [
            "moonshot-intl",
            "moonshot-global",
            "kimi-intl",
            "kimi-global",
        ] {
            assert!(is_moonshot_intl_alias(name));
            assert!(is_moonshot_alias(name));
        }
        for name in ["moonshot", "kimi", "moonshot-cn", "kimi-cn"] {
            assert!(is_moonshot_cn_alias(name));
            assert!(is_moonshot_alias(name));
        }
        assert!(!is_moonshot_alias("gemini"));
    }

    #[test]
    fn qwen_alias_family_is_union_without_bailian() {
        assert!(is_qwen_cn_alias("qwen"));
        assert!(is_qwen_cn_alias("dashscope"));
        assert!(is_qwen_intl_alias("qwen-intl"));
        assert!(is_qwen_intl_alias("dashscope-international"));
        assert!(is_qwen_us_alias("qwen-us"));
        assert!(is_qwen_oauth_alias("qwen-code"));
        for name in [
            "qwen",
            "dashscope",
            "qwen-intl",
            "qwen-us",
            "qwen-code",
            "qwen_oauth",
        ] {
            assert!(is_qwen_alias(name));
        }
        // bailian is its own family and must not fold into qwen.
        assert!(!is_qwen_alias("bailian"));
        assert!(!is_qwen_alias("aliyun"));
    }

    #[test]
    fn standalone_china_aliases_match() {
        assert!(is_qianfan_alias("qianfan"));
        assert!(is_qianfan_alias("baidu"));
        assert!(is_doubao_alias("doubao"));
        assert!(is_doubao_alias("volcengine"));
        assert!(is_doubao_alias("ark"));
        assert!(is_bailian_alias("bailian"));
        assert!(is_bailian_alias("aliyun"));
    }

    #[test]
    fn canonical_name_maps_each_china_family() {
        let cases = [
            ("qwen", "qwen"),
            ("dashscope", "qwen"),
            ("qwen-code", "qwen"),
            ("glm", "glm"),
            ("bigmodel", "glm"),
            ("moonshot", "moonshot"),
            ("kimi-global", "moonshot"),
            ("minimax", "minimax"),
            ("minimaxi", "minimax"),
            ("zai", "zai"),
            ("z.ai-cn", "zai"),
            ("qianfan", "qianfan"),
            ("baidu", "qianfan"),
            ("doubao", "doubao"),
            ("ark", "doubao"),
            ("bailian", "bailian"),
            ("aliyun", "bailian"),
        ];
        for (alias, canonical) in cases {
            assert_eq!(
                canonical_china_provider_name(alias),
                Some(canonical),
                "{alias} should canonicalize to {canonical}",
            );
        }
        assert_eq!(canonical_china_provider_name("openai"), None);
        assert_eq!(canonical_china_provider_name("anthropic"), None);
        assert_eq!(canonical_china_provider_name(""), None);
    }

    #[test]
    fn canonical_name_agrees_with_family_predicates() {
        let samples = [
            "qwen",
            "dashscope-intl",
            "glm",
            "bigmodel",
            "kimi",
            "moonshot-global",
            "minimax",
            "minimax-cn",
            "zai",
            "z.ai-global",
            "qianfan",
            "baidu",
            "doubao",
            "volcengine",
            "bailian",
            "aliyun",
        ];
        for name in samples {
            let canonical = canonical_china_provider_name(name)
                .unwrap_or_else(|| panic!("{name} should be recognized as a China provider"));
            match canonical {
                "qwen" => assert!(is_qwen_alias(name)),
                "glm" => assert!(is_glm_alias(name)),
                "moonshot" => assert!(is_moonshot_alias(name)),
                "minimax" => assert!(is_minimax_alias(name)),
                "zai" => assert!(is_zai_alias(name)),
                "qianfan" => assert!(is_qianfan_alias(name)),
                "doubao" => assert!(is_doubao_alias(name)),
                "bailian" => assert!(is_bailian_alias(name)),
                other => panic!("unexpected canonical name {other} for {name}"),
            }
        }
    }

    #[test]
    fn wire_api_honored_only_by_byo_endpoint_families() {
        for family in ["openai", "llamacpp", "custom"] {
            assert!(family_honors_wire_api(family), "{family} honors wire_api");
        }
        for family in [
            "anthropic",
            "qwen",
            "glm",
            "moonshot",
            "gemini",
            "minimax",
            "",
        ] {
            assert!(!family_honors_wire_api(family), "{family} ignores wire_api");
        }
    }
}

use crate::config::Config;
use crate::onboard::feature_packs::{feature_pack_by_id, preset_by_id};
use anyhow::{bail, Context, Result};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub const WORKSPACE_PRESET_FILE: &str = ".zeroclaw-preset.json";
pub const WORKSPACE_INTENT_CAPABILITIES_FILE: &str = ".zeroclaw-intent-capabilities.json";
pub const INTENT_CAPABILITIES_FILE_ENV: &str = "ZEROCLAW_INTENT_CAPABILITIES_FILE";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspacePresetSelection {
    pub schema_version: u8,
    pub preset_id: String,
    pub packs: Vec<String>,
    pub added_packs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PresetDocument {
    pub schema_version: u32,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub packs: Vec<String>,
    #[serde(default)]
    pub config_overrides: Value,
    #[serde(default)]
    pub metadata: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum PresetImportMode {
    Overwrite,
    Merge,
    Fill,
}

impl std::fmt::Display for PresetImportMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Overwrite => write!(f, "overwrite"),
            Self::Merge => write!(f, "merge"),
            Self::Fill => write!(f, "fill"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectionDiff {
    pub before_preset_id: Option<String>,
    pub after_preset_id: String,
    pub added_packs: Vec<String>,
    pub removed_packs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CapabilitySignal {
    pub capability_id: String,
    pub matched_terms: Vec<String>,
    pub weight: f32,
    pub suggested_add_packs: Vec<String>,
    pub suggested_remove_packs: Vec<String>,
    pub rationale: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PresetScore {
    pub preset_id: String,
    pub score: f32,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConfidenceBreakdown {
    pub base: f32,
    pub signal_bonus: f32,
    pub ranking_bonus: f32,
    pub contradiction_penalty: f32,
    pub final_score: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IntentPlan {
    pub intent: String,
    pub base_preset_id: Option<String>,
    pub add_packs: Vec<String>,
    pub remove_packs: Vec<String>,
    pub confidence: f32,
    pub reasons: Vec<String>,
    pub capability_signals: Vec<CapabilitySignal>,
    pub preset_ranking: Vec<PresetScore>,
    pub confidence_breakdown: ConfidenceBreakdown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RebuildPlan {
    pub manifest_dir: PathBuf,
    pub features: Vec<String>,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportResult {
    pub before: Option<WorkspacePresetSelection>,
    pub after: WorkspacePresetSelection,
    pub mode: PresetImportMode,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum IntentCapabilitiesMergeMode {
    #[default]
    Append,
    Replace,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IntentCapabilityRule {
    pub id: String,
    pub rationale: String,
    #[serde(default)]
    pub keywords: Vec<String>,
    #[serde(default)]
    pub add_packs: Vec<String>,
    #[serde(default)]
    pub remove_packs: Vec<String>,
    #[serde(default)]
    pub preset_biases: BTreeMap<String, f32>,
    #[serde(default = "default_base_weight")]
    pub base_weight: f32,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IntentCapabilitiesDocument {
    pub schema_version: u32,
    #[serde(default)]
    pub merge_mode: IntentCapabilitiesMergeMode,
    #[serde(default)]
    pub capabilities: Vec<IntentCapabilityRule>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ResolvedIntentCapabilities {
    pub rules: Vec<IntentCapabilityRule>,
    pub sources: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PresetValidationResult {
    pub path: String,
    pub format: String,
    pub ok: bool,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PresetValidationReport {
    pub files_checked: usize,
    pub files_failed: usize,
    pub allow_unknown_packs: bool,
    pub results: Vec<PresetValidationResult>,
}

#[derive(Debug, Clone, Copy)]
struct IntentCapability {
    id: &'static str,
    rationale: &'static str,
    keywords: &'static [&'static str],
    add_packs: &'static [&'static str],
    remove_packs: &'static [&'static str],
    preset_biases: &'static [(&'static str, f32)],
    base_weight: f32,
}

const INTENT_CAPABILITIES: &[IntentCapability] = &[
    IntentCapability {
        id: "minimal-footprint",
        rationale: "Optimize for smallest practical footprint and minimal optional surface",
        keywords: &[
            "minimal",
            "smallest",
            "lightweight",
            "tiny",
            "core only",
            "just core",
            "最小",
            "精简",
            "瘦身",
            "只要本体",
            "只保留本体",
        ],
        add_packs: &[],
        remove_packs: &[
            "browser-native",
            "probe-rs",
            "peripheral-rpi",
            "rag-pdf",
            "sandbox-landlock",
        ],
        preset_biases: &[("minimal", 1.0), ("default", 0.35)],
        base_weight: 0.62,
    },
    IntentCapability {
        id: "automation-browser",
        rationale: "Enable browser/runtime automation workflows",
        keywords: &[
            "automation",
            "automate",
            "browser",
            "scrape",
            "web flow",
            "网站",
            "浏览器",
            "自动化",
        ],
        add_packs: &["browser-native"],
        remove_packs: &[],
        preset_biases: &[("automation", 1.0), ("default", 0.45)],
        base_weight: 0.64,
    },
    IntentCapability {
        id: "embedded-debug",
        rationale: "Enable embedded debugging and probing workflows",
        keywords: &[
            "embedded",
            "hardware",
            "mcu",
            "jtag",
            "stm32",
            "probe",
            "register",
            "硬件",
            "单片机",
            "寄存器",
            "烧录",
            "调试",
        ],
        add_packs: &["hardware-core", "probe-rs"],
        remove_packs: &[],
        preset_biases: &[("hardware-lab", 1.0), ("default", 0.35)],
        base_weight: 0.70,
    },
    IntentCapability {
        id: "datasheet-rag",
        rationale: "Enable PDF/datasheet ingestion and retrieval support",
        keywords: &["pdf", "datasheet", "spec sheet", "文档", "手册"],
        add_packs: &["rag-pdf"],
        remove_packs: &[],
        preset_biases: &[
            ("hardware-lab", 0.65),
            ("automation", 0.30),
            ("default", 0.35),
        ],
        base_weight: 0.50,
    },
    IntentCapability {
        id: "security-hardening",
        rationale: "Prioritize Linux sandbox hardening for runtime safety",
        keywords: &[
            "harden",
            "sandbox",
            "secure linux",
            "landlock",
            "加固",
            "沙箱",
            "安全",
        ],
        add_packs: &["sandbox-landlock"],
        remove_packs: &[],
        preset_biases: &[("hardened-linux", 1.0), ("default", 0.30)],
        base_weight: 0.72,
    },
    IntentCapability {
        id: "raspberry-pi-gpio",
        rationale: "Enable Raspberry Pi GPIO/peripheral control",
        keywords: &["raspberry pi", "rpi", "gpio", "树莓派"],
        add_packs: &["peripheral-rpi", "hardware-core"],
        remove_packs: &[],
        preset_biases: &[("hardware-lab", 0.85), ("default", 0.30)],
        base_weight: 0.64,
    },
    IntentCapability {
        id: "update-management",
        rationale: "Enable self-update workflow/tooling",
        keywords: &["update", "self update", "upgrade", "升级", "更新"],
        add_packs: &["tools-update"],
        remove_packs: &[],
        preset_biases: &[
            ("default", 0.60),
            ("automation", 0.55),
            ("hardware-lab", 0.55),
            ("hardened-linux", 0.55),
        ],
        base_weight: 0.58,
    },
    IntentCapability {
        id: "disable-update",
        rationale: "Explicitly disable update/install flows",
        keywords: &[
            "without update",
            "no update",
            "dont update",
            "don't update",
            "不要更新",
            "不需要更新",
        ],
        add_packs: &[],
        remove_packs: &["tools-update"],
        preset_biases: &[("minimal", 0.45)],
        base_weight: 0.66,
    },
    IntentCapability {
        id: "disable-browser",
        rationale: "Explicitly disable browser automation",
        keywords: &[
            "without browser",
            "no browser",
            "不要浏览器",
            "不需要浏览器",
        ],
        add_packs: &[],
        remove_packs: &["browser-native"],
        preset_biases: &[("minimal", 0.40), ("hardware-lab", 0.25)],
        base_weight: 0.62,
    },
    IntentCapability {
        id: "disable-hardware",
        rationale: "Explicitly disable hardware probing/peripherals",
        keywords: &["without hardware", "no hardware", "不要硬件", "不需要硬件"],
        add_packs: &[],
        remove_packs: &["hardware-core", "probe-rs", "peripheral-rpi"],
        preset_biases: &[("minimal", 0.55), ("automation", 0.35)],
        base_weight: 0.64,
    },
];

pub fn workspace_preset_path(config: &Config) -> PathBuf {
    config.workspace_dir.join(WORKSPACE_PRESET_FILE)
}

pub fn workspace_intent_capabilities_path(config: &Config) -> PathBuf {
    config
        .workspace_dir
        .join(WORKSPACE_INTENT_CAPABILITIES_FILE)
}

pub fn resolve_intent_capabilities(
    config: &Config,
    extra_files: &[PathBuf],
) -> Result<ResolvedIntentCapabilities> {
    let mut rules = builtin_intent_capability_rules();
    let mut sources = vec!["builtin".to_string()];
    let mut seen_files: HashSet<PathBuf> = HashSet::new();

    let workspace_file = workspace_intent_capabilities_path(config);
    if workspace_file.exists() {
        apply_intent_capabilities_file(&workspace_file, &mut rules)?;
        sources.push(workspace_file.display().to_string());
        seen_files.insert(workspace_file);
    }

    if let Ok(raw_path) = std::env::var(INTENT_CAPABILITIES_FILE_ENV) {
        let trimmed = raw_path.trim();
        if !trimmed.is_empty() {
            let env_file = PathBuf::from(trimmed);
            if seen_files.insert(env_file.clone()) {
                apply_intent_capabilities_file(&env_file, &mut rules).with_context(|| {
                    format!("Failed to apply capabilities file from {INTENT_CAPABILITIES_FILE_ENV}")
                })?;
                sources.push(format!(
                    "{INTENT_CAPABILITIES_FILE_ENV}:{}",
                    env_file.display()
                ));
            }
        }
    }

    for path in extra_files {
        if seen_files.insert(path.clone()) {
            apply_intent_capabilities_file(path, &mut rules)?;
            sources.push(path.display().to_string());
        }
    }

    Ok(ResolvedIntentCapabilities { rules, sources })
}

pub fn default_selection() -> Result<WorkspacePresetSelection> {
    from_preset_id("default")
}

pub fn from_preset_id(preset_id: &str) -> Result<WorkspacePresetSelection> {
    let preset = preset_by_id(preset_id)
        .with_context(|| format!("Unknown preset id '{preset_id}'. Run `zeroclaw preset list`."))?;
    let mut selection = WorkspacePresetSelection {
        schema_version: 1,
        preset_id: preset.id.to_string(),
        packs: preset
            .packs
            .iter()
            .map(|pack| (*pack).to_string())
            .collect(),
        added_packs: Vec::new(),
    };
    normalize_selection(&mut selection)?;
    Ok(selection)
}

pub fn load_workspace_selection(config: &Config) -> Result<Option<WorkspacePresetSelection>> {
    let path = workspace_preset_path(config);
    if !path.exists() {
        return Ok(None);
    }

    let raw =
        fs::read_to_string(&path).with_context(|| format!("Failed to read {}", path.display()))?;
    if raw.trim().is_empty() {
        return Ok(None);
    }

    let mut selection: WorkspacePresetSelection = serde_json::from_str(&raw)
        .with_context(|| format!("Invalid JSON at {}", path.display()))?;
    normalize_selection(&mut selection)?;
    Ok(Some(selection))
}

pub fn save_workspace_selection(
    config: &Config,
    selection: &WorkspacePresetSelection,
) -> Result<PathBuf> {
    let path = workspace_preset_path(config);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut normalized = selection.clone();
    normalize_selection(&mut normalized)?;
    let payload = serde_json::to_string_pretty(&normalized)?;
    fs::write(&path, payload).with_context(|| format!("Failed to write {}", path.display()))?;
    Ok(path)
}

pub fn compose_selection(
    mut base: WorkspacePresetSelection,
    add_packs: &[String],
    remove_packs: &[String],
) -> Result<WorkspacePresetSelection> {
    for raw_pack in add_packs {
        let pack = raw_pack.trim();
        if pack.is_empty() {
            continue;
        }
        if feature_pack_by_id(pack).is_none() {
            bail!("Unknown pack id '{pack}'");
        }
        base.packs.push(pack.to_string());
    }

    let remove_set: BTreeSet<String> = remove_packs
        .iter()
        .map(|pack| pack.trim())
        .filter(|pack| !pack.is_empty())
        .map(str::to_string)
        .collect();
    if remove_set
        .iter()
        .any(|pack| feature_pack_by_id(pack).is_none())
    {
        let first_unknown = remove_set
            .iter()
            .find(|pack| feature_pack_by_id(pack).is_none())
            .cloned()
            .unwrap_or_else(|| "unknown".to_string());
        bail!("Unknown remove-pack id '{first_unknown}'");
    }

    base.packs.retain(|pack| !remove_set.contains(pack));
    normalize_selection(&mut base)?;
    Ok(base)
}

pub fn risky_pack_ids(selection: &WorkspacePresetSelection) -> Vec<String> {
    selection
        .packs
        .iter()
        .filter_map(|pack| {
            feature_pack_by_id(pack)
                .filter(|entry| entry.requires_confirmation)
                .map(|_| pack.clone())
        })
        .collect()
}

pub fn selection_diff(
    before: Option<&WorkspacePresetSelection>,
    after: &WorkspacePresetSelection,
) -> SelectionDiff {
    let before_set: BTreeSet<String> =
        before.map_or_else(BTreeSet::new, |entry| entry.packs.iter().cloned().collect());
    let after_set: BTreeSet<String> = after.packs.iter().cloned().collect();

    let added_packs = after_set
        .difference(&before_set)
        .cloned()
        .collect::<Vec<_>>();
    let removed_packs = before_set
        .difference(&after_set)
        .cloned()
        .collect::<Vec<_>>();

    SelectionDiff {
        before_preset_id: before.map(|entry| entry.preset_id.clone()),
        after_preset_id: after.preset_id.clone(),
        added_packs,
        removed_packs,
    }
}

pub fn selection_to_document(selection: &WorkspacePresetSelection) -> PresetDocument {
    PresetDocument {
        schema_version: 1,
        id: selection.preset_id.clone(),
        title: None,
        description: Some("ZeroClaw preset export".to_string()),
        packs: selection.packs.clone(),
        config_overrides: Value::Object(serde_json::Map::new()),
        metadata: serde_json::json!({
            "source": "zeroclaw",
            "type": "preset-export"
        }),
    }
}

pub fn export_document_to_path(path: &Path, document: &PresetDocument) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let payload = serde_json::to_string_pretty(document)?;
    fs::write(path, payload).with_context(|| format!("Failed to write {}", path.display()))?;
    Ok(())
}

pub fn import_selection_from_path(
    config: &Config,
    path: &Path,
    mode: PresetImportMode,
) -> Result<ImportResult> {
    let raw =
        fs::read_to_string(path).with_context(|| format!("Failed to read {}", path.display()))?;
    let payload = parse_import_payload(&raw)
        .with_context(|| format!("Unsupported preset payload format in {}", path.display()))?;

    let imported = match payload {
        ImportPayload::Document(document) => selection_from_document(document)?,
        ImportPayload::Selection(selection) => {
            let mut normalized = selection;
            normalize_selection(&mut normalized)?;
            normalized
        }
    };

    let before = load_workspace_selection(config)?;
    let after = merge_selection(before.clone(), imported, mode)?;
    Ok(ImportResult {
        before,
        after,
        mode,
    })
}

pub fn validate_preset_paths(
    paths: &[PathBuf],
    allow_unknown_packs: bool,
) -> Result<PresetValidationReport> {
    if paths.is_empty() {
        bail!("No input paths provided. Pass file or directory paths to validate.");
    }

    let files = collect_json_files(paths)?;
    if files.is_empty() {
        bail!("No JSON files found in the provided paths.");
    }

    let mut results = Vec::new();
    for file in files {
        results.push(validate_preset_file(&file, allow_unknown_packs));
    }

    let files_checked = results.len();
    let files_failed = results.iter().filter(|result| !result.ok).count();

    Ok(PresetValidationReport {
        files_checked,
        files_failed,
        allow_unknown_packs,
        results,
    })
}

pub fn plan_from_intent(intent: &str, current: Option<&WorkspacePresetSelection>) -> IntentPlan {
    let rules = builtin_intent_capability_rules();
    plan_from_intent_with_rules(intent, current, &rules)
}

pub fn plan_from_intent_with_rules(
    intent: &str,
    current: Option<&WorkspacePresetSelection>,
    rules: &[IntentCapabilityRule],
) -> IntentPlan {
    let normalized = intent.to_lowercase();
    let mut reasons = Vec::new();
    let mut add_packs = Vec::new();
    let mut remove_packs = Vec::new();
    let mut capability_signals = Vec::new();
    let mut preset_score_raw: BTreeMap<String, f32> = BTreeMap::new();
    let mut preset_reasons: BTreeMap<String, Vec<String>> = BTreeMap::new();

    for capability in rules {
        if !capability.enabled || capability.keywords.is_empty() {
            continue;
        }

        let matched_terms = match_terms(&normalized, &capability.keywords);
        if matched_terms.is_empty() {
            continue;
        }

        let base_weight = capability.base_weight.clamp(0.0, 1.0);
        let weight = (base_weight + (matched_terms.len().saturating_sub(1) as f32 * 0.08)).min(1.0);

        add_packs.extend(capability.add_packs.iter().cloned());
        remove_packs.extend(capability.remove_packs.iter().cloned());

        for (preset_id, bias) in &capability.preset_biases {
            if preset_id.trim().is_empty() || *bias <= 0.0 {
                continue;
            }
            let contribution = weight * bias;
            *preset_score_raw.entry(preset_id.clone()).or_insert(0.0) += contribution;
            preset_reasons
                .entry(preset_id.clone())
                .or_default()
                .push(format!("{} (+{:.2})", capability.id, contribution));
        }

        capability_signals.push(CapabilitySignal {
            capability_id: capability.id.clone(),
            matched_terms: matched_terms.clone(),
            weight,
            suggested_add_packs: capability.add_packs.clone(),
            suggested_remove_packs: capability.remove_packs.clone(),
            rationale: capability.rationale.clone(),
        });
        reasons.push(format!(
            "Capability '{}' matched terms [{}]",
            capability.id,
            matched_terms.join(", ")
        ));
    }

    dedup_string_list(&mut add_packs);
    dedup_string_list(&mut remove_packs);

    let remove_set: BTreeSet<String> = remove_packs.iter().cloned().collect();
    let contradiction_count = add_packs
        .iter()
        .filter(|pack| remove_set.contains((*pack).as_str()))
        .count();
    add_packs.retain(|pack| !remove_set.contains(pack));

    let mut preset_ranking = preset_score_raw
        .into_iter()
        .map(|(preset_id, raw_score)| PresetScore {
            reasons: preset_reasons.remove(&preset_id).unwrap_or_default(),
            preset_id,
            score: normalize_preset_score(raw_score),
        })
        .collect::<Vec<_>>();
    preset_ranking.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left.preset_id.cmp(&right.preset_id))
    });

    let base_preset_id = select_base_preset_id(current, &preset_ranking);

    if let Some(base) = base_preset_id.as_deref() {
        reasons.push(format!(
            "Selected base preset '{}' from capability ranking",
            base
        ));
    } else if reasons.is_empty() {
        if let Some(selection) = current {
            reasons.push(format!(
                "No strong intent keywords matched; using current preset '{}'",
                selection.preset_id
            ));
        } else {
            reasons.push("No strong intent keywords matched; using default preset".to_string());
        }
    }

    let base_confidence = if capability_signals.is_empty() {
        0.34
    } else {
        0.42
    };
    let signal_bonus = (capability_signals.len() as f32 * 0.08).min(0.32);
    let ranking_bonus = preset_ranking
        .first()
        .map(|top| (top.score * 0.20).min(0.20))
        .unwrap_or(0.0);
    let contradiction_penalty = (contradiction_count as f32 * 0.09).min(0.27);
    let final_score =
        (base_confidence + signal_bonus + ranking_bonus - contradiction_penalty).clamp(0.25, 0.96);

    let confidence_breakdown = ConfidenceBreakdown {
        base: base_confidence,
        signal_bonus,
        ranking_bonus,
        contradiction_penalty,
        final_score,
    };

    IntentPlan {
        intent: intent.to_string(),
        base_preset_id,
        add_packs,
        remove_packs,
        confidence: final_score,
        reasons,
        capability_signals,
        preset_ranking,
        confidence_breakdown,
    }
}

pub fn selection_from_plan(
    plan: &IntentPlan,
    current: Option<&WorkspacePresetSelection>,
) -> Result<WorkspacePresetSelection> {
    let base = if let Some(base_id) = plan.base_preset_id.as_deref() {
        from_preset_id(base_id)?
    } else if let Some(selection) = current {
        selection.clone()
    } else {
        default_selection()?
    };

    compose_selection(base, &plan.add_packs, &plan.remove_packs)
}

pub fn rebuild_plan_for_selection(
    selection: &WorkspacePresetSelection,
    start_dir: &Path,
) -> Result<RebuildPlan> {
    let manifest_dir = find_zeroclaw_manifest_dir(start_dir)?;
    let mut features = Vec::new();
    for pack_id in &selection.packs {
        if let Some(pack) = feature_pack_by_id(pack_id) {
            for feature in pack.cargo_features {
                features.push((*feature).to_string());
            }
        }
    }
    dedup_string_list(&mut features);
    features.sort();

    let mut args = vec![
        "install".to_string(),
        "--path".to_string(),
        ".".to_string(),
        "--force".to_string(),
        "--locked".to_string(),
        "--no-default-features".to_string(),
    ];
    if !features.is_empty() {
        args.push("--features".to_string());
        args.push(features.join(","));
    }

    Ok(RebuildPlan {
        manifest_dir,
        features,
        args,
    })
}

pub fn execute_rebuild_plan(plan: &RebuildPlan) -> Result<()> {
    let status = Command::new("cargo")
        .args(&plan.args)
        .current_dir(&plan.manifest_dir)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .context("Failed to execute cargo rebuild command")?;

    if !status.success() {
        bail!(
            "Rebuild command failed (exit: {}).",
            status
                .code()
                .map_or_else(|| "signal".to_string(), |code| code.to_string())
        );
    }

    Ok(())
}

fn parse_import_payload(raw: &str) -> Result<ImportPayload> {
    serde_json::from_str(raw).context("invalid preset JSON")
}

fn selection_from_document(document: PresetDocument) -> Result<WorkspacePresetSelection> {
    let mut selection = WorkspacePresetSelection {
        schema_version: 1,
        preset_id: document.id,
        packs: document.packs,
        added_packs: Vec::new(),
    };
    normalize_selection(&mut selection)?;
    Ok(selection)
}

fn merge_selection(
    before: Option<WorkspacePresetSelection>,
    imported: WorkspacePresetSelection,
    mode: PresetImportMode,
) -> Result<WorkspacePresetSelection> {
    let mut after = match (before, mode) {
        (None, _) => imported,
        (Some(_existing), PresetImportMode::Overwrite) => imported,
        (Some(existing), PresetImportMode::Merge) => WorkspacePresetSelection {
            schema_version: 1,
            preset_id: imported.preset_id,
            packs: union_preserve_order(existing.packs, imported.packs),
            added_packs: union_preserve_order(existing.added_packs, imported.added_packs),
        },
        (Some(existing), PresetImportMode::Fill) => WorkspacePresetSelection {
            schema_version: 1,
            preset_id: if existing.preset_id.trim().is_empty() {
                imported.preset_id
            } else {
                existing.preset_id
            },
            packs: union_preserve_order(existing.packs, imported.packs),
            added_packs: union_preserve_order(existing.added_packs, imported.added_packs),
        },
    };
    normalize_selection(&mut after)?;
    Ok(after)
}

fn normalize_selection(selection: &mut WorkspacePresetSelection) -> Result<()> {
    if selection.schema_version == 0 {
        selection.schema_version = 1;
    }

    selection.preset_id = selection.preset_id.trim().to_string();
    if selection.preset_id.is_empty() {
        selection.preset_id = "custom".to_string();
    }

    selection.packs = selection
        .packs
        .iter()
        .map(|pack| pack.trim())
        .filter(|pack| !pack.is_empty())
        .map(str::to_string)
        .collect();
    dedup_string_list(&mut selection.packs);

    if !selection.packs.iter().any(|pack| pack == "core-agent") {
        selection.packs.insert(0, "core-agent".to_string());
    }

    for pack in &selection.packs {
        if feature_pack_by_id(pack).is_none() {
            bail!("Unknown pack id '{pack}' in selection");
        }
    }

    selection.added_packs = recompute_added_packs(&selection.preset_id, &selection.packs);
    Ok(())
}

fn recompute_added_packs(preset_id: &str, packs: &[String]) -> Vec<String> {
    if let Some(preset) = preset_by_id(preset_id) {
        let base_set: BTreeSet<&str> = preset.packs.iter().copied().collect();
        packs
            .iter()
            .filter(|pack| !base_set.contains(pack.as_str()))
            .cloned()
            .collect()
    } else {
        packs.to_vec()
    }
}

fn default_true() -> bool {
    true
}

fn default_base_weight() -> f32 {
    0.50
}

fn builtin_intent_capability_rules() -> Vec<IntentCapabilityRule> {
    INTENT_CAPABILITIES
        .iter()
        .map(|capability| IntentCapabilityRule {
            id: capability.id.to_string(),
            rationale: capability.rationale.to_string(),
            keywords: capability
                .keywords
                .iter()
                .map(|keyword| (*keyword).to_string())
                .collect(),
            add_packs: capability
                .add_packs
                .iter()
                .map(|pack| (*pack).to_string())
                .collect(),
            remove_packs: capability
                .remove_packs
                .iter()
                .map(|pack| (*pack).to_string())
                .collect(),
            preset_biases: capability
                .preset_biases
                .iter()
                .map(|(preset_id, bias)| ((*preset_id).to_string(), *bias))
                .collect(),
            base_weight: capability.base_weight,
            enabled: true,
        })
        .collect()
}

fn apply_intent_capabilities_file(
    path: &Path,
    rules: &mut Vec<IntentCapabilityRule>,
) -> Result<()> {
    let raw =
        fs::read_to_string(path).with_context(|| format!("Failed to read {}", path.display()))?;
    let mut document: IntentCapabilitiesDocument = serde_json::from_str(&raw)
        .with_context(|| format!("Invalid intent capabilities JSON at {}", path.display()))?;
    if document.schema_version == 0 {
        document.schema_version = 1;
    }
    if document.schema_version < 1 {
        bail!(
            "Unsupported schema_version {} in {}",
            document.schema_version,
            path.display()
        );
    }

    merge_intent_capabilities_rules(rules, document).with_context(|| {
        format!(
            "Failed to merge intent capabilities from {}",
            path.display()
        )
    })
}

fn merge_intent_capabilities_rules(
    rules: &mut Vec<IntentCapabilityRule>,
    document: IntentCapabilitiesDocument,
) -> Result<()> {
    if document.merge_mode == IntentCapabilitiesMergeMode::Replace {
        rules.clear();
    }

    for mut capability in document.capabilities {
        normalize_intent_capability_rule(&mut capability)?;
        if let Some(existing_index) = rules
            .iter()
            .position(|existing| existing.id == capability.id)
        {
            rules[existing_index] = capability;
        } else {
            rules.push(capability);
        }
    }

    Ok(())
}

fn normalize_intent_capability_rule(rule: &mut IntentCapabilityRule) -> Result<()> {
    rule.id = rule.id.trim().to_string();
    if rule.id.is_empty() {
        bail!("Intent capability id cannot be empty");
    }
    if !is_valid_identifier(&rule.id) {
        bail!(
            "Intent capability id '{}' is invalid; use lowercase letters, digits, '-' or '_'",
            rule.id
        );
    }

    rule.rationale = rule.rationale.trim().to_string();
    if rule.rationale.is_empty() {
        rule.rationale = "No rationale provided".to_string();
    }

    rule.keywords = rule
        .keywords
        .iter()
        .map(|keyword| keyword.trim())
        .filter(|keyword| !keyword.is_empty())
        .map(|keyword| keyword.to_lowercase())
        .collect();
    dedup_string_list(&mut rule.keywords);

    rule.add_packs = rule
        .add_packs
        .iter()
        .map(|pack| pack.trim())
        .filter(|pack| !pack.is_empty())
        .map(str::to_string)
        .collect();
    dedup_string_list(&mut rule.add_packs);

    rule.remove_packs = rule
        .remove_packs
        .iter()
        .map(|pack| pack.trim())
        .filter(|pack| !pack.is_empty())
        .map(str::to_string)
        .collect();
    dedup_string_list(&mut rule.remove_packs);

    for pack in rule.add_packs.iter().chain(rule.remove_packs.iter()) {
        if feature_pack_by_id(pack).is_none() {
            bail!(
                "Intent capability '{}' references unknown pack id '{}'",
                rule.id,
                pack
            );
        }
    }

    let mut normalized_biases = BTreeMap::new();
    for (preset_id, bias) in &rule.preset_biases {
        let preset_id = preset_id.trim();
        if preset_id.is_empty() || *bias <= 0.0 {
            continue;
        }
        normalized_biases.insert(preset_id.to_string(), *bias);
    }
    rule.preset_biases = normalized_biases;

    if !(0.0..=1.0).contains(&rule.base_weight) {
        bail!(
            "Intent capability '{}' base_weight {} is out of range [0.0, 1.0]",
            rule.id,
            rule.base_weight
        );
    }

    Ok(())
}

fn is_valid_identifier(value: &str) -> bool {
    let len = value.len();
    if !(2..=64).contains(&len) {
        return false;
    }
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
        return false;
    }
    chars.all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-' || ch == '_')
}

fn collect_json_files(paths: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for path in paths {
        collect_json_files_from_path(path, &mut files)?;
    }
    files.sort();
    files.dedup();
    Ok(files)
}

fn collect_json_files_from_path(path: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    if path.is_file() {
        if path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.eq_ignore_ascii_case("json"))
            .unwrap_or(false)
        {
            files.push(path.to_path_buf());
        }
        return Ok(());
    }

    if path.is_dir() {
        for entry in fs::read_dir(path)
            .with_context(|| format!("Failed to list directory {}", path.display()))?
        {
            let entry =
                entry.with_context(|| format!("Failed to read entry under {}", path.display()))?;
            collect_json_files_from_path(&entry.path(), files)?;
        }
        return Ok(());
    }

    bail!("Path not found: {}", path.display())
}

fn validate_preset_file(path: &Path, allow_unknown_packs: bool) -> PresetValidationResult {
    let mut errors = Vec::new();
    let raw = match fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(error) => {
            return PresetValidationResult {
                path: path.display().to_string(),
                format: "unknown".to_string(),
                ok: false,
                errors: vec![format!("Failed to read file: {error}")],
            };
        }
    };

    let payload: Value = match serde_json::from_str(&raw) {
        Ok(value) => value,
        Err(error) => {
            return PresetValidationResult {
                path: path.display().to_string(),
                format: "unknown".to_string(),
                ok: false,
                errors: vec![format!("Invalid JSON: {error}")],
            };
        }
    };

    let Some(object) = payload.as_object() else {
        return PresetValidationResult {
            path: path.display().to_string(),
            format: "unknown".to_string(),
            ok: false,
            errors: vec!["Root JSON value must be an object".to_string()],
        };
    };

    let format = if object.contains_key("id") {
        "document"
    } else if object.contains_key("preset_id") {
        "selection"
    } else {
        "unknown"
    }
    .to_string();

    if let Some(schema_version) = object.get("schema_version") {
        if schema_version.as_u64().unwrap_or(0) < 1 {
            errors.push("schema_version must be an integer >= 1".to_string());
        }
    }

    match format.as_str() {
        "document" => {
            let id = object
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_string();
            if !is_valid_identifier(&id) {
                errors.push("id must match lowercase identifier format".to_string());
            }
            validate_optional_non_empty_string(object, "title", &mut errors);
            validate_optional_non_empty_string(object, "description", &mut errors);
        }
        "selection" => {
            let preset_id = object
                .get("preset_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_string();
            if preset_id.is_empty() {
                errors.push("preset_id must be a non-empty string".to_string());
            }
        }
        _ => {
            errors.push("Payload must include either 'id' or 'preset_id'".to_string());
        }
    }

    validate_packs_array(object, allow_unknown_packs, &mut errors);
    validate_optional_object(object, "config_overrides", &mut errors);
    validate_optional_object(object, "metadata", &mut errors);
    scan_for_secret_patterns(&payload, "", &mut errors);

    PresetValidationResult {
        path: path.display().to_string(),
        format,
        ok: errors.is_empty(),
        errors,
    }
}

fn validate_optional_non_empty_string(
    object: &serde_json::Map<String, Value>,
    key: &str,
    errors: &mut Vec<String>,
) {
    if let Some(value) = object.get(key) {
        if value.as_str().map(|entry| !entry.trim().is_empty()) != Some(true) {
            errors.push(format!("{key} must be a non-empty string when provided"));
        }
    }
}

fn validate_optional_object(
    object: &serde_json::Map<String, Value>,
    key: &str,
    errors: &mut Vec<String>,
) {
    if let Some(value) = object.get(key) {
        if !value.is_object() {
            errors.push(format!("{key} must be an object when provided"));
        }
    }
}

fn validate_packs_array(
    object: &serde_json::Map<String, Value>,
    allow_unknown_packs: bool,
    errors: &mut Vec<String>,
) {
    let Some(packs_value) = object.get("packs") else {
        errors.push("packs must be provided".to_string());
        return;
    };
    let Some(packs) = packs_value.as_array() else {
        errors.push("packs must be an array of strings".to_string());
        return;
    };
    if packs.is_empty() {
        errors.push("packs must contain at least one entry".to_string());
        return;
    }

    let mut seen = BTreeSet::new();
    let mut unknown = BTreeSet::new();
    for value in packs {
        let Some(pack) = value.as_str() else {
            errors.push("packs must contain only string values".to_string());
            continue;
        };
        let pack = pack.trim();
        if pack.is_empty() {
            errors.push("packs cannot contain empty strings".to_string());
            continue;
        }
        if !seen.insert(pack.to_string()) {
            errors.push(format!("packs contains duplicate id '{pack}'"));
        }
        if !allow_unknown_packs && feature_pack_by_id(pack).is_none() {
            unknown.insert(pack.to_string());
        }
    }
    if !unknown.is_empty() {
        errors.push(format!(
            "packs includes unknown IDs: {}",
            unknown.into_iter().collect::<Vec<_>>().join(", ")
        ));
    }
}

fn scan_for_secret_patterns(value: &Value, path: &str, errors: &mut Vec<String>) {
    const SECRET_KEYWORDS: &[&str] = &[
        "api_key",
        "apikey",
        "token",
        "secret",
        "password",
        "private_key",
        "access_key",
        "refresh_token",
        "authorization",
    ];

    match value {
        Value::Object(map) => {
            for (key, nested_value) in map {
                let key_path = if path.is_empty() {
                    key.to_string()
                } else {
                    format!("{path}.{key}")
                };
                let key_lower = key.to_lowercase();
                if SECRET_KEYWORDS
                    .iter()
                    .any(|needle| key_lower.contains(needle))
                {
                    errors.push(format!("{key_path}: secret-like key is not allowed"));
                }
                scan_for_secret_patterns(nested_value, &key_path, errors);
            }
        }
        Value::Array(values) => {
            for (index, nested_value) in values.iter().enumerate() {
                let next_path = format!("{path}[{index}]");
                scan_for_secret_patterns(nested_value, &next_path, errors);
            }
        }
        Value::String(text) => {
            if looks_like_secret_value(text) {
                let location = if path.is_empty() { "<root>" } else { path };
                errors.push(format!("{location}: secret-like value is not allowed"));
            }
        }
        _ => {}
    }
}

fn looks_like_secret_value(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    value.starts_with("sk-")
        || value.starts_with("ghp_")
        || value.starts_with("xoxb-")
        || value.starts_with("xoxa-")
        || value.starts_with("xoxp-")
        || value.starts_with("xoxs-")
        || value.starts_with("AKIA")
        || value.starts_with("AIza")
        || lower.starts_with("bearer ")
}

fn match_terms(haystack: &str, terms: &[String]) -> Vec<String> {
    let mut matches = BTreeSet::new();
    for term in terms {
        if haystack.contains(term.as_str()) {
            matches.insert(term.clone());
        }
    }
    matches.into_iter().collect()
}

fn normalize_preset_score(raw_score: f32) -> f32 {
    (raw_score / 2.40).clamp(0.0, 1.0)
}

fn select_base_preset_id(
    current: Option<&WorkspacePresetSelection>,
    preset_ranking: &[PresetScore],
) -> Option<String> {
    let top = preset_ranking.first()?;
    let minimum_top_score = if current.is_some() { 0.32 } else { 0.24 };
    if top.score < minimum_top_score {
        return None;
    }

    if let Some(existing) = current {
        if existing.preset_id == top.preset_id {
            return None;
        }

        let current_score = preset_ranking
            .iter()
            .find(|rank| rank.preset_id == existing.preset_id)
            .map(|rank| rank.score)
            .unwrap_or(0.0);

        if top.score >= current_score + 0.15 {
            return Some(top.preset_id.clone());
        }
        return None;
    }

    Some(top.preset_id.clone())
}

fn dedup_string_list(values: &mut Vec<String>) {
    let mut seen = HashSet::new();
    values.retain(|entry| seen.insert(entry.clone()));
}

fn union_preserve_order(left: Vec<String>, right: Vec<String>) -> Vec<String> {
    let mut merged = left;
    for value in right {
        if !merged.iter().any(|existing| existing == &value) {
            merged.push(value);
        }
    }
    merged
}

fn find_zeroclaw_manifest_dir(start_dir: &Path) -> Result<PathBuf> {
    let mut cursor = Some(start_dir);
    while let Some(dir) = cursor {
        let manifest = dir.join("Cargo.toml");
        if manifest.exists() {
            let raw = fs::read_to_string(&manifest)
                .with_context(|| format!("Failed to read {}", manifest.display()))?;
            if raw.contains("name = \"zeroclaw\"") {
                return Ok(dir.to_path_buf());
            }
        }
        cursor = dir.parent();
    }

    bail!(
        "Could not find zeroclaw Cargo.toml from '{}'. Run rebuild from a zeroclaw source checkout.",
        start_dir.display()
    );
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum ImportPayload {
    Document(PresetDocument),
    Selection(WorkspacePresetSelection),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn default_selection_contains_core_agent() {
        let selection = default_selection().unwrap();
        assert!(selection.packs.iter().any(|pack| pack == "core-agent"));
        assert_eq!(selection.preset_id, "default");
    }

    #[test]
    fn compose_selection_adds_and_removes_packs() {
        let base = from_preset_id("minimal").unwrap();
        let with_changes = compose_selection(
            base,
            &["browser-native".to_string(), "tools-update".to_string()],
            &["tools-update".to_string()],
        )
        .unwrap();

        assert!(with_changes.packs.contains(&"browser-native".to_string()));
        assert!(!with_changes.packs.contains(&"tools-update".to_string()));
    }

    #[test]
    fn merge_mode_prefers_imported_scalar() {
        let before = WorkspacePresetSelection {
            schema_version: 1,
            preset_id: "minimal".to_string(),
            packs: vec!["core-agent".to_string()],
            added_packs: vec![],
        };
        let imported = WorkspacePresetSelection {
            schema_version: 1,
            preset_id: "automation".to_string(),
            packs: vec!["core-agent".to_string(), "browser-native".to_string()],
            added_packs: vec!["browser-native".to_string()],
        };

        let merged = merge_selection(Some(before), imported, PresetImportMode::Merge).unwrap();
        assert_eq!(merged.preset_id, "automation");
        assert!(merged.packs.contains(&"browser-native".to_string()));
    }

    #[test]
    fn fill_mode_keeps_existing_scalar() {
        let before = WorkspacePresetSelection {
            schema_version: 1,
            preset_id: "minimal".to_string(),
            packs: vec!["core-agent".to_string()],
            added_packs: vec![],
        };
        let imported = WorkspacePresetSelection {
            schema_version: 1,
            preset_id: "automation".to_string(),
            packs: vec!["core-agent".to_string(), "browser-native".to_string()],
            added_packs: vec!["browser-native".to_string()],
        };

        let filled = merge_selection(Some(before), imported, PresetImportMode::Fill).unwrap();
        assert_eq!(filled.preset_id, "minimal");
        assert!(filled.packs.contains(&"browser-native".to_string()));
    }

    #[test]
    fn intent_plan_detects_hardware_and_update() {
        let plan = plan_from_intent("我要做硬件调试，也要支持自动更新", None);
        assert_eq!(plan.base_preset_id.as_deref(), Some("hardware-lab"));
        assert!(plan.add_packs.contains(&"probe-rs".to_string()));
        assert!(plan.add_packs.contains(&"tools-update".to_string()));
        assert!(plan.confidence > 0.5);
        assert!(!plan.capability_signals.is_empty());
        assert!(!plan.preset_ranking.is_empty());
    }

    #[test]
    fn intent_plan_respects_no_update() {
        let plan = plan_from_intent("automation setup but no update", None);
        assert!(plan.remove_packs.contains(&"tools-update".to_string()));
        assert!(!plan.add_packs.contains(&"tools-update".to_string()));
        assert!(plan
            .capability_signals
            .iter()
            .any(|signal| signal.capability_id == "disable-update"));
    }

    #[test]
    fn intent_plan_emits_ranked_preset_explanations() {
        let plan = plan_from_intent("need browser automation", None);
        assert_eq!(plan.base_preset_id.as_deref(), Some("automation"));
        assert!(!plan.preset_ranking.is_empty());
        assert_eq!(plan.preset_ranking[0].preset_id, "automation");
        assert!(plan.preset_ranking[0]
            .reasons
            .iter()
            .any(|reason| reason.contains("automation-browser")));
    }

    #[test]
    fn intent_plan_breakdown_matches_confidence() {
        let plan = plan_from_intent("minimal setup", None);
        assert!((plan.confidence - plan.confidence_breakdown.final_score).abs() < f32::EPSILON);
        assert!(plan.confidence_breakdown.base > 0.0);
    }

    #[test]
    fn plan_from_intent_supports_external_rule_set() {
        let rules = vec![IntentCapabilityRule {
            id: "webhook-focus".to_string(),
            rationale: "Optimize for webhook-first workflows".to_string(),
            keywords: vec!["webhook".to_string()],
            add_packs: vec!["browser-native".to_string()],
            remove_packs: vec!["tools-update".to_string()],
            preset_biases: BTreeMap::from([(String::from("automation"), 1.0)]),
            base_weight: 0.70,
            enabled: true,
        }];

        let plan = plan_from_intent_with_rules("need webhook integration", None, &rules);
        assert!(plan.add_packs.contains(&"browser-native".to_string()));
        assert!(plan.remove_packs.contains(&"tools-update".to_string()));
        assert_eq!(plan.base_preset_id.as_deref(), Some("automation"));
        assert!(plan
            .capability_signals
            .iter()
            .any(|signal| signal.capability_id == "webhook-focus"));
    }

    #[test]
    fn merge_capability_rules_replace_mode_replaces_existing() {
        let mut base = builtin_intent_capability_rules();
        let doc = IntentCapabilitiesDocument {
            schema_version: 1,
            merge_mode: IntentCapabilitiesMergeMode::Replace,
            capabilities: vec![IntentCapabilityRule {
                id: "custom-only".to_string(),
                rationale: "Custom capability".to_string(),
                keywords: vec!["custom".to_string()],
                add_packs: vec!["core-agent".to_string()],
                remove_packs: vec![],
                preset_biases: BTreeMap::new(),
                base_weight: 0.5,
                enabled: true,
            }],
        };

        merge_intent_capabilities_rules(&mut base, doc).unwrap();
        assert_eq!(base.len(), 1);
        assert_eq!(base[0].id, "custom-only");
    }

    #[test]
    fn validate_preset_paths_detects_unknown_pack_and_secret_key() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("bad-preset.json");
        fs::write(
            &path,
            r#"{
              "schema_version": 1,
              "id": "bad-preset",
              "packs": ["core-agent", "pack-does-not-exist"],
              "metadata": { "api_key": "sk-12345678901234567890" }
            }"#,
        )
        .unwrap();

        let report = validate_preset_paths(&[path], false).unwrap();
        assert_eq!(report.files_checked, 1);
        assert_eq!(report.files_failed, 1);
        assert!(report.results[0]
            .errors
            .iter()
            .any(|error| error.contains("unknown IDs")));
        assert!(report.results[0]
            .errors
            .iter()
            .any(|error| error.contains("secret-like key")));
    }

    #[test]
    fn validate_preset_paths_accepts_valid_payload() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("good-preset.json");
        fs::write(
            &path,
            r#"{
              "schema_version": 1,
              "id": "good-preset",
              "title": "Good Preset",
              "description": "Valid payload",
              "packs": ["core-agent", "browser-native"],
              "config_overrides": {},
              "metadata": { "author": "tester" }
            }"#,
        )
        .unwrap();

        let report = validate_preset_paths(&[path], false).unwrap();
        assert_eq!(report.files_checked, 1);
        assert_eq!(report.files_failed, 0);
        assert!(report.results[0].ok);
    }

    #[test]
    fn risky_pack_ids_detects_confirmation_packs() {
        let selection = WorkspacePresetSelection {
            schema_version: 1,
            preset_id: "custom".to_string(),
            packs: vec![
                "core-agent".to_string(),
                "tools-update".to_string(),
                "sandbox-landlock".to_string(),
            ],
            added_packs: vec!["tools-update".to_string(), "sandbox-landlock".to_string()],
        };
        let risky = risky_pack_ids(&selection);
        assert!(risky.contains(&"tools-update".to_string()));
        assert!(risky.contains(&"sandbox-landlock".to_string()));
    }

    #[test]
    fn rebuild_plan_collects_feature_flags() {
        let selection = WorkspacePresetSelection {
            schema_version: 1,
            preset_id: "custom".to_string(),
            packs: vec!["core-agent".to_string(), "browser-native".to_string()],
            added_packs: vec!["browser-native".to_string()],
        };

        let current_dir = std::env::current_dir().unwrap();
        let plan = rebuild_plan_for_selection(&selection, &current_dir).unwrap();
        assert!(plan.features.contains(&"browser-native".to_string()));
        assert!(plan.args.iter().any(|arg| arg == "--no-default-features"));
    }

    #[test]
    fn selection_from_plan_uses_current_when_no_base() {
        let current = WorkspacePresetSelection {
            schema_version: 1,
            preset_id: "custom".to_string(),
            packs: vec!["core-agent".to_string(), "browser-native".to_string()],
            added_packs: vec!["browser-native".to_string()],
        };
        let plan = IntentPlan {
            intent: "do something generic".to_string(),
            base_preset_id: None,
            add_packs: vec!["rag-pdf".to_string()],
            remove_packs: vec![],
            confidence: 0.4,
            reasons: vec!["fallback".to_string()],
            capability_signals: vec![],
            preset_ranking: vec![],
            confidence_breakdown: ConfidenceBreakdown {
                base: 0.4,
                signal_bonus: 0.0,
                ranking_bonus: 0.0,
                contradiction_penalty: 0.0,
                final_score: 0.4,
            },
        };

        let next = selection_from_plan(&plan, Some(&current)).unwrap();
        assert_eq!(next.preset_id, "custom");
        assert!(next.packs.contains(&"browser-native".to_string()));
        assert!(next.packs.contains(&"rag-pdf".to_string()));
    }
}

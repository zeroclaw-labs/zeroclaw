//! ZeroTrading — 量化 Skills 加载器
//!
//! 从 workspace/zerotrading/skills/ 下的四个分类目录中
//! 自动扫描、读取并按优先级排序所有 .md 技能文件。
//!
//! 加载顺序（注入优先级）:
//!   风控(0) → 因子(1) → 策略(2) → 经验(3)

use std::path::{Path, PathBuf};

use crate::zerotrading::types::{SkillCategory, TradingSkill};

/// 从指定根目录扫描并返回所有量化 Skills，按优先级排序。
///
/// # 参数
/// - `skills_root`: Skills 根目录（通常是 `workspace/zerotrading/skills/`）
///
/// # 返回
/// 按 [`SkillCategory`] 优先级升序排列（风控→因子→策略→经验）的 Skill 列表
pub fn load_trading_skills(skills_root: &Path) -> Vec<TradingSkill> {
    if !skills_root.exists() {
        tracing::debug!(
            path = %skills_root.display(),
            "zerotrading skills directory not found, skipping"
        );
        return Vec::new();
    }

    // 按优先级顺序扫描各分类目录
    let categories = [
        SkillCategory::RiskControl,
        SkillCategory::Factor,
        SkillCategory::Strategy,
        SkillCategory::Experience,
    ];

    let mut all_skills = Vec::new();

    for category in &categories {
        let dir = skills_root.join(category.dir_name());
        let skills = load_category_skills(&dir, *category);
        if !skills.is_empty() {
            tracing::debug!(
                category = category.label_en(),
                count = skills.len(),
                "zerotrading skills loaded"
            );
        }
        all_skills.extend(skills);
    }

    // 同类别内按文件名字典序排列，保证跨平台确定性
    // （已经按分类排好，内部再排一次文件名）
    all_skills
}

/// 扫描单个分类目录下的所有 .md 文件
fn load_category_skills(dir: &Path, category: SkillCategory) -> Vec<TradingSkill> {
    if !dir.exists() || !dir.is_dir() {
        return Vec::new();
    }

    let mut skills = Vec::new();

    let Ok(entries) = std::fs::read_dir(dir) else {
        tracing::warn!(path = %dir.display(), "failed to read zerotrading skills directory");
        return skills;
    };

    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
        })
        .collect();

    // 确定性排序：按文件名升序
    paths.sort();

    for path in paths {
        match load_skill_file(&path, category) {
            Ok(skill) => skills.push(skill),
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "failed to load zerotrading skill file"
                );
            }
        }
    }

    skills
}

/// 读取并解析单个 .md 文件为 [`TradingSkill`]
fn load_skill_file(path: &Path, category: SkillCategory) -> anyhow::Result<TradingSkill> {
    let content = std::fs::read_to_string(path)?;

    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();

    Ok(TradingSkill {
        name,
        category,
        content: content.trim().to_string(),
        path: path.to_path_buf(),
    })
}

/// 在 workspace 目录下定位 skills 根目录。
///
/// 优先级:
/// 1. 配置中显式指定的目录
/// 2. `{workspace}/zerotrading/skills/`
pub fn resolve_skills_dir(workspace_dir: &Path, config_override: Option<&Path>) -> PathBuf {
    if let Some(override_path) = config_override {
        if override_path.is_absolute() {
            return override_path.to_path_buf();
        }
        return workspace_dir.join(override_path);
    }
    workspace_dir.join("zerotrading").join("skills")
}

/// 将 Skills 转换为 LLM 提示词片段，按分类分块注入。
///
/// 输出格式：
/// ```text
/// ## ZeroTrading Skills
///
/// ### 🛡️ 风控 (Risk Control)
/// ---
/// #### skill_name
/// <content>
///
/// ### 📊 因子 (Factor)
/// ...
/// ```
///
/// 超过 `max_chars` 限制的 Skill 会被截断并注释。
pub fn skills_to_trading_prompt(skills: &[TradingSkill], max_chars: usize) -> String {
    if skills.is_empty() {
        return String::new();
    }

    let mut out = String::from(
        "## ZeroTrading Quantitative Skills\n\n\
         以下量化技能已按优先级预加载。\
         **风控约束不可被策略覆盖，务必严格遵守。**\
         决策时请综合所有相关技能的信号。\n\n",
    );

    let categories = [
        SkillCategory::RiskControl,
        SkillCategory::Factor,
        SkillCategory::Strategy,
        SkillCategory::Experience,
    ];

    let mut total_chars = out.len();

    for category in &categories {
        let cat_skills: Vec<&TradingSkill> =
            skills.iter().filter(|s| s.category == *category).collect();

        if cat_skills.is_empty() {
            continue;
        }

        let header = format!(
            "### {} {} ({})\n\n",
            category.icon(),
            category.dir_name(),
            category.label_en()
        );
        out.push_str(&header);
        total_chars += header.len();

        for skill in cat_skills {
            if total_chars >= max_chars {
                out.push_str("\n> ⚠️ [部分技能因 context 限制被省略]\n");
                break;
            }

            let skill_header = format!("#### {}\n\n", skill.name);
            let remaining = max_chars.saturating_sub(total_chars + skill_header.len() + 4);

            let content = if skill.content.len() <= remaining {
                skill.content.clone()
            } else {
                // 截断到字符边界
                let mut end = remaining;
                while end > 0 && !skill.content.is_char_boundary(end) {
                    end -= 1;
                }
                format!("{}…\n\n> [内容已截断]", &skill.content[..end])
            };

            out.push_str(&skill_header);
            out.push_str(&content);
            out.push_str("\n\n---\n\n");

            total_chars += skill_header.len() + content.len() + 8;
        }
    }

    out.trim_end_matches("\n---\n\n").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn make_skill_dir(tmp: &TempDir) -> PathBuf {
        let root = tmp.path().join("skills");
        for cat in &["风控", "因子", "策略", "经验"] {
            fs::create_dir_all(root.join(cat)).unwrap();
        }
        root
    }

    #[test]
    fn loads_skills_in_priority_order() {
        let tmp = TempDir::new().unwrap();
        let root = make_skill_dir(&tmp);

        fs::write(root.join("策略/btc_trend.md"), "# BTC 趋势策略").unwrap();
        fs::write(root.join("风控/max_drawdown.md"), "# 最大回撤限制 5%").unwrap();
        fs::write(root.join("因子/funding_rate.md"), "# 资金费率因子").unwrap();
        fs::write(root.join("经验/trade_philosophy.md"), "# 操盘心法").unwrap();

        let skills = load_trading_skills(&root);
        assert_eq!(skills.len(), 4);

        // 风控排第一
        assert_eq!(skills[0].category, SkillCategory::RiskControl);
        assert_eq!(skills[1].category, SkillCategory::Factor);
        assert_eq!(skills[2].category, SkillCategory::Strategy);
        assert_eq!(skills[3].category, SkillCategory::Experience);
    }

    #[test]
    fn empty_dir_returns_empty() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("nonexistent");
        let skills = load_trading_skills(&root);
        assert!(skills.is_empty());
    }

    #[test]
    fn ignores_non_md_files() {
        let tmp = TempDir::new().unwrap();
        let root = make_skill_dir(&tmp);

        fs::write(root.join("策略/notes.txt"), "some text").unwrap();
        fs::write(root.join("策略/real_skill.md"), "# Real").unwrap();

        let skills = load_trading_skills(&root);
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "real_skill");
    }

    #[test]
    fn prompt_includes_all_categories() {
        let skills = vec![
            TradingSkill {
                name: "test_risk".into(),
                category: SkillCategory::RiskControl,
                content: "Max drawdown: 5%".into(),
                path: "/tmp/test.md".into(),
            },
            TradingSkill {
                name: "test_strat".into(),
                category: SkillCategory::Strategy,
                content: "Buy on dip".into(),
                path: "/tmp/test2.md".into(),
            },
        ];

        let prompt = skills_to_trading_prompt(&skills, 100_000);
        assert!(prompt.contains("ZeroTrading Quantitative Skills"));
        assert!(prompt.contains("风控"));
        assert!(prompt.contains("策略"));
        assert!(prompt.contains("test_risk"));
        assert!(prompt.contains("test_strat"));
    }

    #[test]
    fn prompt_truncates_on_limit() {
        let large_content = "x".repeat(10_000);
        let skills = vec![TradingSkill {
            name: "big_skill".into(),
            category: SkillCategory::Strategy,
            content: large_content,
            path: "/tmp/big.md".into(),
        }];

        let prompt = skills_to_trading_prompt(&skills, 500);
        assert!(prompt.len() <= 600); // 留一定余量给头部和标记
    }

    #[test]
    fn resolve_skills_dir_uses_override() {
        let ws = Path::new("/workspace");
        let ovr = Path::new("/custom/skills");
        let result = resolve_skills_dir(ws, Some(ovr));
        assert_eq!(result, Path::new("/custom/skills"));
    }

    #[test]
    fn resolve_skills_dir_defaults_to_workspace() {
        let ws = Path::new("/workspace");
        let result = resolve_skills_dir(ws, None);
        assert_eq!(result, Path::new("/workspace/zerotrading/skills"));
    }
}

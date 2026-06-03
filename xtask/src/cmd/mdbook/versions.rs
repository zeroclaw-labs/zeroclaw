use serde_json::json;
use std::env;
use std::fs;

// Simple struct to represent parsed semver for sorting
#[derive(Debug, PartialEq, Eq)]
struct Version {
    major: u32,
    minor: u32,
    patch: u32,
    pre: Option<String>,
    tag: String,
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.major
            .cmp(&other.major)
            .then(self.minor.cmp(&other.minor))
            .then(self.patch.cmp(&other.patch))
            .then_with(|| match (&self.pre, &other.pre) {
                // No pre-release is greater than pre-release
                (None, None) => std::cmp::Ordering::Equal,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (Some(_), None) => std::cmp::Ordering::Less,
                (Some(a), Some(b)) => a.cmp(b),
            })
    }
}

fn parse_version(tag: &str) -> Option<Version> {
    if !tag.starts_with('v') {
        return None;
    }
    let rest = &tag[1..];
    let (base, pre) = match rest.find('-') {
        Some(idx) => (&rest[..idx], Some(rest[idx + 1..].to_string())),
        None => (rest, None),
    };

    let parts: Vec<&str> = base.split('.').collect();
    if parts.len() != 3 {
        return None;
    }

    let major = parts[0].parse().ok()?;
    let minor = parts[1].parse().ok()?;
    let patch = parts[2].parse().ok()?;

    Some(Version {
        major,
        minor,
        patch,
        pre,
        tag: tag.to_string(),
    })
}

pub fn run() -> anyhow::Result<()> {
    let min_version_env = env::var("DOCS_MIN_VERSION").unwrap_or_default();
    let min_parsed = if min_version_env.is_empty() {
        None
    } else {
        parse_version(&min_version_env)
    };

    let mut dirs = Vec::new();
    if let Ok(entries) = fs::read_dir(".") {
        for entry in entries.flatten() {
            if let Ok(file_type) = entry.file_type() {
                if !file_type.is_dir() {
                    continue;
                }
                let name = entry.file_name().to_string_lossy().to_string();
                if name == "master" || name == "stable" {
                    dirs.push(name);
                } else if parse_version(&name).is_some() {
                    // Check floor
                    if let Some(min) = &min_parsed
                        && let Some(parsed) = parse_version(&name)
                        && parsed < *min
                    {
                        continue;
                    }
                    dirs.push(name);
                }
            }
        }
    }

    // Sort: master, then stable, then tagged versions newest-first
    dirs.sort_by(|a, b| {
        if a == "master" && b != "master" {
            return std::cmp::Ordering::Less;
        }
        if b == "master" && a != "master" {
            return std::cmp::Ordering::Greater;
        }
        if a == "stable" && b != "stable" {
            return std::cmp::Ordering::Less;
        }
        if b == "stable" && a != "stable" {
            return std::cmp::Ordering::Greater;
        }

        let ver_a = parse_version(a);
        let ver_b = parse_version(b);
        match (ver_a, ver_b) {
            (Some(va), Some(vb)) => vb.cmp(&va), // Newest first
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.cmp(b),
        }
    });

    let mut versions = Vec::new();
    let mut stable_tag = None;

    for tag in &dirs {
        let label = match tag.as_str() {
            "master" => "Development (master)".to_string(),
            "stable" => {
                stable_tag = Some(tag.clone());
                "Stable".to_string()
            }
            _ => tag.clone(),
        };
        versions.push(json!({
            "tag": tag,
            "label": label,
        }));
    }

    if stable_tag.is_none() {
        // Find latest stable version (no pre-release)
        for tag in dirs.iter().rev() {
            if let Some(v) = parse_version(tag)
                && v.pre.is_none()
            {
                stable_tag = Some(tag.clone());
                break;
            }
        }
    }

    let output = json!({
        "stable": stable_tag,
        "versions": versions,
    });

    println!("{}", serde_json::to_string_pretty(&output)?);

    Ok(())
}

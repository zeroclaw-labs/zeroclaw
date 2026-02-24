use anyhow::{bail, Result};
use std::collections::BTreeSet;

const BANKING_DOMAINS: &[&str] = &[
    "*.chase.com",
    "*.bankofamerica.com",
    "*.wellsfargo.com",
    "*.fidelity.com",
    "*.schwab.com",
    "*.venmo.com",
    "*.paypal.com",
    "*.robinhood.com",
    "*.coinbase.com",
];

const MEDICAL_DOMAINS: &[&str] = &[
    "*.mychart.com",
    "*.epic.com",
    "*.patient.portal.*",
    "*.healthrecords.*",
];

const GOVERNMENT_DOMAINS: &[&str] = &["*.ssa.gov", "*.irs.gov", "*.login.gov", "*.id.me"];

const IDENTITY_PROVIDER_DOMAINS: &[&str] = &[
    "accounts.google.com",
    "login.microsoftonline.com",
    "appleid.apple.com",
];

const DOMAIN_CATEGORIES: &[(&str, &[&str])] = &[
    ("banking", BANKING_DOMAINS),
    ("medical", MEDICAL_DOMAINS),
    ("government", GOVERNMENT_DOMAINS),
    ("identity_providers", IDENTITY_PROVIDER_DOMAINS),
];

#[derive(Debug, Clone, Default)]
pub struct DomainMatcher {
    patterns: Vec<String>,
}

impl DomainMatcher {
    pub fn new(gated_domains: &[String], categories: &[String]) -> Result<Self> {
        let mut set = BTreeSet::new();

        for domain in gated_domains {
            set.insert(normalize_pattern(domain)?);
        }

        for domain in Self::expand_categories(categories)? {
            set.insert(domain);
        }

        Ok(Self {
            patterns: set.into_iter().collect(),
        })
    }

    pub fn patterns(&self) -> &[String] {
        &self.patterns
    }

    pub fn is_gated(&self, domain: &str) -> bool {
        let Some(normalized_domain) = normalize_domain(domain) else {
            return false;
        };

        self.patterns
            .iter()
            .any(|pattern| domain_matches_pattern(pattern, &normalized_domain))
    }

    pub fn expand_categories(categories: &[String]) -> Result<Vec<String>> {
        let mut expanded = Vec::new();
        for category in categories {
            let normalized = category.trim().to_ascii_lowercase();
            let Some((_, domains)) = DOMAIN_CATEGORIES
                .iter()
                .find(|(name, _)| *name == normalized.as_str())
            else {
                let known = DOMAIN_CATEGORIES
                    .iter()
                    .map(|(name, _)| *name)
                    .collect::<Vec<_>>()
                    .join(", ");
                bail!("Unknown OTP domain category '{category}'. Known categories: {known}");
            };
            expanded.extend(domains.iter().map(|domain| (*domain).to_string()));
        }
        Ok(expanded)
    }

    pub fn validate_pattern(pattern: &str) -> Result<()> {
        let _ = normalize_pattern(pattern)?;
        Ok(())
    }
}

fn normalize_domain(raw: &str) -> Option<String> {
    let mut domain = raw.trim().to_ascii_lowercase();
    if domain.is_empty() {
        return None;
    }

    if let Some((_, rest)) = domain.split_once("://") {
        domain = rest.to_string();
    }

    domain = domain
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default()
        .to_string();
    if let Some((_, host)) = domain.rsplit_once('@') {
        domain = host.to_string();
    }
    if let Some((host, _port)) = domain.split_once(':') {
        domain = host.to_string();
    }
    domain = domain.trim_end_matches('.').to_string();

    if domain.is_empty() {
        None
    } else {
        Some(domain)
    }
}

fn normalize_pattern(raw: &str) -> Result<String> {
    let pattern = raw.trim().to_ascii_lowercase();
    if pattern.is_empty() {
        bail!("Domain pattern must not be empty");
    }
    if pattern == "*" {
        return Ok(pattern);
    }
    if pattern.starts_with('.') || pattern.ends_with('.') {
        bail!("Domain pattern '{raw}' must not start or end with '.'");
    }
    if pattern.contains("..") {
        bail!("Domain pattern '{raw}' must not contain consecutive dots");
    }
    if pattern.contains("**") {
        bail!("Domain pattern '{raw}' must not contain consecutive '*'");
    }
    if !pattern
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '-' || c == '*')
    {
        bail!(
            "Domain pattern '{raw}' contains invalid characters; allowed: a-z, 0-9, '.', '-', '*'"
        );
    }
    if pattern.split('.').any(|label| label.is_empty()) {
        bail!("Domain pattern '{raw}' contains an empty label");
    }
    if pattern.starts_with("*.") && pattern.len() <= 2 {
        bail!("Domain pattern '{raw}' is incomplete");
    }
    Ok(pattern)
}

fn domain_matches_pattern(pattern: &str, domain: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if !pattern.contains('*') {
        return pattern == domain;
    }
    wildcard_match(pattern.as_bytes(), domain.as_bytes())
}

fn wildcard_match(pattern: &[u8], value: &[u8]) -> bool {
    let mut p = 0usize;
    let mut v = 0usize;
    let mut star_idx: Option<usize> = None;
    let mut match_idx = 0usize;

    while v < value.len() {
        if p < pattern.len() && pattern[p] == value[v] {
            p += 1;
            v += 1;
            continue;
        }

        if p < pattern.len() && pattern[p] == b'*' {
            star_idx = Some(p);
            p += 1;
            match_idx = v;
            continue;
        }

        if let Some(star) = star_idx {
            p = star + 1;
            match_idx += 1;
            v = match_idx;
            continue;
        }

        return false;
    }

    while p < pattern.len() && pattern[p] == b'*' {
        p += 1;
    }
    p == pattern.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_match_works() {
        let matcher =
            DomainMatcher::new(&["accounts.google.com".to_string()], &[] as &[String]).unwrap();
        assert!(matcher.is_gated("accounts.google.com"));
        assert!(matcher.is_gated("https://accounts.google.com/login"));
        assert!(!matcher.is_gated("mail.google.com"));
    }

    #[test]
    fn wildcard_match_works() {
        let matcher = DomainMatcher::new(&["*.chase.com".to_string()], &[] as &[String]).unwrap();
        assert!(matcher.is_gated("www.chase.com"));
        assert!(matcher.is_gated("secure.chase.com"));
        assert!(!matcher.is_gated("chase.com"));
    }

    #[test]
    fn category_preset_expands_and_matches() {
        let matcher = DomainMatcher::new(&[] as &[String], &["banking".to_string()]).unwrap();
        assert!(matcher.is_gated("login.paypal.com"));
        assert!(matcher.is_gated("api.coinbase.com"));
        assert!(!matcher.is_gated("developer.mozilla.org"));
    }

    #[test]
    fn non_matching_domain_returns_false() {
        let matcher =
            DomainMatcher::new(&["accounts.google.com".to_string()], &[] as &[String]).unwrap();
        assert!(!matcher.is_gated("example.com"));
    }

    #[test]
    fn malformed_domain_pattern_is_rejected() {
        let err = DomainMatcher::new(&["bad domain.com".to_string()], &[] as &[String])
            .expect_err("expected invalid pattern");
        assert!(err.to_string().contains("invalid characters"));
    }

    #[test]
    fn unknown_category_is_rejected() {
        let err = DomainMatcher::new(&[] as &[String], &["unknown".to_string()])
            .expect_err("expected unknown category rejection");
        assert!(err.to_string().contains("Unknown OTP domain category"));
    }
}

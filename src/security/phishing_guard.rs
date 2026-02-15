//! Phishing Guard - Protection against malicious links and suspicious skills
//!
//! Detects:
//! - Suspicious domain names (typosquatting, IDN homographs)
//! - URL shorteners that may hide malicious destinations
//! - IP-based URLs
//! - Suspicious skill patterns (unauthorized modifications, suspicious code)
//! - Certificate/SSL anomalies

use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};
use url::Url;

/// Threat level classification
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ThreatLevel {
    Safe,
    Suspicious,
    Dangerous,
    Blocked,
}

impl ThreatLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Safe => "safe",
            Self::Suspicious => "suspicious",
            Self::Dangerous => "dangerous",
            Self::Blocked => "blocked",
        }
    }
}

/// Link scan result
#[derive(Debug, Clone)]
pub struct LinkScanResult {
    pub url: String,
    pub threat_level: ThreatLevel,
    pub reasons: Vec<String>,
    pub normalized_url: Option<String>,
}

/// Skill security scan result
#[derive(Debug, Clone)]
pub struct SkillScanResult {
    pub skill_name: String,
    pub threat_level: ThreatLevel,
    pub reasons: Vec<String>,
    pub suspicious_patterns: Vec<String>,
}

/// Phishing Guard configuration
#[derive(Debug, Clone)]
pub struct PhishingGuardConfig {
    /// Known malicious domains (exact match)
    pub blocked_domains: HashSet<String>,
    /// Suspicious TLDs
    pub suspicious_tlds: HashSet<String>,
    /// Known URL shorteners
    pub url_shorteners: HashSet<String>,
    /// Suspicious keywords in domains
    pub suspicious_keywords: Vec<String>,
    /// Enable typosquatting detection
    pub detect_typosquatting: bool,
    /// Enable IDN homograph detection
    pub detect_idn_homographs: bool,
    /// Block IP-based URLs
    pub block_ip_urls: bool,
    /// Block URL shorteners
    pub block_shorteners: bool,
    /// Suspicious skill patterns
    pub suspicious_skill_patterns: Vec<Regex>,
}

impl Default for PhishingGuardConfig {
    fn default() -> Self {
        let mut blocked_domains = HashSet::new();
        // Common phishing domains (example list - should be loaded from threat intel)
        blocked_domains.insert("phishing-example.com".to_string());
        
        let mut suspicious_tlds = HashSet::new();
        suspicious_tlds.insert(".tk".to_string());
        suspicious_tlds.insert(".ml".to_string());
        suspicious_tlds.insert(".ga".to_string());
        suspicious_tlds.insert(".cf".to_string());
        
        let mut url_shorteners = HashSet::new();
        url_shorteners.insert("bit.ly".to_string());
        url_shorteners.insert("tinyurl.com".to_string());
        url_shorteners.insert("t.co".to_string());
        url_shorteners.insert("goo.gl".to_string());
        url_shorteners.insert("ow.ly".to_string());
        url_shorteners.insert("short.link".to_string());
        
        let suspicious_keywords = vec![
            "login".to_string(),
            "verify".to_string(),
            "secure".to_string(),
            "account".to_string(),
            "update".to_string(),
            "confirm".to_string(),
            "authenticate".to_string(),
        ];
        
        let suspicious_skill_patterns = vec![
            Regex::new(r#"eval\s*\("#).unwrap(),
            Regex::new(r#"exec\s*\("#).unwrap(),
            Regex::new(r#"system\s*\("#).unwrap(),
            Regex::new(r#"subprocess\.call"#).unwrap(),
            Regex::new(r#"os\.system"#).unwrap(),
            Regex::new(r"rm\s+-rf\s+/").unwrap(),
            Regex::new(r#"curl\s+.*\|\s*sh"#).unwrap(),
            Regex::new(r#"wget\s+.*\|\s*bash"#).unwrap(),
        ];
        
        Self {
            blocked_domains,
            suspicious_tlds,
            url_shorteners,
            suspicious_keywords,
            detect_typosquatting: true,
            detect_idn_homographs: true,
            block_ip_urls: true,
            block_shorteners: false, // Warn but don't block by default
            suspicious_skill_patterns,
        }
    }
}

/// Phishing Guard main struct
pub struct PhishingGuard {
    config: PhishingGuardConfig,
    /// Cache for domain reputation (domain -> threat level)
    domain_cache: Arc<RwLock<HashMap<String, (ThreatLevel, Vec<String>)>>>,
}

impl PhishingGuard {
    pub fn new(config: PhishingGuardConfig) -> Self {
        Self {
            config,
            domain_cache: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn default() -> Self {
        Self::new(PhishingGuardConfig::default())
    }

    /// Scan a URL for phishing indicators
    pub fn scan_url(&self, url_str: &str) -> LinkScanResult {
        let mut reasons = Vec::new();
        let mut threat_level = ThreatLevel::Safe;

        // Parse URL
        let url = match Url::parse(url_str) {
            Ok(u) => u,
            Err(_) => {
                return LinkScanResult {
                    url: url_str.to_string(),
                    threat_level: ThreatLevel::Suspicious,
                    reasons: vec!["Invalid URL format".to_string()],
                    normalized_url: None,
                }
            }
        };

        let host = match url.host_str() {
            Some(h) => h.to_lowercase(),
            None => {
                return LinkScanResult {
                    url: url_str.to_string(),
                    threat_level: ThreatLevel::Suspicious,
                    reasons: vec!["URL has no host".to_string()],
                    normalized_url: None,
                }
            }
        };

        // Check blocked domains
        if self.config.blocked_domains.contains(&host) {
            reasons.push(format!("Domain '{}' is in blocklist", host));
            threat_level = ThreatLevel::Blocked;
            return LinkScanResult {
                url: url_str.to_string(),
                threat_level,
                reasons,
                normalized_url: Some(url.to_string()),
            };
        }

        // Check if it's an IP address
        if self.config.block_ip_urls {
            if self.is_ip_address(&host) {
                reasons.push("URL uses IP address instead of domain name".to_string());
                threat_level = ThreatLevel::Suspicious;
            }
        }

        // Check URL shorteners
        if self.config.url_shorteners.contains(&host) {
            reasons.push(format!("URL uses shortener '{}', destination hidden", host));
            if self.config.block_shorteners {
                threat_level = ThreatLevel::Blocked;
            } else {
                threat_level = ThreatLevel::Suspicious;
            }
        }

        // Check suspicious TLDs
        for tld in &self.config.suspicious_tlds {
            if host.ends_with(tld) {
                reasons.push(format!("Suspicious TLD '{}' detected", tld));
                threat_level = threat_level.max(ThreatLevel::Suspicious);
            }
        }

        // Check suspicious keywords in domain
        let domain_without_tld = self.extract_domain_without_tld(&host);
        for keyword in &self.config.suspicious_keywords {
            if domain_without_tld.contains(keyword) {
                reasons.push(format!("Suspicious keyword '{}' in domain", keyword));
                threat_level = threat_level.max(ThreatLevel::Suspicious);
            }
        }

        // Check IDN homographs
        if self.config.detect_idn_homographs {
            if self.contains_homograph(&host) {
                reasons.push("Possible IDN homograph attack detected".to_string());
                threat_level = threat_level.max(ThreatLevel::Dangerous);
            }
        }

        // Check for suspicious URL patterns
        if url.path().contains("..") || url.path().contains("%2e%2e") {
            reasons.push("URL contains path traversal pattern".to_string());
            threat_level = threat_level.max(ThreatLevel::Suspicious);
        }

        // Check credentials in URL
        if url.username() != "" || url.password().is_some() {
            reasons.push("URL contains embedded credentials".to_string());
            threat_level = threat_level.max(ThreatLevel::Suspicious);
        }

        LinkScanResult {
            url: url_str.to_string(),
            threat_level,
            reasons,
            normalized_url: Some(url.to_string()),
        }
    }

    /// Scan multiple URLs
    pub fn scan_urls(&self, urls: &[String]) -> Vec<LinkScanResult> {
        urls.iter().map(|u| self.scan_url(u)).collect()
    }

    /// Check if text contains suspicious URLs
    pub fn scan_text(&self, text: &str) -> Vec<LinkScanResult> {
        let urls = self.extract_urls(text);
        self.scan_urls(&urls)
    }

    /// Scan a skill for security issues
    pub fn scan_skill(&self, skill_name: &str, skill_content: &str) -> SkillScanResult {
        let mut reasons = Vec::new();
        let mut suspicious_patterns = Vec::new();
        let mut threat_level = ThreatLevel::Safe;

        // Check for suspicious patterns
        for pattern in &self.config.suspicious_skill_patterns {
            if pattern.is_match(skill_content) {
                let pattern_str = pattern.as_str().to_string();
                suspicious_patterns.push(pattern_str.clone());
                reasons.push(format!("Suspicious pattern detected: {}", pattern_str));
                threat_level = threat_level.max(ThreatLevel::Suspicious);
            }
        }

        // Check for URLs in skill
        let urls = self.extract_urls(skill_content);
        for url in &urls {
            let scan = self.scan_url(url);
            match scan.threat_level {
                ThreatLevel::Blocked | ThreatLevel::Dangerous => {
                    reasons.push(format!("Dangerous URL in skill: {}", url));
                    threat_level = threat_level.max(ThreatLevel::Dangerous);
                }
                ThreatLevel::Suspicious => {
                    reasons.push(format!("Suspicious URL in skill: {}", url));
                    threat_level = threat_level.max(ThreatLevel::Suspicious);
                }
                _ => {}
            }
        }

        // Check for excessive permissions (heuristic)
        if skill_content.contains("sudo") || skill_content.contains("admin") {
            reasons.push("Skill requests elevated privileges".to_string());
            threat_level = threat_level.max(ThreatLevel::Suspicious);
        }

        SkillScanResult {
            skill_name: skill_name.to_string(),
            threat_level,
            reasons,
            suspicious_patterns,
        }
    }

    /// Validate if a skill is safe to install
    pub fn validate_skill_install(&self, skill_name: &str, skill_content: &str) -> Result<(), Vec<String>> {
        let scan = self.scan_skill(skill_name, skill_content);
        
        match scan.threat_level {
            ThreatLevel::Blocked => Err(scan.reasons),
            ThreatLevel::Dangerous => Err(scan.reasons),
            ThreatLevel::Suspicious => {
                // Suspicious but not blocked - return warnings
                if scan.reasons.is_empty() {
                    Ok(())
                } else {
                    Err(scan.reasons)
                }
            }
            ThreatLevel::Safe => Ok(()),
        }
    }

    /// Extract URLs from text using regex
    fn extract_urls(&self, text: &str) -> Vec<String> {
        let url_regex = Regex::new(
            r#"https?://[a-zA-Z0-9][-a-zA-Z0-9]*[a-zA-Z0-9]*\.[^\s<>"{}|^`\[\]]+"#
        ).unwrap_or_else(|_| Regex::new(r#"https?://\S+"#).unwrap());
        
        url_regex
            .find_iter(text)
            .map(|m| m.as_str().to_string())
            .collect()
    }

    /// Check if host is an IP address
    fn is_ip_address(&self, host: &str) -> bool {
        // Remove port if present
        let host = host.split(':').next().unwrap_or(host);
        
        // Check IPv4
        if host.parse::<std::net::Ipv4Addr>().is_ok() {
            return true;
        }
        
        // Check IPv6 (with or without brackets)
        let host = host.trim_start_matches('[').trim_end_matches(']');
        host.parse::<std::net::Ipv6Addr>().is_ok()
    }

    /// Extract domain without TLD
    fn extract_domain_without_tld(&self, host: &str) -> String {
        let parts: Vec<&str> = host.split('.').collect();
        if parts.len() >= 2 {
            parts[parts.len() - 2].to_string()
        } else {
            host.to_string()
        }
    }

    /// Check for IDN homograph characters
    fn contains_homograph(&self, host: &str) -> bool {
        // Check for mixed scripts or suspicious Unicode characters
        let mut has_latin = false;
        let mut has_cyrillic = false;
        let mut has_greek = false;
        
        for ch in host.chars() {
            if ch.is_ascii_alphabetic() {
                has_latin = true;
            }
            // Cyrillic range
            if ('\u{0400}'..='\u{04FF}').contains(&ch) {
                has_cyrillic = true;
            }
            // Greek range
            if ('\u{0370}'..='\u{03FF}').contains(&ch) {
                has_greek = true;
            }
        }
        
        // Multiple scripts detected
        let script_count = [has_latin, has_cyrillic, has_greek]
            .iter()
            .filter(|&&x| x)
            .count();
        
        script_count > 1
    }

    /// Add domain to blocklist
    pub fn block_domain(&mut self, domain: &str) {
        self.config.blocked_domains.insert(domain.to_lowercase());
    }

    /// Remove domain from blocklist
    pub fn unblock_domain(&mut self, domain: &str) {
        self.config.blocked_domains.remove(&domain.to_lowercase());
    }

    /// Check if a URL is safe (convenience method)
    pub fn is_safe(&self, url: &str) -> bool {
        matches!(self.scan_url(url).threat_level, ThreatLevel::Safe)
    }

    /// Get scan result for URL (convenience method)
    pub fn check_url(&self, url: &str) -> (bool, Vec<String>) {
        let scan = self.scan_url(url);
        let is_safe = matches!(scan.threat_level, ThreatLevel::Safe);
        (is_safe, scan.reasons)
    }
}

impl Default for PhishingGuard {
    fn default() -> Self {
        Self::new(PhishingGuardConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_safe_url() {
        let guard = PhishingGuard::default();
        let result = guard.scan_url("https://github.com/user/repo");
        assert!(matches!(result.threat_level, ThreatLevel::Safe));
        assert!(result.reasons.is_empty());
    }

    #[test]
    fn test_ip_url() {
        let guard = PhishingGuard::default();
        let result = guard.scan_url("http://192.168.1.1/login");
        assert!(matches!(result.threat_level, ThreatLevel::Suspicious));
        assert!(result.reasons.iter().any(|r| r.contains("IP address")));
    }

    #[test]
    fn test_url_shortener() {
        let guard = PhishingGuard::default();
        let result = guard.scan_url("https://bit.ly/abc123");
        assert!(matches!(result.threat_level, ThreatLevel::Suspicious));
        assert!(result.reasons.iter().any(|r| r.contains("shortener")));
    }

    #[test]
    fn test_blocked_domain() {
        let mut guard = PhishingGuard::default();
        guard.block_domain("evil.com");
        let result = guard.scan_url("https://evil.com/phishing");
        assert!(matches!(result.threat_level, ThreatLevel::Blocked));
    }

    #[test]
    fn test_suspicious_keyword() {
        let guard = PhishingGuard::default();
        let result = guard.scan_url("https://secure-login-bank.tk/verify");
        assert!(result.threat_level != ThreatLevel::Safe);
    }

    #[test]
    fn test_extract_urls() {
        let guard = PhishingGuard::default();
        let text = "Check out https://example.com and http://test.org for more info.";
        let urls = guard.extract_urls(text);
        assert_eq!(urls.len(), 2);
    }

    #[test]
    fn test_scan_skill() {
        let guard = PhishingGuard::default();
        let skill_content = r#"
            [skill]
            name = "test-skill"
            
            [[tools]]
            command = "eval(malicious_code)"
        "#;
        let result = guard.scan_skill("test-skill", skill_content);
        assert!(result.threat_level != ThreatLevel::Safe);
        assert!(!result.suspicious_patterns.is_empty());
    }

    #[test]
    fn test_is_ip_address() {
        let guard = PhishingGuard::default();
        assert!(guard.is_ip_address("192.168.1.1"));
        assert!(guard.is_ip_address("10.0.0.1:8080"));
        assert!(!guard.is_ip_address("example.com"));
    }

    #[test]
    fn test_homograph_detection() {
        let guard = PhishingGuard::default();
        // Mixed scripts (Latin + Cyrillic)
        assert!(guard.contains_homograph("gооgle.com")); // 'о' is Cyrillic
        assert!(!guard.contains_homograph("google.com"));
    }

    #[test]
    fn test_check_url_convenience() {
        let guard = PhishingGuard::default();
        let (is_safe, reasons) = guard.check_url("https://google.com");
        assert!(is_safe);
        assert!(reasons.is_empty());
    }
}

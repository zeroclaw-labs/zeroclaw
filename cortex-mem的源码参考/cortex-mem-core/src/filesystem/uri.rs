use crate::{Dimension, Error, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Cortex URI representing a memory resource
///
/// Simplified URI structure:
///
/// ```text
/// cortex://
/// ├── resources/{resource_name}/
/// ├── user/preferences/{name}.md
/// ├── user/entities/{name}.md
/// ├── user/events/{name}.md
/// ├── agent/cases/{name}.md
/// ├── agent/skills/{name}.md
/// └── session/{session_id}/timeline/{date}/{time}.md
/// ```
///
/// Key changes from v1:
/// - No user_id/agent_id in URI (tenant isolation at storage level)
/// - user dimension: preferences/entities/events categories
/// - agent dimension: cases/skills categories
#[derive(Debug, Clone, PartialEq)]
pub struct CortexUri {
    pub dimension: Dimension,
    pub category: String,
    pub subcategory: Option<String>,
    pub resource: Option<String>,
    pub params: HashMap<String, String>,
}

impl CortexUri {
    /// Create a new CortexUri
    pub fn new(dimension: Dimension) -> Self {
        Self {
            dimension,
            category: String::new(),
            subcategory: None,
            resource: None,
            params: HashMap::new(),
        }
    }

    /// Create user preferences URI
    pub fn user_preferences(name: &str) -> String {
        format!("cortex://user/preferences/{}.md", name)
    }

    /// Create user entities URI
    pub fn user_entities(name: &str) -> String {
        format!("cortex://user/entities/{}.md", name)
    }

    /// Create user events URI
    pub fn user_events(name: &str) -> String {
        format!("cortex://user/events/{}.md", name)
    }

    /// Create agent cases URI
    pub fn agent_cases(name: &str) -> String {
        format!("cortex://agent/cases/{}.md", name)
    }

    /// Create agent skills URI
    pub fn agent_skills(name: &str) -> String {
        format!("cortex://agent/skills/{}.md", name)
    }

    /// Create session URI
    pub fn session(session_id: &str) -> String {
        format!("cortex://session/{}", session_id)
    }

    /// Create session timeline URI
    pub fn session_timeline(session_id: &str, _date: &str, time: &str) -> String {
        format!("cortex://session/{}/timeline/{}.md", session_id, time)
    }

    /// Convert URI to file system path
    pub fn to_file_path(&self, root: &Path) -> PathBuf {
        let mut path = root.to_path_buf();

        // Add dimension
        path.push(self.dimension.as_str());

        // Add category
        if !self.category.is_empty() {
            path.push(&self.category);
        }

        // Add subcategory
        if let Some(ref sub) = self.subcategory {
            path.push(sub);
        }

        // Add resource
        if let Some(ref res) = self.resource {
            path.push(res);
        }

        path
    }

    /// Get directory URI (without resource)
    pub fn directory_uri(&self) -> String {
        let mut uri = format!("cortex://{}", self.dimension.as_str());

        if !self.category.is_empty() {
            uri.push('/');
            uri.push_str(&self.category);
        }

        if let Some(ref sub) = self.subcategory {
            uri.push('/');
            uri.push_str(sub);
        }

        uri
    }

    /// Convert to full URI string
    pub fn to_uri_string(&self) -> String {
        let mut uri = format!("cortex://{}", self.dimension.as_str());

        if !self.category.is_empty() {
            uri.push('/');
            uri.push_str(&self.category);
        }

        if let Some(ref sub) = self.subcategory {
            uri.push('/');
            uri.push_str(sub);
        }

        if let Some(ref res) = self.resource {
            uri.push('/');
            uri.push_str(res);
        }

        if !self.params.is_empty() {
            uri.push('?');
            let params: Vec<String> = self
                .params
                .iter()
                .map(|(k, v)| format!("{}={}", k, v))
                .collect();
            uri.push_str(&params.join("&"));
        }

        uri
    }
}

/// URI Parser for cortex:// protocol
pub struct UriParser;

impl UriParser {
    /// Parse a cortex:// URI string
    ///
    /// # Examples
    ///
    /// ```
    /// use cortex_mem_core::filesystem::UriParser;
    ///
    /// let uri = UriParser::parse("cortex://session/abc123/timeline").unwrap();
    /// assert_eq!(uri.dimension, cortex_mem_core::Dimension::Session);
    /// assert_eq!(uri.category, "abc123");
    /// ```
    pub fn parse(uri: &str) -> Result<CortexUri> {
        // 1. Validate scheme
        if !uri.starts_with("cortex://") {
            return Err(Error::InvalidScheme);
        }

        // 2. Split path and query
        let uri_without_scheme = &uri[9..]; // Skip "cortex://"
        let (path_part, query_part) = uri_without_scheme
            .split_once('?')
            .map(|(p, q)| (p, Some(q)))
            .unwrap_or((uri_without_scheme, None));

        // 3. Parse path
        let parts: Vec<&str> = path_part.split('/').filter(|s| !s.is_empty()).collect();

        // Allow dimension-only URIs (e.g., "cortex://session")
        if parts.is_empty() {
            return Err(Error::InvalidPath);
        }

        let dimension = Dimension::from_str(parts[0])
            .ok_or_else(|| Error::InvalidDimension(parts[0].to_string()))?;

        // Parse category, subcategory, and resource
        // New structure: cortex://{dimension}/{category}/{subcategory}/{resource}
        let category = parts.get(1).map(|s| s.to_string()).unwrap_or_default();

        let (subcategory, resource) = if parts.len() <= 2 {
            (None, None)
        } else if parts.len() == 3 {
            // If last part has extension, it's a file (resource)
            if parts[2].contains('.') {
                (None, Some(parts[2].to_string()))
            } else {
                (Some(parts[2].to_string()), None)
            }
        } else {
            // parts.len() > 3: category/subcategory/resource/...
            let sub = parts[2].to_string();
            let res = parts[3..].join("/");
            (Some(sub), Some(res))
        };

        // 4. Parse query params
        let params = Self::parse_query_params(query_part);

        Ok(CortexUri {
            dimension,
            category,
            subcategory,
            resource,
            params,
        })
    }

    fn parse_query_params(query: Option<&str>) -> HashMap<String, String> {
        query
            .map(|q| {
                q.split('&')
                    .filter_map(|pair| {
                        let mut parts = pair.split('=');
                        Some((parts.next()?.to_string(), parts.next()?.to_string()))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

//! Filesystem URI tests

#[cfg(test)]
mod tests {
    use super::super::{CortexUri, UriParser};
    use crate::Dimension;

    #[test]
    fn test_parse_user_preferences() {
        let uri_str = "cortex://user/preferences/language.md";
        let uri = UriParser::parse(uri_str).unwrap();
        
        assert!(matches!(uri.dimension, Dimension::User));
        assert_eq!(uri.category, "preferences");
        assert_eq!(uri.resource, Some("language.md".to_string()));
    }

    #[test]
    fn test_parse_agent_cases() {
        let uri_str = "cortex://agent/cases/case_001.md";
        let uri = UriParser::parse(uri_str).unwrap();
        
        assert!(matches!(uri.dimension, Dimension::Agent));
        assert_eq!(uri.category, "cases");
        assert_eq!(uri.resource, Some("case_001.md".to_string()));
    }

    #[test]
    fn test_parse_session() {
        let uri_str = "cortex://session/abc123/timeline";
        let uri = UriParser::parse(uri_str).unwrap();
        
        assert!(matches!(uri.dimension, Dimension::Session));
        assert_eq!(uri.category, "abc123");
        assert_eq!(uri.subcategory, Some("timeline".to_string()));
    }

    #[test]
    fn test_parse_resources() {
        let uri_str = "cortex://resources/my_project/docs/readme.md";
        let uri = UriParser::parse(uri_str).unwrap();
        
        assert!(matches!(uri.dimension, Dimension::Resources));
        assert_eq!(uri.category, "my_project");
        assert_eq!(uri.subcategory, Some("docs".to_string()));
        assert_eq!(uri.resource, Some("readme.md".to_string()));
    }

    #[test]
    fn test_to_uri_string() {
        let uri = CortexUri::new(Dimension::User);
        assert_eq!(uri.to_uri_string(), "cortex://user");
    }

    #[test]
    fn test_directory_uri() {
        let uri_str = "cortex://user/preferences/language.md";
        let uri = UriParser::parse(uri_str).unwrap();
        assert_eq!(uri.directory_uri(), "cortex://user/preferences");
    }

    #[test]
    fn test_invalid_scheme() {
        let result = UriParser::parse("http://example.com");
        assert!(result.is_err());
    }

    #[test]
    fn test_invalid_dimension() {
        let result = UriParser::parse("cortex://invalid/path");
        assert!(result.is_err());
    }

    #[test]
    fn test_helper_methods() {
        assert_eq!(
            CortexUri::user_preferences("language"),
            "cortex://user/preferences/language.md"
        );
        assert_eq!(
            CortexUri::user_entities("project_alpha"),
            "cortex://user/entities/project_alpha.md"
        );
        assert_eq!(
            CortexUri::user_events("decision_001"),
            "cortex://user/events/decision_001.md"
        );
        assert_eq!(
            CortexUri::agent_cases("bug_fix_001"),
            "cortex://agent/cases/bug_fix_001.md"
        );
        assert_eq!(
            CortexUri::agent_skills("search_code"),
            "cortex://agent/skills/search_code.md"
        );
        assert_eq!(
            CortexUri::session("abc123"),
            "cortex://session/abc123"
        );
    }
}

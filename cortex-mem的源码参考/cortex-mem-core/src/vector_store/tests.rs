//! Vector store tests

#[cfg(test)]
mod tests {
    use super::super::{uri_to_vector_id, parse_vector_id};
    use crate::types::ContextLayer;

    /// Check if a string is a valid UUID format (8-4-4-4-12 hex digits)
    fn is_valid_uuid_format(s: &str) -> bool {
        let parts: Vec<&str> = s.split('-').collect();
        if parts.len() != 5 {
            return false;
        }
        parts[0].len() == 8 && parts[1].len() == 4 && 
        parts[2].len() == 4 && parts[3].len() == 4 && parts[4].len() == 12 &&
        s.chars().filter(|&c| c != '-').all(|c| c.is_ascii_hexdigit())
    }

    #[test]
    fn test_uri_to_vector_id_l0() {
        let uri = "cortex://session/abc/timeline.md";
        let id = uri_to_vector_id(uri, ContextLayer::L0Abstract);
        assert!(is_valid_uuid_format(&id), "ID should be valid UUID format, got: {}", id);
        // Verify deterministic - same input produces same ID
        let id2 = uri_to_vector_id(uri, ContextLayer::L0Abstract);
        assert_eq!(id, id2, "ID should be deterministic");
    }

    #[test]
    fn test_uri_to_vector_id_l1() {
        let uri = "cortex://user/preferences/language.md";
        let id = uri_to_vector_id(uri, ContextLayer::L1Overview);
        assert!(is_valid_uuid_format(&id), "ID should be valid UUID format, got: {}", id);
        // Verify deterministic
        let id2 = uri_to_vector_id(uri, ContextLayer::L1Overview);
        assert_eq!(id, id2, "ID should be deterministic");
    }

    #[test]
    fn test_uri_to_vector_id_l2() {
        let uri = "cortex://agent/cases/case_001.md";
        let id = uri_to_vector_id(uri, ContextLayer::L2Detail);
        assert!(is_valid_uuid_format(&id), "ID should be valid UUID format, got: {}", id);
        // Verify deterministic
        let id2 = uri_to_vector_id(uri, ContextLayer::L2Detail);
        assert_eq!(id, id2, "ID should be deterministic");
    }

    #[test]
    fn test_different_layers_produce_different_ids() {
        let uri = "cortex://session/test/message.md";
        let id_l0 = uri_to_vector_id(uri, ContextLayer::L0Abstract);
        let id_l1 = uri_to_vector_id(uri, ContextLayer::L1Overview);
        let id_l2 = uri_to_vector_id(uri, ContextLayer::L2Detail);
        
        // Different layers should produce different IDs
        assert_ne!(id_l0, id_l1, "L0 and L1 should have different IDs");
        assert_ne!(id_l1, id_l2, "L1 and L2 should have different IDs");
        assert_ne!(id_l0, id_l2, "L0 and L2 should have different IDs");
    }

    #[test]
    fn test_different_uris_produce_different_ids() {
        let uri1 = "cortex://session/test/message1.md";
        let uri2 = "cortex://session/test/message2.md";
        
        let id1 = uri_to_vector_id(uri1, ContextLayer::L2Detail);
        let id2 = uri_to_vector_id(uri2, ContextLayer::L2Detail);
        
        assert_ne!(id1, id2, "Different URIs should produce different IDs");
    }

    #[test]
    fn test_parse_vector_id_with_layer_suffix() {
        // Test parsing legacy IDs that contain layer suffix (for backward compatibility)
        let id = "cortex://session/abc/timeline.md#/L0";
        let (uri, layer) = parse_vector_id(id);
        assert_eq!(uri, "cortex://session/abc/timeline.md");
        assert!(matches!(layer, ContextLayer::L0Abstract));
    }

    #[test]
    fn test_parse_vector_id_l1() {
        let id = "cortex://user/preferences/language.md#/L1";
        let (uri, layer) = parse_vector_id(id);
        assert_eq!(uri, "cortex://user/preferences/language.md");
        assert!(matches!(layer, ContextLayer::L1Overview));
    }

    #[test]
    fn test_parse_vector_id_l2() {
        let id = "cortex://agent/cases/case_001.md";
        let (uri, layer) = parse_vector_id(id);
        assert_eq!(uri, "cortex://agent/cases/case_001.md");
        assert!(matches!(layer, ContextLayer::L2Detail));
    }

    #[test]
    fn test_parse_vector_id_uuid_format() {
        // parse_vector_id should return UUID as-is (cannot reverse hash)
        let uuid_id = "547e892d-584e-279b-b987-414299aa3dcf";
        let (result, layer) = parse_vector_id(uuid_id);
        assert_eq!(result, uuid_id);
        assert!(matches!(layer, ContextLayer::L2Detail));
    }
}

#[allow(unused_imports)]
pub use zeroclaw_runtime::rag::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_pin_aliases_key_value() {
        let md = r#"## Pin Aliases
red_led: 13
builtin_led: 13
user_led: 5"#;
        let a = parse_pin_aliases(md);
        assert_eq!(a.get("red_led"), Some(&13));
        assert_eq!(a.get("builtin_led"), Some(&13));
        assert_eq!(a.get("user_led"), Some(&5));
    }

    #[test]
    fn parse_pin_aliases_table() {
        let md = r#"## Pin Aliases
| alias | pin |
|-------|-----|
| red_led | 13 |
| builtin_led | 13 |"#;
        let a = parse_pin_aliases(md);
        assert_eq!(a.get("red_led"), Some(&13));
        assert_eq!(a.get("builtin_led"), Some(&13));
    }

    #[test]
    fn parse_pin_aliases_empty() {
        let a = parse_pin_aliases("No aliases here");
        assert!(a.is_empty());
    }

    #[test]
    fn infer_board_from_path_nucleo() {
        let base = std::path::Path::new("/base");
        let path = std::path::Path::new("/base/nucleo-f401re.md");
        assert_eq!(
            infer_board_from_path(path, base),
            Some("nucleo-f401re".into())
        );
    }

    #[test]
    fn infer_board_generic_none() {
        let base = std::path::Path::new("/base");
        let path = std::path::Path::new("/base/generic.md");
        assert_eq!(infer_board_from_path(path, base), None);
    }

    #[test]
    fn hardware_rag_load_and_retrieve() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("datasheets");
        std::fs::create_dir_all(&base).unwrap();
        let content = r#"# Test Board
## Pin Aliases
red_led: 13
## GPIO
Pin 13: LED
"#;
        std::fs::write(base.join("test-board.md"), content).unwrap();

        let rag = HardwareRag::load(tmp.path(), "datasheets").unwrap();
        assert!(!rag.is_empty());
        let boards = vec!["test-board".to_string()];
        let chunks = rag.retrieve("led", &boards, 5);
        assert!(!chunks.is_empty());
        let ctx = rag.pin_alias_context("red led", &boards);
        assert!(ctx.contains("13"));
    }

    #[test]
    fn hardware_rag_load_empty_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("empty_ds");
        std::fs::create_dir_all(&base).unwrap();
        let rag = HardwareRag::load(tmp.path(), "empty_ds").unwrap();
        assert!(rag.is_empty());
    }
}

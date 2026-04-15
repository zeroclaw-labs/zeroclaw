// Seed Categories — 9 hardcoded, immutable main categories (v3.0)
//
// These categories are baked into the binary and cannot be deleted or renamed.
// Users can create Custom Categories on top of these (see `user_categories` table).

use serde::{Deserialize, Serialize};

/// The 9 immutable seed categories, in display order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SeedCategory {
    Daily,
    Shopping,
    Document,
    Coding,
    Interpret,
    Phone,
    Image,
    Music,
    Video,
}

/// A seed category entry with key, localized name, and icon.
#[derive(Debug, Clone)]
pub struct SeedCategoryInfo {
    pub key: &'static str,
    pub name_ko: &'static str,
    pub icon: &'static str,
    pub index: u8,
}

/// All 9 seed categories in canonical display order.
pub const SEED_CATEGORIES: &[SeedCategoryInfo] = &[
    SeedCategoryInfo { key: "daily",     name_ko: "일상업무", icon: "\u{1F3E0}", index: 1 }, // 🏠
    SeedCategoryInfo { key: "shopping",  name_ko: "쇼핑",     icon: "\u{1F6D2}", index: 2 }, // 🛒
    SeedCategoryInfo { key: "document",  name_ko: "문서작업", icon: "\u{1F4DD}", index: 3 }, // 📝
    SeedCategoryInfo { key: "coding",    name_ko: "코딩작업", icon: "\u{1F4BB}", index: 4 }, // 💻
    SeedCategoryInfo { key: "interpret", name_ko: "통역",     icon: "\u{1F399}", index: 5 }, // 🎙️
    SeedCategoryInfo { key: "phone",     name_ko: "전화비서", icon: "\u{260E}",  index: 6 }, // ☎️
    SeedCategoryInfo { key: "image",     name_ko: "이미지",   icon: "\u{1F3A8}", index: 7 }, // 🎨
    SeedCategoryInfo { key: "music",     name_ko: "음악",     icon: "\u{1F3B5}", index: 8 }, // 🎵
    SeedCategoryInfo { key: "video",     name_ko: "동영상",   icon: "\u{1F3AC}", index: 9 }, // 🎬
];

impl SeedCategory {
    /// Get the string key for this category.
    pub fn key(self) -> &'static str {
        match self {
            Self::Daily => "daily",
            Self::Shopping => "shopping",
            Self::Document => "document",
            Self::Coding => "coding",
            Self::Interpret => "interpret",
            Self::Phone => "phone",
            Self::Image => "image",
            Self::Music => "music",
            Self::Video => "video",
        }
    }

    /// Parse from string key. Returns None for unknown keys.
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "daily" => Some(Self::Daily),
            "shopping" => Some(Self::Shopping),
            "document" => Some(Self::Document),
            "coding" => Some(Self::Coding),
            "interpret" => Some(Self::Interpret),
            "phone" => Some(Self::Phone),
            "image" => Some(Self::Image),
            "music" => Some(Self::Music),
            "video" => Some(Self::Video),
            _ => None,
        }
    }

    /// Get the SeedCategoryInfo for this category.
    pub fn info(self) -> &'static SeedCategoryInfo {
        &SEED_CATEGORIES[self.index() as usize]
    }

    /// 0-based index in the SEED_CATEGORIES array.
    pub fn index(self) -> u8 {
        match self {
            Self::Daily => 0,
            Self::Shopping => 1,
            Self::Document => 2,
            Self::Coding => 3,
            Self::Interpret => 4,
            Self::Phone => 5,
            Self::Image => 6,
            Self::Music => 7,
            Self::Video => 8,
        }
    }

    /// All seed categories in order.
    pub fn all() -> &'static [SeedCategory] {
        &[
            Self::Daily,
            Self::Shopping,
            Self::Document,
            Self::Coding,
            Self::Interpret,
            Self::Phone,
            Self::Image,
            Self::Music,
            Self::Video,
        ]
    }

    /// Check if a string key is a known seed category.
    pub fn is_seed_key(key: &str) -> bool {
        Self::from_key(key).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_count_is_9() {
        assert_eq!(SEED_CATEGORIES.len(), 9);
        assert_eq!(SeedCategory::all().len(), 9);
    }

    #[test]
    fn key_roundtrip() {
        for cat in SeedCategory::all() {
            let key = cat.key();
            let parsed = SeedCategory::from_key(key).unwrap();
            assert_eq!(*cat, parsed);
        }
    }

    #[test]
    fn unknown_key_returns_none() {
        assert!(SeedCategory::from_key("unknown").is_none());
        assert!(SeedCategory::from_key("").is_none());
    }

    #[test]
    fn info_matches_key() {
        for cat in SeedCategory::all() {
            let info = cat.info();
            assert_eq!(info.key, cat.key());
        }
    }

    #[test]
    fn indexes_are_sequential() {
        for (i, cat) in SeedCategory::all().iter().enumerate() {
            assert_eq!(cat.index() as usize, i);
        }
    }

    #[test]
    fn display_order_preserved() {
        let keys: Vec<&str> = SEED_CATEGORIES.iter().map(|c| c.key).collect();
        assert_eq!(
            keys,
            vec!["daily", "shopping", "document", "coding", "interpret", "phone", "image", "music", "video"]
        );
    }

    #[test]
    fn is_seed_key_works() {
        assert!(SeedCategory::is_seed_key("daily"));
        assert!(SeedCategory::is_seed_key("phone"));
        assert!(!SeedCategory::is_seed_key("custom"));
        assert!(!SeedCategory::is_seed_key(""));
    }

    #[test]
    fn serde_roundtrip() {
        let cat = SeedCategory::Phone;
        let json = serde_json::to_string(&cat).unwrap();
        assert_eq!(json, "\"phone\"");
        let parsed: SeedCategory = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, cat);
    }

    #[test]
    fn all_have_nonempty_names_and_icons() {
        for info in SEED_CATEGORIES {
            assert!(!info.name_ko.is_empty(), "Empty name for {}", info.key);
            assert!(!info.icon.is_empty(), "Empty icon for {}", info.key);
        }
    }
}

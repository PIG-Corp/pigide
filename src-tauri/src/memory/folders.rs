//! Mapping between PigMemory `Kind` and on-disk folder prefix.
//!
//! Centralises the kind-to-folder convention so nothing else hardcodes
//! `"concepts/"` etc. The default kind for a flat-slug note is `Source`
//! (legacy notes from before Phase 0 land in this bucket).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Concept,
    Entity,
    Source,
    Task,
    Chat,
    Meta,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Concept => "concept",
            Kind::Entity => "entity",
            Kind::Source => "source",
            Kind::Task => "task",
            Kind::Chat => "chat",
            Kind::Meta => "meta",
        }
    }

    pub fn folder(self) -> &'static str {
        match self {
            Kind::Concept => "concepts",
            Kind::Entity => "entities",
            Kind::Source => "sources",
            Kind::Task => "tasks",
            Kind::Chat => "chats",
            Kind::Meta => "meta",
        }
    }

    pub fn parse(s: &str) -> Option<Kind> {
        match s {
            "concept" => Some(Kind::Concept),
            "entity" => Some(Kind::Entity),
            "source" => Some(Kind::Source),
            "task" => Some(Kind::Task),
            "chat" => Some(Kind::Chat),
            "meta" => Some(Kind::Meta),
            _ => None,
        }
    }

    /// Default kind used when a note has no `kind` field on disk yet.
    pub fn default_for_legacy() -> Kind {
        Kind::Source
    }
}

/// Best-effort guess from the slug's leading folder. Used only by the
/// migration to assign a kind to old notes that happen to live in a
/// recognisable folder; otherwise falls back to `Source`.
pub fn kind_for_slug(slug: &str) -> Kind {
    let leading = slug.split('/').next().unwrap_or(slug);
    match leading {
        "concepts" => Kind::Concept,
        "entities" => Kind::Entity,
        "sources" => Kind::Source,
        "tasks" => Kind::Task,
        "chats" => Kind::Chat,
        "meta" => Kind::Meta,
        _ => Kind::default_for_legacy(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_str_round_trip() {
        for k in [
            Kind::Concept,
            Kind::Entity,
            Kind::Source,
            Kind::Task,
            Kind::Chat,
            Kind::Meta,
        ] {
            assert_eq!(Kind::parse(k.as_str()), Some(k));
        }
    }

    #[test]
    fn folder_is_pluralised() {
        assert_eq!(Kind::Concept.folder(), "concepts");
        assert_eq!(Kind::Task.folder(), "tasks");
        assert_eq!(Kind::Meta.folder(), "meta");
    }

    #[test]
    fn legacy_default_is_source() {
        assert_eq!(Kind::default_for_legacy(), Kind::Source);
        assert_eq!(kind_for_slug("auth-pattern"), Kind::Source);
    }

    #[test]
    fn kind_for_slug_recognises_folders() {
        assert_eq!(kind_for_slug("tasks/abc-123"), Kind::Task);
        assert_eq!(kind_for_slug("concepts/idempotent-upsert"), Kind::Concept);
        assert_eq!(kind_for_slug("chats/claude/2026-05-27"), Kind::Chat);
        assert_eq!(kind_for_slug("meta/hot"), Kind::Meta);
    }
}

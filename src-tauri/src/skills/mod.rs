//! PigIDE Skills system.
//!
//! Skills are small composable prompt-modules — `.md` files with a YAML
//! frontmatter — that the Architect (Orchestrator) auto-discovers, indexes,
//! hot-reloads, and selects per turn. See [`SKILLS_DESIGN.md`] at the repo
//! root for the full design.
//!
//! Module layout:
//!
//! - [`skill`] — frontmatter + body type, parser.
//! - [`composer`] — handlebars-lite renderer + system-prompt composition.
//! - [`router`] — deterministic + optional LLM routing.
//! - [`registry`] — in-memory index, discovery, hot-reload watcher.
//! - [`trace`] — per-turn telemetry persisted to SQLite.
//! - [`tools`] — Tauri commands consumed by the frontend.

pub mod claude_import;
pub mod composer;
pub mod registry;
pub mod router;
pub mod skill;
pub mod tools;
pub mod trace;
pub mod watcher;

pub use composer::{compose_system_prompt, ComposeResult};
pub use registry::{SkillRegistry, SkillSource};
pub use router::{route, RouteResult, RouterConfig, RouterMode};
pub use skill::{Skill, SkillFrontmatter};

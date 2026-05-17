//! Always-On Architect — фоновый супервизор, который непрерывно следит за
//! всеми running-агентами в активном workspace и реагирует на их состояние.
//!
//! Loop живёт на `tokio` runtime внутри AppState и не зависит от того,
//! открыто ли UI-окно. Безопасные действия (нажать `y` на continue, заассайнить
//! следующую todo-таску простаивающему агенту) выполняются автоматически.
//! Опасные вопросы и ошибки эскалируются в чат-панель.

pub mod classifier;
pub mod policy;
pub mod supervisor;

pub use classifier::{classify, AgentSignal};
pub use policy::{decide, PolicyDecision};
pub use supervisor::{Architect, ArchitectConfig, DecisionLog, DecisionRecord};

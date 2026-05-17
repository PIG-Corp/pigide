//! Public surface of the resolver. This module is the only thing the rest
//! of PigIDE needs to import.

pub mod aliases;
pub mod fuzzy;
pub mod indexer;
pub mod parsers;
pub mod resolver;
pub mod service;
pub mod translit;

pub use indexer::{ProjectEntry, ProjectIndex, ScanOptions};
pub use resolver::{Candidate, ResolveContext, ResolveOutcome, ResolveStatus};
pub use service::ResolverService;

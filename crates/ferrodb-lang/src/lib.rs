//! # ferrodb-lang
//!
//! Query language support. Phase 3 provides what execution needs: statement
//! splitting with result titles, parameters and statement analysis (read-only
//! enforcement, transaction tracking, warnings). Completion, diagnostics and
//! formatting arrive with the intelligent editor (phase 5).

pub mod service;
pub mod sql;

pub use service::BasicLanguageService;

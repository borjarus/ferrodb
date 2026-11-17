//! # ferrodb-lang
//!
//! Query language support.
//! - [`sql`]: statement splitting, parameters and statement analysis
//!   (phase 3); the intelligent editor of phase 5 — dialects as data,
//!   structural parser, name resolution, completion, inspections with quick
//!   fixes, formatter, rename, structure view, parameter info, live
//!   templates;
//! - [`mql`]: the MongoDB console language (phase 4b);
//! - [`service`]: the [`LanguageService`](ferrodb_api::LanguageService)
//!   implementations on top of both.

pub mod mql;
pub mod service;
pub mod sql;

pub use service::{BasicLanguageService, Engine};

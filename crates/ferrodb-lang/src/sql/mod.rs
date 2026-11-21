//! SQL support shared by all dialects: lexing, statement splitting,
//! parameters and statement analysis (phase 3); dialects as data, the
//! structural parser and the schema model of the intelligent editor
//! (phase 5). Lexical dialect differences are a [`Flavor`].

pub mod analyze;
pub mod complete;
pub mod dialect;
pub mod document;
pub mod format;
pub mod inspect;
pub mod lexer;
pub mod matcher;
pub mod params;
pub mod refactor;
pub mod resolve;
pub mod schema;
pub mod split;
pub mod syntax;
pub mod templates;

pub use analyze::{StatementClass, TxCommand, classify, risks, statement_risk};
pub use dialect::Dialect;
pub use lexer::Flavor;
pub use params::{OffsetMap, parameters, substitute};
pub use split::{Statement, split};

#[cfg(test)]
mod dialect_corpus_tests;
#[cfg(test)]
mod levela_tests;

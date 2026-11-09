//! SQL support shared by all dialects: lexing, statement splitting,
//! parameters and statement analysis. Dialect differences are a [`Flavor`].

pub mod analyze;
pub mod lexer;
pub mod params;
pub mod split;

pub use analyze::{StatementClass, TxCommand, classify, risks, statement_risk};
pub use lexer::Flavor;
pub use params::{OffsetMap, parameters, substitute};
pub use split::{Statement, split};

//! The decision table of the permission layer: what the policy says about a
//! call before anybody is asked (no I/O).

use ferrodb_api::{AgentAccess, ConsentMode};

/// What a tool call does, as far as the policy is concerned.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CallClass {
    /// Listings and definitions (explorer metadata).
    Metadata,
    /// A query that only reads (or is only explained, never run).
    Read,
    /// A query that changes data or schema.
    Modify,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PolicyDecision {
    /// The data source does not exist for agents.
    NotFound,
    /// Refused by the policy, with the reason given to the agent.
    Deny(String),
    /// Allowed without asking.
    Allow,
    /// The user must decide.
    Ask,
}

pub const METADATA_ONLY: &str =
    "queries are not allowed: agents may only read the metadata of this data source";
pub const READ_ONLY_FOR_AGENTS: &str =
    "data source is read-only for agents: the query changes data or schema";
pub const READ_ONLY_SOURCE: &str = "data source is read-only (its connection is configured read-only): the query changes data or schema";

/// The policy's answer for a call of `class` on a data source with `access`;
/// `source_read_only` is the data source's own read-only flag.
///
/// - `Hidden`: the source does not exist for agents.
/// - `MetadataOnly`: listings and definitions only.
/// - `ReadOnly`: modifying queries are refused without asking.
/// - `ReadWrite`: modifying queries always ask, whatever the consent mode,
///   and are refused on read-only sources.
/// - Calls that do not modify ask according to `consent`: every call,
///   queries only, or never (`WritesOnly`).
pub fn decide(
    access: AgentAccess,
    consent: ConsentMode,
    source_read_only: bool,
    class: CallClass,
) -> PolicyDecision {
    match (access, class) {
        (AgentAccess::Hidden, _) => PolicyDecision::NotFound,
        (AgentAccess::MetadataOnly, CallClass::Read | CallClass::Modify) => {
            PolicyDecision::Deny(METADATA_ONLY.into())
        }
        (AgentAccess::ReadOnly, CallClass::Modify) => {
            PolicyDecision::Deny(READ_ONLY_FOR_AGENTS.into())
        }
        (AgentAccess::ReadWrite, CallClass::Modify) if source_read_only => {
            PolicyDecision::Deny(READ_ONLY_SOURCE.into())
        }
        (AgentAccess::ReadWrite, CallClass::Modify) => PolicyDecision::Ask,
        (_, CallClass::Metadata) => match consent {
            ConsentMode::EveryCall => PolicyDecision::Ask,
            ConsentMode::QueriesOnly | ConsentMode::WritesOnly => PolicyDecision::Allow,
        },
        (_, CallClass::Read) => match consent {
            ConsentMode::EveryCall | ConsentMode::QueriesOnly => PolicyDecision::Ask,
            ConsentMode::WritesOnly => PolicyDecision::Allow,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use AgentAccess as A;
    use CallClass as C;
    use ConsentMode as M;
    use PolicyDecision as D;

    fn deny(msg: &str) -> D {
        D::Deny(msg.into())
    }

    /// Every access × consent mode × call class, on a writable source.
    #[test]
    fn decision_table() {
        let table = [
            // access, mode, metadata, read, modify
            (
                A::Hidden,
                M::EveryCall,
                D::NotFound,
                D::NotFound,
                D::NotFound,
            ),
            (
                A::Hidden,
                M::QueriesOnly,
                D::NotFound,
                D::NotFound,
                D::NotFound,
            ),
            (
                A::Hidden,
                M::WritesOnly,
                D::NotFound,
                D::NotFound,
                D::NotFound,
            ),
            (
                A::MetadataOnly,
                M::EveryCall,
                D::Ask,
                deny(METADATA_ONLY),
                deny(METADATA_ONLY),
            ),
            (
                A::MetadataOnly,
                M::QueriesOnly,
                D::Allow,
                deny(METADATA_ONLY),
                deny(METADATA_ONLY),
            ),
            (
                A::MetadataOnly,
                M::WritesOnly,
                D::Allow,
                deny(METADATA_ONLY),
                deny(METADATA_ONLY),
            ),
            (
                A::ReadOnly,
                M::EveryCall,
                D::Ask,
                D::Ask,
                deny(READ_ONLY_FOR_AGENTS),
            ),
            (
                A::ReadOnly,
                M::QueriesOnly,
                D::Allow,
                D::Ask,
                deny(READ_ONLY_FOR_AGENTS),
            ),
            (
                A::ReadOnly,
                M::WritesOnly,
                D::Allow,
                D::Allow,
                deny(READ_ONLY_FOR_AGENTS),
            ),
            (A::ReadWrite, M::EveryCall, D::Ask, D::Ask, D::Ask),
            (A::ReadWrite, M::QueriesOnly, D::Allow, D::Ask, D::Ask),
            (A::ReadWrite, M::WritesOnly, D::Allow, D::Allow, D::Ask),
        ];
        for (access, mode, metadata, read, modify) in table {
            for (class, expected) in [
                (C::Metadata, metadata),
                (C::Read, read),
                (C::Modify, modify),
            ] {
                assert_eq!(
                    decide(access, mode, false, class),
                    expected,
                    "{access:?} / {mode:?} / {class:?}"
                );
            }
        }
    }

    /// A read-only data source refuses writes even with read-write access;
    /// reads and metadata are unaffected.
    #[test]
    fn read_only_source() {
        for mode in [M::EveryCall, M::QueriesOnly, M::WritesOnly] {
            assert_eq!(
                decide(A::ReadWrite, mode, true, C::Modify),
                deny(READ_ONLY_SOURCE)
            );
            assert_eq!(
                decide(A::ReadOnly, mode, true, C::Modify),
                deny(READ_ONLY_FOR_AGENTS)
            );
            assert_eq!(
                decide(A::ReadWrite, mode, true, C::Read),
                decide(A::ReadWrite, mode, false, C::Read)
            );
            assert_eq!(
                decide(A::ReadWrite, mode, true, C::Metadata),
                decide(A::ReadWrite, mode, false, C::Metadata)
            );
            assert_eq!(decide(A::Hidden, mode, true, C::Read), D::NotFound);
        }
    }
}

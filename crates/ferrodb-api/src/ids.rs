use std::fmt;

use serde::{Deserialize, Serialize};

macro_rules! string_id {
    ($(#[$m:meta])* $name:ident) => {
        $(#[$m])*
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            pub fn new(s: impl Into<String>) -> Self {
                Self(s.into())
            }
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl From<&str> for $name {
            fn from(s: &str) -> Self {
                Self(s.to_owned())
            }
        }
    };
}

macro_rules! numeric_id {
    ($(#[$m:meta])* $name:ident) => {
        $(#[$m])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub u64);

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}#{}", stringify!($name), self.0)
            }
        }
    };
}

string_id!(
    /// Stable identifier of a configured data source (UUID-like string).
    DataSourceId
);
string_id!(
    /// Driver identifier, e.g. `"postgresql"`, `"redshift"`, `"oracle"`, `"mongodb"`.
    DriverId
);
string_id!(
    /// Driver family / protocol, e.g. `"pgwire"`, `"tds"`, `"jdbc"`, `"adbc"`.
    FamilyId
);
string_id!(
    /// SQL dialect, e.g. `"postgresql"`, `"greenplum"`, `"tsql"`, `"oracle"`, `"hiveql"`.
    SqlDialectId
);

numeric_id!(
    /// An open session (connection + console context) on a data source.
    SessionId
);
numeric_id!(
    /// One execution of a script.
    QueryId
);
numeric_id!(
    /// One result (table, documents or keys) produced by a query.
    ResultId
);

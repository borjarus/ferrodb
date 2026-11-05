use std::time::Duration;

/// Load and failure scenarios switched from the developer menu (plan, section 6).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Scenario {
    #[default]
    Normal,
    MillionRows,
    HundredThousandDocuments,
    MillionKeys,
    FiveMbDocument,
    TenThousandTables,
    SlowNetwork,
    ConnectionError,
    DriverNotDownloaded,
    JdbcProcessFailure,
}

impl Scenario {
    pub const ALL: [Scenario; 10] = [
        Self::Normal,
        Self::MillionRows,
        Self::HundredThousandDocuments,
        Self::MillionKeys,
        Self::FiveMbDocument,
        Self::TenThousandTables,
        Self::SlowNetwork,
        Self::ConnectionError,
        Self::DriverNotDownloaded,
        Self::JdbcProcessFailure,
    ];

    /// Stable id used in translations (`scenario-<id>`) and settings.
    pub fn id(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::MillionRows => "million-rows",
            Self::HundredThousandDocuments => "100k-documents",
            Self::MillionKeys => "million-keys",
            Self::FiveMbDocument => "5mb-document",
            Self::TenThousandTables => "10k-tables",
            Self::SlowNetwork => "slow-network",
            Self::ConnectionError => "connection-error",
            Self::DriverNotDownloaded => "driver-not-downloaded",
            Self::JdbcProcessFailure => "jdbc-process-failure",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.id() == id)
    }

    /// Simulated round-trip latency.
    pub fn latency(self) -> Duration {
        match self {
            Self::SlowNetwork => Duration::from_millis(1500),
            _ => Duration::from_millis(40),
        }
    }

    pub fn table_rows(self) -> u64 {
        if self == Self::MillionRows {
            1_000_000
        } else {
            1_000
        }
    }

    pub fn table_columns(self) -> usize {
        if self == Self::MillionRows { 50 } else { 12 }
    }

    pub fn documents(self) -> u64 {
        if self == Self::HundredThousandDocuments {
            100_000
        } else {
            200
        }
    }

    pub fn keys(self) -> u64 {
        if self == Self::MillionKeys {
            1_000_000
        } else {
            2_000
        }
    }

    pub fn tables_per_schema(self) -> usize {
        if self == Self::TenThousandTables {
            10_000
        } else {
            24
        }
    }
}

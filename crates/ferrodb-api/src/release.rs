//! Release services (plan, phase 10): signed application updates with a
//! beta channel, and crash reports sent only with the user's consent.
//!
//! Both are application-level, not tied to a data source. The default
//! implementations ([`NoUpdates`], [`NoCrashReports`]) report
//! `available() == false`: the mock, tests and older backends work unchanged.
//!
//! Security rules (plan, section 3):
//! - an update manifest is accepted only with a valid Ed25519 signature made
//!   with the release key embedded in the application; the package it points
//!   to is accepted only when its SHA-256 matches the signed manifest;
//! - nothing leaves the machine without consent: crash reports are written
//!   locally and sent only when [`CrashConsent::Always`] is set or the user
//!   sends them explicitly; secrets are masked before a report is written;
//! - no telemetry of any other kind.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{BoxFuture, CancelHandle, Result};

/// Which releases the user wants to be offered.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum UpdateChannel {
    #[default]
    Stable,
    /// The beta program: release candidates and betas, plus stable releases.
    Beta,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UpdateSettings {
    pub channel: UpdateChannel,
    /// Check once a day, at startup (one request, no identifiers sent).
    pub check_automatically: bool,
}

impl Default for UpdateSettings {
    fn default() -> Self {
        Self {
            channel: UpdateChannel::Stable,
            check_automatically: true,
        }
    }
}

/// A release offered by the signed manifest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateInfo {
    /// Semantic version (`1.0.0`, `1.1.0-beta.2`).
    pub version: String,
    pub channel: UpdateChannel,
    /// Release date (`YYYY-MM-DD`).
    pub date: String,
    /// Release notes (Markdown, short).
    pub notes: String,
    /// Package for this platform.
    pub url: String,
    pub sha256: String,
    pub size: u64,
}

/// Events of a running download / install.
#[derive(Clone, Debug, PartialEq)]
pub enum UpdateEvent {
    Progress { done: u64, total: u64 },
    /// The package is downloaded and verified; [`UpdateService::install`]
    /// may be called with this path.
    Ready(PathBuf),
    Failed(String),
}

pub struct UpdateDownload {
    pub events: async_channel::Receiver<UpdateEvent>,
    pub cancel: CancelHandle,
}

pub trait UpdateService: Send + Sync {
    fn available(&self) -> bool {
        true
    }
    /// Version of the running application.
    fn current_version(&self) -> String;
    fn settings(&self) -> UpdateSettings;
    fn save_settings(&self, settings: UpdateSettings) -> BoxFuture<'_, Result<()>>;
    /// Fetches and verifies the manifest; `Some` when a newer release exists
    /// on the configured channel for this platform.
    fn check(&self) -> BoxFuture<'_, Result<Option<UpdateInfo>>>;
    /// `true` when an automatic check is due (enabled and not done in the
    /// last 24 hours). The UI calls [`Self::check`] at startup if so.
    fn check_due(&self) -> bool;
    /// Downloads the package and verifies its checksum.
    fn download(&self, update: UpdateInfo) -> UpdateDownload;
    /// Starts the installer of a verified package. The caller quits the
    /// application afterwards.
    fn install(&self, package: PathBuf) -> BoxFuture<'_, Result<()>>;
}

pub struct NoUpdates;

impl UpdateService for NoUpdates {
    fn available(&self) -> bool {
        false
    }
    fn current_version(&self) -> String {
        String::new()
    }
    fn settings(&self) -> UpdateSettings {
        UpdateSettings::default()
    }
    fn save_settings(&self, _settings: UpdateSettings) -> BoxFuture<'_, Result<()>> {
        Box::pin(async { Err(crate::Error::Unsupported("updates".into())) })
    }
    fn check(&self) -> BoxFuture<'_, Result<Option<UpdateInfo>>> {
        Box::pin(async { Err(crate::Error::Unsupported("updates".into())) })
    }
    fn check_due(&self) -> bool {
        false
    }
    fn download(&self, _update: UpdateInfo) -> UpdateDownload {
        let (tx, rx) = async_channel::bounded(1);
        let _ = tx.try_send(UpdateEvent::Failed("updates are not available".into()));
        UpdateDownload {
            events: rx,
            cancel: CancelHandle::new(),
        }
    }
    fn install(&self, _package: PathBuf) -> BoxFuture<'_, Result<()>> {
        Box::pin(async { Err(crate::Error::Unsupported("updates".into())) })
    }
}

/// What to do with crash reports.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CrashConsent {
    /// Ask after a crash, at the next start (default).
    #[default]
    Ask,
    /// Send automatically.
    Always,
    /// Keep reports on disk only.
    Never,
}

/// A crash captured by the panic hook (or a fatal error), stored locally.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrashReport {
    /// File stem of the report (`20271108-101500-1a2b`).
    pub id: String,
    /// RFC 3339.
    pub time: String,
    pub app_version: String,
    /// `windows x86_64 10.0.26100`.
    pub os: String,
    /// Thread name, where known.
    pub thread: Option<String>,
    /// Panic message, secrets masked.
    pub message: String,
    /// `file:line:column` of the panic.
    pub location: Option<String>,
    pub backtrace: String,
    /// Last log lines before the crash, secrets masked.
    pub log_tail: Vec<String>,
    /// Sent to the crash endpoint already.
    pub sent: bool,
    /// User comment added before sending.
    #[serde(default)]
    pub comment: String,
}

pub trait CrashReportService: Send + Sync {
    fn available(&self) -> bool {
        true
    }
    fn consent(&self) -> CrashConsent;
    fn set_consent(&self, consent: CrashConsent) -> BoxFuture<'_, Result<()>>;
    /// Reports from earlier runs not sent nor dismissed yet, newest first.
    fn pending(&self) -> Vec<CrashReport>;
    /// Every stored report, newest first.
    fn all(&self) -> Vec<CrashReport>;
    /// Sends the reports (with an optional comment) and marks them as sent.
    fn send(&self, ids: Vec<String>, comment: String) -> BoxFuture<'_, Result<()>>;
    /// Keeps the reports on disk but stops offering them.
    fn dismiss(&self, ids: Vec<String>) -> BoxFuture<'_, Result<()>>;
    /// Deletes every stored report.
    fn clear(&self) -> BoxFuture<'_, Result<()>>;
    /// Where reports are kept (`None` in memory).
    fn directory(&self) -> Option<PathBuf>;
}

pub struct NoCrashReports;

impl CrashReportService for NoCrashReports {
    fn available(&self) -> bool {
        false
    }
    fn consent(&self) -> CrashConsent {
        CrashConsent::Never
    }
    fn set_consent(&self, _consent: CrashConsent) -> BoxFuture<'_, Result<()>> {
        Box::pin(async { Err(crate::Error::Unsupported("crash reports".into())) })
    }
    fn pending(&self) -> Vec<CrashReport> {
        Vec::new()
    }
    fn all(&self) -> Vec<CrashReport> {
        Vec::new()
    }
    fn send(&self, _ids: Vec<String>, _comment: String) -> BoxFuture<'_, Result<()>> {
        Box::pin(async { Err(crate::Error::Unsupported("crash reports".into())) })
    }
    fn dismiss(&self, _ids: Vec<String>) -> BoxFuture<'_, Result<()>> {
        Box::pin(async { Ok(()) })
    }
    fn clear(&self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async { Ok(()) })
    }
    fn directory(&self) -> Option<PathBuf> {
        None
    }
}

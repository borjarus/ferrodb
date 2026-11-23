//! Crash reports: the panic hook that writes them, the store that lists
//! them, and the [`CrashReportService`] the UI uses.
//!
//! Layout: `<data>/crashes/<id>.json` (one [`CrashReport`] each) and
//! `<data>/crashes/dismissed.txt` (ids the user chose not to send).
//! Reports are sent only by an explicit [`CrashReportService::send`] (the UI
//! calls it after the user agreed, or silently with
//! [`CrashConsent::Always`]); the endpoint is fixed at build time.

use std::{
    collections::HashSet,
    fs,
    panic::PanicHookInfo,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicU32, Ordering},
    },
};

use chrono::{DateTime, Utc};
use ferrodb_api::{BoxFuture, CrashConsent, CrashReport, CrashReportService, Error, Result};

use crate::{logbuf, mask::mask_secrets, settings::SettingsFile, spawn_blocking};

/// Error of [`CrashReportService::send`] without a build-time endpoint.
pub const NO_ENDPOINT: &str = "no crash endpoint configured in this build";
const DISMISSED: &str = "dismissed.txt";

/// `<data>/crashes`.
pub fn crash_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("crashes")
}

// ------------------------------------------------------------------ hook

struct HookState {
    dir: PathBuf,
    version: String,
}

fn hook_state() -> &'static Mutex<Option<HookState>> {
    static S: OnceLock<Mutex<Option<HookState>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(None))
}

/// `windows x86_64 10.0.26100`; the OS version is probed once, off the
/// panic path.
pub fn os_description() -> String {
    static OS: OnceLock<String> = OnceLock::new();
    OS.get_or_init(|| {
        let version = os_version().unwrap_or_default();
        format!(
            "{} {} {}",
            std::env::consts::OS,
            std::env::consts::ARCH,
            version
        )
        .trim()
        .to_owned()
    })
    .clone()
}

fn command_output(cmd: &str, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new(cmd).args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

fn os_version() -> Option<String> {
    if cfg!(windows) {
        // "Microsoft Windows [Version 10.0.26100.1234]"
        let v = command_output("cmd", &["/C", "ver"])?;
        let start = v.find("Version ")? + "Version ".len();
        Some(v[start..].trim_end_matches(']').to_owned())
    } else if cfg!(target_os = "macos") {
        command_output("sw_vers", &["-productVersion"])
    } else {
        fs::read_to_string("/proc/sys/kernel/osrelease")
            .ok()
            .map(|s| s.trim().to_owned())
    }
}

/// A unique report id: `20271108-101500-1a2b`.
fn new_id(now: DateTime<Utc>) -> String {
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let salt = (now.timestamp_subsec_nanos() ^ std::process::id().rotate_left(7) ^ n) & 0xffff;
    format!("{}-{salt:04x}", now.format("%Y%m%d-%H%M%S"))
}

fn panic_message(info: &PanicHookInfo<'_>) -> String {
    let payload = info.payload();
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_owned()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "Box<dyn Any>".to_owned()
    }
}

/// Builds the report of a panic (secrets masked).
pub fn report_from_panic(info: &PanicHookInfo<'_>, version: &str) -> CrashReport {
    let now = Utc::now();
    CrashReport {
        id: new_id(now),
        time: now.to_rfc3339(),
        app_version: version.to_owned(),
        os: os_description(),
        thread: std::thread::current().name().map(str::to_owned),
        message: mask_secrets(&panic_message(info)),
        location: info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column())),
        backtrace: std::backtrace::Backtrace::force_capture().to_string(),
        log_tail: logbuf::tail(),
        sent: false,
        comment: String::new(),
    }
}

/// Writes a report to `dir/<id>.json`.
pub fn write_report(dir: &Path, report: &CrashReport) -> std::io::Result<PathBuf> {
    fs::create_dir_all(dir)?;
    let path = dir.join(format!("{}.json", report.id));
    let json = serde_json::to_vec_pretty(report).map_err(std::io::Error::other)?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, json)?;
    fs::rename(&tmp, &path)?;
    Ok(path)
}

/// Installs the panic hook: every panic writes a report to
/// `<data_dir>/crashes`, then the previous hook runs (stderr message,
/// test harness…). Call first thing in `main`; calling again only changes
/// the directory and version.
pub fn install_panic_hook(data_dir: &Path, version: &str) {
    let first = {
        let mut s = hook_state().lock().unwrap_or_else(|p| p.into_inner());
        let first = s.is_none();
        *s = Some(HookState {
            dir: crash_dir(data_dir),
            version: version.to_owned(),
        });
        first
    };
    // Probe the OS version now, not while panicking.
    std::thread::spawn(os_description);
    if !first {
        return;
    }
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        write_panic_report(info);
        previous(info);
    }));
}

/// Writes the report of `info` where the installed hook says (if any);
/// returns its path. Never panics.
pub fn write_panic_report(info: &PanicHookInfo<'_>) -> Option<PathBuf> {
    let (dir, version) = {
        let s = match hook_state().try_lock() {
            Ok(s) => s,
            Err(std::sync::TryLockError::Poisoned(p)) => p.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => return None,
        };
        let s = s.as_ref()?;
        (s.dir.clone(), s.version.clone())
    };
    let report = report_from_panic(info, &version);
    match write_report(&dir, &report) {
        Ok(p) => {
            eprintln!("FerroDb: crash report written to {}", p.display());
            Some(p)
        }
        Err(e) => {
            eprintln!("FerroDb: could not write a crash report: {e}");
            None
        }
    }
}

// ------------------------------------------------------------------ store

/// Reports on disk (or in memory when there is no data directory).
pub struct CrashStore {
    dir: Option<PathBuf>,
    /// In-memory reports (`dir == None`; also tests).
    memory: Mutex<Vec<CrashReport>>,
    memory_dismissed: Mutex<HashSet<String>>,
}

impl CrashStore {
    pub fn new(dir: Option<PathBuf>) -> Self {
        Self {
            dir,
            memory: Mutex::new(Vec::new()),
            memory_dismissed: Mutex::new(HashSet::new()),
        }
    }

    pub fn dir(&self) -> Option<&Path> {
        self.dir.as_deref()
    }

    /// Every report, newest first.
    pub fn all(&self) -> Vec<CrashReport> {
        let mut v = match &self.dir {
            Some(dir) => fs::read_dir(dir)
                .map(|rd| {
                    rd.flatten()
                        .map(|e| e.path())
                        .filter(|p| p.extension().is_some_and(|e| e == "json"))
                        .filter_map(|p| fs::read(&p).ok())
                        .filter_map(|b| serde_json::from_slice::<CrashReport>(&b).ok())
                        .collect()
                })
                .unwrap_or_default(),
            None => self
                .memory
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clone(),
        };
        v.sort_by(|a, b| b.time.cmp(&a.time).then_with(|| b.id.cmp(&a.id)));
        v
    }

    pub fn dismissed(&self) -> HashSet<String> {
        match &self.dir {
            Some(dir) => fs::read_to_string(dir.join(DISMISSED))
                .map(|t| {
                    t.lines()
                        .map(str::trim)
                        .filter(|l| !l.is_empty())
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default(),
            None => self
                .memory_dismissed
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clone(),
        }
    }

    pub fn dismiss(&self, ids: &[String]) -> std::io::Result<()> {
        let mut set = self.dismissed();
        set.extend(ids.iter().cloned());
        match &self.dir {
            Some(dir) => {
                fs::create_dir_all(dir)?;
                let mut lines: Vec<_> = set.into_iter().collect();
                lines.sort();
                fs::write(dir.join(DISMISSED), lines.join("\n") + "\n")
            }
            None => {
                *self
                    .memory_dismissed
                    .lock()
                    .unwrap_or_else(|p| p.into_inner()) = set;
                Ok(())
            }
        }
    }

    /// Adds or replaces a report.
    pub fn save(&self, report: &CrashReport) -> std::io::Result<()> {
        match &self.dir {
            Some(dir) => write_report(dir, report).map(|_| ()),
            None => {
                let mut m = self.memory.lock().unwrap_or_else(|p| p.into_inner());
                m.retain(|r| r.id != report.id);
                m.push(report.clone());
                Ok(())
            }
        }
    }

    pub fn clear(&self) -> std::io::Result<()> {
        match &self.dir {
            Some(dir) => {
                if let Ok(rd) = fs::read_dir(dir) {
                    for p in rd.flatten().map(|e| e.path()) {
                        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                        if name.ends_with(".json") || name == DISMISSED || name.ends_with(".tmp") {
                            fs::remove_file(&p)?;
                        }
                    }
                }
                Ok(())
            }
            None => {
                self.memory
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .clear();
                self.memory_dismissed
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .clear();
                Ok(())
            }
        }
    }
}

// ------------------------------------------------------------------ service

/// Posts one report (JSON) to the endpoint. Blocking.
pub(crate) fn post_report(
    endpoint: &str,
    report: &CrashReport,
    allow_loopback_http: bool,
) -> std::result::Result<(), String> {
    crate::http::check_url(endpoint, allow_loopback_http)?;
    let body = serde_json::to_string(report).map_err(|e| e.to_string())?;
    crate::http::agent()
        .post(endpoint)
        .header("content-type", "application/json")
        .send(body)
        .map(|_| ())
        .map_err(|e| format!("{endpoint}: {e}"))
}

pub struct CrashReports {
    store: Arc<CrashStore>,
    settings: Arc<SettingsFile>,
    endpoint: Option<String>,
    allow_loopback_http: bool,
    /// Reports newer than this were written by the running process and are
    /// not offered until the next start.
    started: DateTime<Utc>,
}

impl CrashReports {
    pub fn new(
        store: Arc<CrashStore>,
        settings: Arc<SettingsFile>,
        endpoint: Option<String>,
        allow_loopback_http: bool,
    ) -> Self {
        Self {
            store,
            settings,
            endpoint,
            allow_loopback_http,
            started: Utc::now(),
        }
    }

    pub fn store(&self) -> &Arc<CrashStore> {
        &self.store
    }

    fn is_from_earlier_run(&self, r: &CrashReport) -> bool {
        DateTime::parse_from_rfc3339(&r.time).is_ok_and(|t| t.with_timezone(&Utc) <= self.started)
    }
}

fn io(e: std::io::Error) -> Error {
    Error::Other(e.to_string())
}

impl CrashReportService for CrashReports {
    fn consent(&self) -> CrashConsent {
        self.settings.get().consent
    }

    fn set_consent(&self, consent: CrashConsent) -> BoxFuture<'_, Result<()>> {
        let settings = self.settings.clone();
        Box::pin(spawn_blocking(move || {
            settings.update(|s| s.consent = consent).map_err(io)
        }))
    }

    fn pending(&self) -> Vec<CrashReport> {
        let dismissed = self.store.dismissed();
        self.store
            .all()
            .into_iter()
            .filter(|r| !r.sent && !dismissed.contains(&r.id) && self.is_from_earlier_run(r))
            .collect()
    }

    fn all(&self) -> Vec<CrashReport> {
        self.store.all()
    }

    fn send(&self, ids: Vec<String>, comment: String) -> BoxFuture<'_, Result<()>> {
        let store = self.store.clone();
        let endpoint = self.endpoint.clone();
        let allow = self.allow_loopback_http;
        Box::pin(spawn_blocking(move || {
            let Some(endpoint) = endpoint else {
                return Err(Error::Other(NO_ENDPOINT.into()));
            };
            let comment = mask_secrets(comment.trim());
            for mut r in store.all().into_iter().filter(|r| ids.contains(&r.id)) {
                if !comment.is_empty() {
                    r.comment = comment.clone();
                }
                post_report(&endpoint, &r, allow).map_err(Error::Connection)?;
                r.sent = true;
                store.save(&r).map_err(io)?;
            }
            Ok(())
        }))
    }

    fn dismiss(&self, ids: Vec<String>) -> BoxFuture<'_, Result<()>> {
        let store = self.store.clone();
        Box::pin(spawn_blocking(move || store.dismiss(&ids).map_err(io)))
    }

    fn clear(&self) -> BoxFuture<'_, Result<()>> {
        let store = self.store.clone();
        Box::pin(spawn_blocking(move || store.clear().map_err(io)))
    }

    fn directory(&self) -> Option<PathBuf> {
        self.store.dir().map(Path::to_path_buf)
    }
}

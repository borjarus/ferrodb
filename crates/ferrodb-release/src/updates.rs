//! Signed application updates ([`UpdateService`]).
//!
//! `check` fetches `<manifest>` and `<manifest>.sig` from the URL fixed at
//! build time, verifies the Ed25519 signature with the embedded public key
//! and picks the newest release for this platform and channel
//! ([`crate::manifest::select`]). `download` streams the package to
//! `<data>/updates/`, with progress and cancellation, and accepts it only
//! when its size and SHA-256 match the signed manifest. `install` starts the
//! platform installer; the caller then quits:
//!
//! | Platform | Package | `install` |
//! |---|---|---|
//! | Windows | NSIS `…-setup.exe` | runs the installer (its normal UI: it closes / replaces the running app) |
//! | macOS | `.dmg` | opens the disk image in Finder (the user drags the app to Applications) |
//! | Linux | `.AppImage` | when running from an AppImage (`$APPIMAGE`): replaces that file and starts it; otherwise opens the folder |
//! | Linux | `.deb` | opens it with the system package installer (`xdg-open`) |

use std::{
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

use ferrodb_api::{
    BoxFuture, CancelHandle, Error, Result, UpdateDownload, UpdateEvent, UpdateInfo, UpdateService,
    UpdateSettings,
};
use sha2::{Digest, Sha256};

use crate::{ReleaseOptions, http, manifest, settings::SettingsFile, spawn_blocking};

/// Error of [`UpdateService::check`] without a build-time manifest URL.
pub const NO_UPDATE_URL: &str = "no update URL configured in this build";
/// Error without a build-time public key.
pub const NO_UPDATE_KEY: &str = "no update signing key embedded in this build";
/// Automatic checks: at most once in this many seconds.
pub const CHECK_INTERVAL_SECS: i64 = 24 * 60 * 60;
const MANIFEST_LIMIT: u64 = 1 << 20;

pub struct Updates {
    options: ReleaseOptions,
    settings: Arc<SettingsFile>,
}

impl Updates {
    pub fn new(options: ReleaseOptions, settings: Arc<SettingsFile>) -> Self {
        Self { options, settings }
    }

    /// Where packages are downloaded.
    pub fn download_dir(&self) -> PathBuf {
        match &self.options.data_dir {
            Some(d) => d.join("updates"),
            None => std::env::temp_dir().join("ferrodb-updates"),
        }
    }

    /// Blocking check (fetch, verify, select).
    pub fn check_blocking(&self) -> std::result::Result<Option<UpdateInfo>, String> {
        let url = self
            .options
            .update_url
            .clone()
            .ok_or_else(|| NO_UPDATE_URL.to_owned())?;
        let key = self
            .options
            .public_key
            .clone()
            .ok_or_else(|| NO_UPDATE_KEY.to_owned())?;
        http::check_url(&url, self.options.allow_loopback_http)?;
        let body =
            http::get_bytes(&url, MANIFEST_LIMIT)?.ok_or_else(|| format!("{url}: not found"))?;
        let sig = http::get_bytes(&format!("{url}.sig"), 4096)?;
        let sig = sig.map(|s| String::from_utf8_lossy(&s).into_owned());
        let manifest = manifest::verify(&body, sig.as_deref(), &key).map_err(|e| e.to_string())?;
        let settings = self.settings.get().updates;
        let found = manifest::select(
            &manifest,
            &self.options.version,
            settings.channel,
            &self.options.target,
        );
        if let Some(u) = &found {
            // The package URL must be as trustworthy as the manifest's.
            http::check_url(&u.url, self.options.allow_loopback_http)?;
        }
        Ok(found)
    }
}

fn now_secs() -> i64 {
    chrono::Utc::now().timestamp()
}

/// File name for a package URL (last path segment, sanitized).
fn package_name(update: &UpdateInfo) -> String {
    let last = update
        .url
        .split(['?', '#'])
        .next()
        .unwrap_or("")
        .rsplit('/')
        .next()
        .unwrap_or("");
    let clean: String = last
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '+'))
        .collect();
    let clean = clean.trim_start_matches('.').to_owned();
    if clean.is_empty() {
        format!("ferrodb-{}.pkg", update.version)
    } else {
        clean
    }
}

/// Downloads `update.url` into `dir`, verifying size and SHA-256.
pub fn download_package(
    update: &UpdateInfo,
    dir: &Path,
    allow_loopback_http: bool,
    cancel: &CancelHandle,
    progress: &dyn Fn(u64, u64),
) -> std::result::Result<PathBuf, String> {
    http::check_url(&update.url, allow_loopback_http)?;
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let dest = dir.join(package_name(update));
    let part = dir.join(format!("{}.part", package_name(update)));
    let r = (|| {
        let mut resp = http::agent()
            .get(&update.url)
            .call()
            .map_err(|e| format!("{}: {e}", update.url))?;
        // A little slack over the signed size: a longer body fails the hash.
        let mut body = resp
            .body_mut()
            .with_config()
            .limit(update.size.saturating_add(1 << 16))
            .reader();
        let mut out = File::create(&part).map_err(|e| e.to_string())?;
        let mut h = Sha256::new();
        let mut buf = vec![0u8; 1 << 16];
        let mut done = 0u64;
        progress(0, update.size);
        loop {
            if cancel.is_cancelled() {
                return Err("cancelled".to_owned());
            }
            let n = body
                .read(&mut buf)
                .map_err(|e| format!("{}: {e}", update.url))?;
            if n == 0 {
                break;
            }
            h.update(&buf[..n]);
            out.write_all(&buf[..n]).map_err(|e| e.to_string())?;
            done += n as u64;
            progress(done, update.size);
        }
        out.flush().map_err(|e| e.to_string())?;
        drop(out);
        if done != update.size {
            return Err(format!(
                "the package has {done} bytes, the signed manifest says {}",
                update.size
            ));
        }
        let got: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
        if !got.eq_ignore_ascii_case(&update.sha256) {
            return Err(format!(
                "checksum mismatch: expected {}, got {got} (package rejected)",
                update.sha256
            ));
        }
        let _ = fs::remove_file(&dest);
        fs::rename(&part, &dest).map_err(|e| e.to_string())?;
        Ok(dest.clone())
    })();
    if r.is_err() {
        let _ = fs::remove_file(&part);
    }
    r
}

/// What `install` does with `package` on this platform (pure: testable).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InstallPlan {
    /// Start a program with arguments, don't wait.
    Run { program: PathBuf, args: Vec<String> },
    /// Replace the running AppImage with the package, then start it.
    ReplaceAppImage { target: PathBuf },
}

pub fn install_plan(
    os: &str,
    package: &Path,
    appimage: Option<PathBuf>,
) -> std::result::Result<InstallPlan, String> {
    let name = package
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let pkg = package.display().to_string();
    match os {
        "windows" if name.ends_with(".exe") || name.ends_with(".msi") => Ok(InstallPlan::Run {
            program: package.to_path_buf(),
            args: Vec::new(),
        }),
        "macos" if name.ends_with(".dmg") => Ok(InstallPlan::Run {
            program: "open".into(),
            args: vec![pkg],
        }),
        "linux" if name.ends_with(".appimage") => match appimage {
            Some(target) => Ok(InstallPlan::ReplaceAppImage { target }),
            None => Ok(InstallPlan::Run {
                program: "xdg-open".into(),
                args: vec![
                    package
                        .parent()
                        .map(|p| p.display().to_string())
                        .unwrap_or(pkg),
                ],
            }),
        },
        "linux" if name.ends_with(".deb") || name.ends_with(".rpm") => Ok(InstallPlan::Run {
            program: "xdg-open".into(),
            args: vec![pkg],
        }),
        _ => Err(format!("cannot install {name} on {os}")),
    }
}

fn run_plan(plan: InstallPlan, package: &Path) -> std::result::Result<(), String> {
    match plan {
        InstallPlan::Run { program, args } => std::process::Command::new(&program)
            .args(&args)
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("{}: {e}", program.display())),
        InstallPlan::ReplaceAppImage { target } => {
            // Copy next to the target, then rename over it (atomic on one
            // file system; the running image stays mapped until exit).
            let tmp = target.with_extension("AppImage.new");
            fs::copy(package, &tmp).map_err(|e| e.to_string())?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                fs::set_permissions(&tmp, fs::Permissions::from_mode(0o755))
                    .map_err(|e| e.to_string())?;
            }
            fs::rename(&tmp, &target).map_err(|e| e.to_string())?;
            std::process::Command::new(&target)
                .spawn()
                .map(|_| ())
                .map_err(|e| format!("{}: {e}", target.display()))
        }
    }
}

impl UpdateService for Updates {
    fn current_version(&self) -> String {
        self.options.version.clone()
    }

    fn settings(&self) -> UpdateSettings {
        self.settings.get().updates
    }

    fn save_settings(&self, settings: UpdateSettings) -> BoxFuture<'_, Result<()>> {
        let file = self.settings.clone();
        Box::pin(spawn_blocking(move || {
            file.update(|s| s.updates = settings)
                .map_err(|e| Error::Other(e.to_string()))
        }))
    }

    fn check(&self) -> BoxFuture<'_, Result<Option<UpdateInfo>>> {
        let this = Updates {
            options: self.options.clone(),
            settings: self.settings.clone(),
        };
        Box::pin(spawn_blocking(move || {
            let r = this.check_blocking();
            // Any attempt counts: a failing server is not hammered.
            if this.options.update_url.is_some() {
                let _ = this
                    .settings
                    .update(|s| s.last_update_check = Some(now_secs()));
            }
            match &r {
                Ok(Some(u)) => tracing::info!(version = %u.version, "update available"),
                Ok(None) => tracing::info!("no update available"),
                Err(e) => tracing::warn!(error = %e, "update check failed"),
            }
            r.map_err(Error::Other)
        }))
    }

    fn check_due(&self) -> bool {
        let s = self.settings.get();
        self.options.update_url.is_some()
            && self.options.public_key.is_some()
            && s.updates.check_automatically
            && s.last_update_check
                .is_none_or(|t| now_secs() - t >= CHECK_INTERVAL_SECS || t > now_secs())
    }

    fn download(&self, update: UpdateInfo) -> UpdateDownload {
        let (tx, rx) = async_channel::unbounded();
        let cancel = CancelHandle::new();
        let dir = self.download_dir();
        let allow = self.options.allow_loopback_http;
        let c = cancel.clone();
        std::thread::Builder::new()
            .name("update-download".into())
            .spawn(move || {
                let progress = |done: u64, total: u64| {
                    let _ = tx.try_send(UpdateEvent::Progress { done, total });
                };
                let ev = match download_package(&update, &dir, allow, &c, &progress) {
                    Ok(path) => {
                        tracing::info!(path = %path.display(), "update downloaded and verified");
                        UpdateEvent::Ready(path)
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "update download failed");
                        UpdateEvent::Failed(e)
                    }
                };
                let _ = tx.try_send(ev);
            })
            .ok();
        UpdateDownload { events: rx, cancel }
    }

    fn install(&self, package: PathBuf) -> BoxFuture<'_, Result<()>> {
        let dir = self.download_dir();
        Box::pin(spawn_blocking(move || {
            // Only packages this service downloaded and verified.
            let inside = package
                .canonicalize()
                .ok()
                .zip(dir.canonicalize().ok())
                .is_some_and(|(p, d)| p.starts_with(d));
            if !inside {
                return Err(Error::Other(format!(
                    "{} is not a verified update package",
                    package.display()
                )));
            }
            let appimage = std::env::var_os("APPIMAGE").map(PathBuf::from);
            let plan =
                install_plan(std::env::consts::OS, &package, appimage).map_err(Error::Other)?;
            tracing::info!(?plan, "starting the update installer");
            run_plan(plan, &package).map_err(Error::Other)
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_plans() {
        let p = Path::new("/d/FerroDb_1.0.0_x64-setup.exe");
        assert_eq!(
            install_plan("windows", p, None),
            Ok(InstallPlan::Run {
                program: p.into(),
                args: vec![]
            })
        );
        let dmg = Path::new("/d/FerroDb_1.0.0_aarch64.dmg");
        assert!(matches!(
            install_plan("macos", dmg, None),
            Ok(InstallPlan::Run { program, .. }) if program == Path::new("open")
        ));
        let ai = Path::new("/d/FerroDb_1.0.0_x86_64.AppImage");
        assert_eq!(
            install_plan("linux", ai, Some("/opt/FerroDb.AppImage".into())),
            Ok(InstallPlan::ReplaceAppImage {
                target: "/opt/FerroDb.AppImage".into()
            })
        );
        let deb = Path::new("/d/ferrodb_1.0.0_amd64.deb");
        assert!(matches!(
            install_plan("linux", deb, None),
            Ok(InstallPlan::Run { program, .. }) if program == Path::new("xdg-open")
        ));
        assert!(install_plan("windows", dmg, None).is_err());
    }

    #[test]
    fn package_names_are_sanitized() {
        let mut u = UpdateInfo {
            version: "1.0.0".into(),
            channel: ferrodb_api::UpdateChannel::Stable,
            date: String::new(),
            notes: String::new(),
            url: "https://x/a/FerroDb_1.0.0_x64-setup.exe?sig=1".into(),
            sha256: String::new(),
            size: 0,
        };
        assert_eq!(package_name(&u), "FerroDb_1.0.0_x64-setup.exe");
        u.url = "https://x/../..".into();
        assert_eq!(package_name(&u), "ferrodb-1.0.0.pkg");
    }
}

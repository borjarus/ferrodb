//! Release automation (phase 10): the update signing key, the signed update
//! manifest, and code signing of the packages.
//!
//! Environment (all optional; a step that is not configured is skipped with
//! a warning, so `cargo xtask package` keeps working on a developer machine):
//!
//! | Variable | Used by |
//! |---|---|
//! | `FERRODB_RELEASE_KEY_FILE` | `release-manifest`: Ed25519 PKCS#8 key (base64 text or DER) |
//! | `FERRODB_SIGN_CERT` + `FERRODB_SIGN_CERT_PASSWORD` | Windows: `.pfx` for `signtool` |
//! | `FERRODB_SIGN_CERT_SHA1` | Windows: certificate thumbprint in the store (instead of a `.pfx`) |
//! | `FERRODB_SIGN_TIMESTAMP_URL` | Windows: RFC 3161 server (default DigiCert) |
//! | `FERRODB_MACOS_SIGN_IDENTITY` | macOS: `Developer ID Application: … (TEAMID)` |
//! | `FERRODB_NOTARY_APPLE_ID` + `FERRODB_NOTARY_TEAM_ID` + `FERRODB_NOTARY_PASSWORD` | macOS: notarization with an app-specific password |
//! | `FERRODB_NOTARY_KEY` + `FERRODB_NOTARY_KEY_ID` + `FERRODB_NOTARY_ISSUER` | macOS: notarization with an App Store Connect API key (`.p8` path) |
//! | `FERRODB_GPG_KEY` | Linux: key id for detached `.asc` signatures |

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
use ferrodb_api::UpdateChannel;
use ferrodb_release::manifest::{self, Manifest, Package, Release};
use sha2::{Digest, Sha256};

use crate::{root, run};

fn var(name: &str) -> Option<String> {
    env::var(name).ok().filter(|v| !v.trim().is_empty())
}

/// Parses `--name value` options.
fn opt(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn is_inside(path: &Path, dir: &Path) -> bool {
    let canon = |p: &Path| {
        p.canonicalize().unwrap_or_else(|_| {
            // Not created yet: canonicalize the parent.
            p.parent()
                .and_then(|d| d.canonicalize().ok())
                .map(|d| d.join(p.file_name().unwrap_or_default()))
                .unwrap_or_else(|| p.to_path_buf())
        })
    };
    canon(path).starts_with(canon(dir))
}

// ------------------------------------------------------------------ keys

/// `release-keygen [--out <file>]`: a new Ed25519 key. The private key
/// (PKCS#8, base64) goes to a file outside the repository; the public key
/// is printed for `FERRODB_UPDATE_PUBKEY`.
pub fn keygen(args: &[String]) -> Result<(), String> {
    let out = opt(args, "--out").map(PathBuf::from).unwrap_or_else(|| {
        let home = env::var_os("USERPROFILE")
            .or_else(|| env::var_os("HOME"))
            .map(PathBuf::from)
            .unwrap_or_else(env::temp_dir);
        home.join(".ferrodb-release").join("update-key.pk8.b64")
    });
    if is_inside(&out, &root()) {
        return Err(format!(
            "{} is inside the repository: keep the private key elsewhere",
            out.display()
        ));
    }
    if out.exists() {
        return Err(format!("{} exists already; not overwriting", out.display()));
    }
    let (pkcs8, public) = manifest::generate_key().map_err(|e| e.to_string())?;
    if let Some(dir) = out.parent() {
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    fs::write(&out, B64.encode(&pkcs8) + "\n").map_err(|e| e.to_string())?;
    eprintln!("private key written to {}", out.display());
    eprintln!("store it as the CI secret FERRODB_RELEASE_KEY; never commit it");
    eprintln!("public key (FERRODB_UPDATE_PUBKEY):");
    println!("{public}");
    Ok(())
}

fn read_key(path: &Path) -> Result<Vec<u8>, String> {
    let bytes = fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let text = String::from_utf8_lossy(&bytes);
    Ok(B64.decode(text.trim()).unwrap_or(bytes))
}

// ------------------------------------------------------------------ manifest

/// Manifest platform key of a package file name (`None` = not an update
/// package: `.deb`, signatures…).
pub fn package_target(name: &str) -> Option<String> {
    let lower = name.to_ascii_lowercase();
    let arch = if lower.contains("aarch64") || lower.contains("arm64") {
        "aarch64"
    } else {
        "x86_64"
    };
    let os = if lower.ends_with("-setup.exe") || lower.ends_with(".msi") {
        "windows"
    } else if lower.ends_with(".dmg") {
        "macos"
    } else if lower.ends_with(".appimage") {
        "linux"
    } else {
        return None;
    };
    Some(format!("{os}-{arch}"))
}

fn sha256_file(path: &Path) -> Result<(String, u64), String> {
    let bytes = fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let hex = Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    Ok((hex, bytes.len() as u64))
}

/// `release-manifest --version 1.0.0 --channel stable --notes "…" | --notes-file F
/// --dir <packages> --base-url <url of the release assets> [--out manifest.json] [--date YYYY-MM-DD]`
pub fn manifest_cmd(args: &[String]) -> Result<(), String> {
    let version = opt(args, "--version").ok_or("--version is required")?;
    if ferrodb_release::version::Version::parse(&version).is_none() {
        return Err(format!("`{version}` is not a semantic version"));
    }
    let channel = match opt(args, "--channel").as_deref().unwrap_or("stable") {
        "stable" => UpdateChannel::Stable,
        "beta" => UpdateChannel::Beta,
        other => return Err(format!("unknown channel `{other}` (stable | beta)")),
    };
    let notes = match (opt(args, "--notes"), opt(args, "--notes-file")) {
        (Some(n), _) => n,
        (None, Some(f)) => fs::read_to_string(&f).map_err(|e| format!("{f}: {e}"))?,
        (None, None) => String::new(),
    };
    let dir = PathBuf::from(opt(args, "--dir").ok_or("--dir is required")?);
    let base = opt(args, "--base-url").ok_or("--base-url is required")?;
    let base = base.trim_end_matches('/');
    let out = PathBuf::from(opt(args, "--out").unwrap_or_else(|| "manifest.json".into()));
    let date =
        opt(args, "--date").unwrap_or_else(|| chrono::Utc::now().format("%Y-%m-%d").to_string());
    let key_file = var("FERRODB_RELEASE_KEY_FILE")
        .ok_or("FERRODB_RELEASE_KEY_FILE is not set (see `cargo xtask release-keygen`)")?;
    let key = read_key(Path::new(&key_file))?;

    let mut platforms = std::collections::BTreeMap::new();
    let mut entries: Vec<_> = fs::read_dir(&dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .collect();
    entries.sort();
    for path in entries {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let Some(target) = package_target(name) else {
            continue;
        };
        let (sha256, size) = sha256_file(&path)?;
        eprintln!("{target}: {name} ({size} bytes, sha256 {sha256})");
        platforms.insert(
            target,
            Package {
                url: format!("{base}/{name}"),
                sha256,
                size,
            },
        );
    }
    if platforms.is_empty() {
        return Err(format!("no update packages in {}", dir.display()));
    }
    let mut m: Manifest = match fs::read(&out) {
        Ok(b) => serde_json::from_slice(&b).map_err(|e| format!("{}: {e}", out.display()))?,
        Err(_) => Manifest {
            schema: manifest::SCHEMA,
            releases: Vec::new(),
        },
    };
    manifest::merge(
        &mut m,
        Release {
            version,
            channel,
            date,
            notes,
            platforms,
        },
    );
    let body = serde_json::to_vec_pretty(&m).map_err(|e| e.to_string())?;
    let sig = manifest::sign(&body, &key).map_err(|e| e.to_string())?;
    // Self-check with the public key of the same key.
    let public = manifest::public_key_of(&key).map_err(|e| e.to_string())?;
    let public = manifest::decode_public_key(&public).map_err(|e| e.to_string())?;
    manifest::verify(&body, Some(&sig), &public).map_err(|e| e.to_string())?;
    fs::write(&out, &body).map_err(|e| e.to_string())?;
    let sig_path = PathBuf::from(format!("{}.sig", out.display()));
    fs::write(&sig_path, sig + "\n").map_err(|e| e.to_string())?;
    eprintln!("wrote {} and {}", out.display(), sig_path.display());
    Ok(())
}

// ------------------------------------------------------------------ signing

fn warn(msg: &str) {
    eprintln!("xtask: warning: {msg}");
}

/// Authenticode with `signtool` (Windows SDK).
pub fn sign_windows(files: &[PathBuf]) -> Result<(), String> {
    let cert = var("FERRODB_SIGN_CERT");
    let thumb = var("FERRODB_SIGN_CERT_SHA1");
    if cert.is_none() && thumb.is_none() {
        warn("Authenticode signing skipped (FERRODB_SIGN_CERT / FERRODB_SIGN_CERT_SHA1 not set)");
        return Ok(());
    }
    let ts =
        var("FERRODB_SIGN_TIMESTAMP_URL").unwrap_or_else(|| "http://timestamp.digicert.com".into());
    for f in files {
        let mut cmd = Command::new("signtool");
        cmd.args(["sign", "/fd", "sha256", "/tr", &ts, "/td", "sha256"]);
        match (&cert, &thumb) {
            (Some(pfx), _) => {
                cmd.args(["/f", pfx]);
                if let Some(pw) = var("FERRODB_SIGN_CERT_PASSWORD") {
                    cmd.args(["/p", &pw]);
                }
            }
            (None, Some(sha1)) => {
                cmd.args(["/sha1", sha1]);
            }
            (None, None) => unreachable!(),
        }
        // The command line holds the password: don't echo it.
        let status = cmd
            .arg(f)
            .current_dir(root())
            .status()
            .map_err(|e| format!("signtool: {e}"))?;
        if !status.success() {
            return Err(format!("signtool failed for {}", f.display()));
        }
        run(Command::new("signtool")
            .args(["verify", "/pa", "/v"])
            .arg(f))?;
    }
    Ok(())
}

/// `codesign` with the hardened runtime and a secure timestamp.
pub fn codesign_macos(identity: &str, path: &Path) -> Result<(), String> {
    run(Command::new("codesign")
        .args(["--force", "--options", "runtime", "--timestamp", "--sign"])
        .arg(identity)
        .arg(path))?;
    run(Command::new("codesign")
        .args(["--verify", "--deep", "--strict", "--verbose=2"])
        .arg(path))
}

/// Notarizes and staples `path` (a `.dmg`); skipped without credentials.
pub fn notarize_macos(path: &Path) -> Result<(), String> {
    let mut cmd = Command::new("xcrun");
    cmd.args(["notarytool", "submit"]).arg(path).arg("--wait");
    if let (Some(id), Some(team), Some(pw)) = (
        var("FERRODB_NOTARY_APPLE_ID"),
        var("FERRODB_NOTARY_TEAM_ID"),
        var("FERRODB_NOTARY_PASSWORD"),
    ) {
        cmd.args(["--apple-id", &id, "--team-id", &team, "--password", &pw]);
    } else if let (Some(key), Some(key_id), Some(issuer)) = (
        var("FERRODB_NOTARY_KEY"),
        var("FERRODB_NOTARY_KEY_ID"),
        var("FERRODB_NOTARY_ISSUER"),
    ) {
        cmd.args(["--key", &key, "--key-id", &key_id, "--issuer", &issuer]);
    } else {
        warn("notarization skipped (FERRODB_NOTARY_* not set)");
        return Ok(());
    }
    eprintln!(
        "$ xcrun notarytool submit {} --wait (credentials hidden)",
        path.display()
    );
    let status = cmd
        .current_dir(root())
        .status()
        .map_err(|e| format!("notarytool: {e}"))?;
    if !status.success() {
        return Err(format!("notarization of {} failed", path.display()));
    }
    run(Command::new("xcrun").args(["stapler", "staple"]).arg(path))?;
    run(Command::new("xcrun")
        .args(["stapler", "validate"])
        .arg(path))
}

/// Detached ASCII-armoured GPG signatures (`<file>.asc`).
pub fn gpg_sign(files: &[PathBuf]) -> Result<(), String> {
    let Some(key) = var("FERRODB_GPG_KEY") else {
        warn("GPG signatures skipped (FERRODB_GPG_KEY not set)");
        return Ok(());
    };
    for f in files {
        run(Command::new("gpg")
            .args([
                "--batch",
                "--yes",
                "--armor",
                "--detach-sign",
                "--local-user",
                &key,
            ])
            .arg(f))?;
    }
    Ok(())
}

/// Files in `dir` with one of `extensions` (case-insensitive).
pub fn files_with(dir: &Path, extensions: &[&str]) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| {
                    let n = p
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("")
                        .to_ascii_lowercase();
                    extensions.iter().any(|e| n.ends_with(e))
                })
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_targets() {
        assert_eq!(
            package_target("FerroDb_1.0.0_x64-setup.exe").as_deref(),
            Some("windows-x86_64")
        );
        assert_eq!(
            package_target("FerroDb_1.0.0_aarch64.dmg").as_deref(),
            Some("macos-aarch64")
        );
        assert_eq!(
            package_target("FerroDb_1.0.0_x64.dmg").as_deref(),
            Some("macos-x86_64")
        );
        assert_eq!(
            package_target("FerroDb_1.0.0_x86_64.AppImage").as_deref(),
            Some("linux-x86_64")
        );
        assert_eq!(package_target("ferrodb_1.0.0_amd64.deb"), None);
        assert_eq!(package_target("FerroDb.exe"), None);
    }

    #[test]
    fn options() {
        let a: Vec<String> = ["--version", "1.0.0", "--dir"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(opt(&a, "--version").as_deref(), Some("1.0.0"));
        assert_eq!(opt(&a, "--dir"), None);
        assert!(is_inside(&root().join("x.key"), &root()));
        assert!(!is_inside(&env::temp_dir().join("x.key"), &root()));
    }
}

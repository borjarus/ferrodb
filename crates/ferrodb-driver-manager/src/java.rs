//! Java runtime for the JDBC bridge: an installed JDK / JRE ≥ the required
//! feature version (`JAVA_HOME`, `PATH`, the usual install locations) or an
//! Eclipse Temurin JRE downloaded from the Adoptium API and verified with the
//! checksum the API publishes.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use serde::{Deserialize, Serialize};

use crate::{
    ManagerError,
    download::{self, Progress},
};

/// A usable Java runtime.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JavaRuntime {
    /// The `java` executable.
    pub java: PathBuf,
    /// Feature version (17, 21…).
    pub feature: u32,
    /// Full version string (`17.0.20.1`).
    pub version: String,
}

fn exe() -> &'static str {
    if cfg!(windows) { "java.exe" } else { "java" }
}

/// Parses `17.0.2`, `1.8.0_392`, `21`, `25-ea` into the feature version.
pub fn feature_version(v: &str) -> Option<u32> {
    let v = v.trim().trim_matches('"');
    let mut parts = v.split(|c: char| !c.is_ascii_digit());
    let first: u32 = parts.next()?.parse().ok()?;
    if first == 1 {
        parts.next()?.parse().ok()
    } else {
        Some(first)
    }
}

/// Version of the runtime at `home` from its `release` file (no process
/// started), else from `java -version`.
fn probe(java: &Path) -> Option<JavaRuntime> {
    if !java.is_file() {
        return None;
    }
    let home = java.parent()?.parent()?;
    let from_release = fs::read_to_string(home.join("release")).ok().and_then(|r| {
        r.lines()
            .find_map(|l| l.strip_prefix("JAVA_VERSION="))
            .map(|v| v.trim().trim_matches('"').to_owned())
    });
    let version = match from_release {
        Some(v) => v,
        None => {
            let out = Command::new(java).arg("-version").output().ok()?;
            let text = String::from_utf8_lossy(&out.stderr).into_owned();
            let line = text.lines().find(|l| l.contains("version"))?;
            line.split('"').nth(1)?.to_owned()
        }
    };
    Some(JavaRuntime {
        java: java.to_owned(),
        feature: feature_version(&version)?,
        version,
    })
}

/// Homes of JDKs installed in the usual places.
fn candidates() -> Vec<PathBuf> {
    let mut homes: Vec<PathBuf> = Vec::new();
    if let Some(h) = std::env::var_os("JAVA_HOME") {
        homes.push(PathBuf::from(h));
    }
    let mut roots: Vec<PathBuf> = Vec::new();
    if cfg!(windows) {
        for var in ["ProgramFiles", "ProgramW6432", "LOCALAPPDATA"] {
            if let Some(p) = std::env::var_os(var) {
                let p = PathBuf::from(p);
                for vendor in [
                    "Eclipse Adoptium",
                    "Java",
                    "Microsoft",
                    "Zulu",
                    "Amazon Corretto",
                    "BellSoft",
                    "Programs/Temurin",
                    "Programs/Eclipse Adoptium",
                ] {
                    roots.push(p.join(vendor));
                }
            }
        }
    } else if cfg!(target_os = "macos") {
        roots.push("/Library/Java/JavaVirtualMachines".into());
        if let Some(h) = std::env::var_os("HOME") {
            roots.push(PathBuf::from(h).join("Library/Java/JavaVirtualMachines"));
        }
    } else {
        roots.push("/usr/lib/jvm".into());
        roots.push("/opt/java".into());
        if let Some(h) = std::env::var_os("HOME") {
            roots.push(PathBuf::from(h).join(".sdkman/candidates/java"));
        }
    }
    if let Some(h) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
        roots.push(PathBuf::from(h).join(".jdks"));
    }
    for r in roots {
        if let Ok(rd) = fs::read_dir(&r) {
            for e in rd.flatten() {
                let p = e.path();
                homes.push(p.join("Contents/Home"));
                homes.push(p);
            }
        }
    }
    homes
}

/// The newest installed runtime with feature version ≥ `min`, preferring
/// `JAVA_HOME`, then the runtimes under `extra` (downloaded ones), then
/// `PATH`, then the usual locations.
pub fn detect(min: u32, extra: &[PathBuf]) -> Option<JavaRuntime> {
    let mut homes: Vec<PathBuf> = Vec::new();
    if let Some(h) = std::env::var_os("JAVA_HOME") {
        homes.push(PathBuf::from(h));
    }
    homes.extend(extra.iter().cloned());
    // `PATH`: the first `java` whose home qualifies.
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            let j = dir.join(exe());
            if j.is_file() {
                let real = fs::canonicalize(&j).unwrap_or(j);
                if let Some(home) = real.parent().and_then(Path::parent) {
                    homes.push(home.to_owned());
                }
            }
        }
    }
    let first_ok = homes
        .iter()
        .filter_map(|h| probe(&h.join("bin").join(exe())))
        .find(|r| r.feature >= min);
    if first_ok.is_some() {
        return first_ok;
    }
    candidates()
        .iter()
        .filter_map(|h| probe(&h.join("bin").join(exe())))
        .filter(|r| r.feature >= min)
        .max_by_key(|r| r.feature)
}

/// Adoptium names of this platform: (os, architecture).
fn adoptium_platform() -> Result<(&'static str, &'static str), ManagerError> {
    let os = match std::env::consts::OS {
        "windows" => "windows",
        "linux" => "linux",
        "macos" => "mac",
        other => {
            return Err(ManagerError::Unsupported(format!(
                "Java download for {other}"
            )));
        }
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "aarch64",
        other => {
            return Err(ManagerError::Unsupported(format!(
                "Java download for {other}"
            )));
        }
    };
    Ok((os, arch))
}

/// One package of the Adoptium "latest assets" answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JrePackage {
    pub url: String,
    pub sha256: String,
    pub name: String,
    pub release: String,
    pub size: Option<u64>,
}

/// Reads `GET /v3/assets/latest/{feature}/hotspot` output.
pub fn parse_adoptium(v: &serde_json::Value) -> Result<JrePackage, ManagerError> {
    let first = v
        .as_array()
        .and_then(|a| a.first())
        .ok_or_else(|| ManagerError::Network("Adoptium: no package for this platform".into()))?;
    let pkg = &first["binary"]["package"];
    let s = |x: &serde_json::Value| x.as_str().map(str::to_owned);
    Ok(JrePackage {
        url: s(&pkg["link"]).ok_or_else(|| ManagerError::Network("Adoptium: no link".into()))?,
        sha256: s(&pkg["checksum"])
            .ok_or_else(|| ManagerError::Network("Adoptium: no checksum".into()))?,
        name: s(&pkg["name"]).unwrap_or_else(|| "jre.zip".into()),
        release: s(&first["release_name"]).unwrap_or_else(|| "jre".into()),
        size: pkg["size"].as_u64(),
    })
}

/// Downloads the latest Temurin JRE of `feature` into `dir` and returns it.
pub fn download_temurin(
    feature: u32,
    dir: &Path,
    progress: Progress<'_>,
) -> Result<JavaRuntime, ManagerError> {
    let (os, arch) = adoptium_platform()?;
    let api = format!(
        "https://api.adoptium.net/v3/assets/latest/{feature}/hotspot?architecture={arch}&image_type=jre&os={os}&vendor=eclipse"
    );
    let pkg = parse_adoptium(&download::get_json(&api)?)?;
    let archive = dir.join("downloads").join(&pkg.name);
    download::download(&pkg.url, &pkg.sha256, &archive, progress)?;
    let target = dir.join(&pkg.release);
    let _ = fs::remove_dir_all(&target);
    let unpacked = dir.join(format!("{}.unpack", pkg.release));
    let _ = fs::remove_dir_all(&unpacked);
    download::extract(&archive, &unpacked, None)?;
    // Archives hold one top directory (`jdk-21.0.5+11-jre`).
    let top = fs::read_dir(&unpacked)?
        .flatten()
        .map(|e| e.path())
        .find(|p| p.is_dir())
        .unwrap_or_else(|| unpacked.clone());
    fs::rename(&top, &target)?;
    let _ = fs::remove_dir_all(&unpacked);
    let _ = fs::remove_file(&archive);
    let home = if target.join("Contents/Home").is_dir() {
        target.join("Contents/Home")
    } else {
        target
    };
    probe(&home.join("bin").join(exe()))
        .ok_or_else(|| ManagerError::Archive("the downloaded JRE has no java executable".into()))
}

/// Homes of runtimes downloaded into `dir`.
pub fn downloaded(dir: &Path) -> Vec<PathBuf> {
    fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| p.is_dir() && !p.ends_with("downloads"))
                .map(|p| {
                    if p.join("Contents/Home").is_dir() {
                        p.join("Contents/Home")
                    } else {
                        p
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feature_versions() {
        assert_eq!(feature_version("17.0.20.1"), Some(17));
        assert_eq!(feature_version("\"1.8.0_392\""), Some(8));
        assert_eq!(feature_version("21"), Some(21));
        assert_eq!(feature_version("25-ea"), Some(25));
        assert_eq!(feature_version("x"), None);
    }

    #[test]
    fn adoptium_answer() {
        let v: serde_json::Value = serde_json::from_str(
            r#"[{"binary":{"package":{"checksum":"ab12","link":"https://github.com/adoptium/temurin21-binaries/releases/download/jdk-21.0.5%2B11/OpenJDK21U-jre_x64_windows_hotspot_21.0.5_11.zip","name":"OpenJDK21U-jre_x64_windows_hotspot_21.0.5_11.zip","size":47000000}},"release_name":"jdk-21.0.5+11","version":{"major":21}}]"#,
        )
        .unwrap();
        let p = parse_adoptium(&v).unwrap();
        assert_eq!(p.sha256, "ab12");
        assert_eq!(p.release, "jdk-21.0.5+11");
        assert!(p.name.ends_with(".zip"));
        assert!(parse_adoptium(&serde_json::json!([])).is_err());
    }

    #[test]
    fn release_file_is_read() {
        let d = tempfile::tempdir().unwrap();
        let bin = d.path().join("bin");
        fs::create_dir_all(&bin).unwrap();
        fs::write(bin.join(exe()), b"").unwrap();
        fs::write(d.path().join("release"), "JAVA_VERSION=\"21.0.5\"\n").unwrap();
        let r = probe(&bin.join(exe())).unwrap();
        assert_eq!((r.feature, r.version.as_str()), (21, "21.0.5"));
        assert_eq!(
            detect(21, &[d.path().to_owned()]).map(|r| r.feature >= 21),
            Some(true)
        );
    }
}

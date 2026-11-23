//! Code signatures of downloaded native code on macOS (plan, phase 10:
//! "signatures and verification for downloaded ADBC drivers and Java
//! runtime on macOS").
//!
//! The SHA-256 from the catalog (ADBC wheels: the digest PyPI publishes;
//! Temurin: the checksum of the Adoptium API) is checked first, always; this
//! module runs after that, on macOS only:
//!
//! | Artifact | Signature found | Action |
//! |---|---|---|
//! | ADBC library (`.dylib` from a PyPI wheel) | Developer ID, verifies | keep, remove `com.apple.quarantine` |
//! | ADBC library | ad-hoc or none (the usual case for Go-built wheels) | **ad-hoc sign** (`codesign -s -`) so Apple Silicon loads it, remove quarantine. The catalog checksum is the trust anchor |
//! | ADBC library | Developer ID that does **not** verify (modified after signing) | reject, delete |
//! | Temurin JRE bundle | Developer ID (Eclipse / Adoptium), `codesign --verify --deep --strict` passes | remove quarantine from the bundle |
//! | Temurin JRE bundle | anything else | reject, delete |
//!
//! Quarantine is removed only from files that passed both checks. Files
//! written by FerroDb itself are not normally quarantined (the attribute is
//! added by browsers and other LSQuarantine apps), but offline installs use
//! files the user downloaded with a browser.
//!
//! The decision table ([`decide`]) and the parsing of `codesign -dv` output
//! ([`parse_signature`]) are pure and tested on every OS; running the tools
//! is compiled on macOS only, elsewhere the functions are no-ops.

use std::path::Path;

use crate::ManagerError;

/// What `codesign -dv` says about a file or bundle.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Signature {
    /// Signed with a certificate (Developer ID…).
    Identity {
        team: Option<String>,
    },
    /// Ad-hoc signature (`Signature=adhoc`): no identity.
    AdHoc,
    Unsigned,
}

/// Parses the (stderr) output of `codesign -dv --verbose=2 <path>`.
pub fn parse_signature(output: &str) -> Signature {
    if output.contains("not signed at all") {
        return Signature::Unsigned;
    }
    if output.lines().any(|l| l.trim() == "Signature=adhoc") {
        return Signature::AdHoc;
    }
    let team = output
        .lines()
        .find_map(|l| l.trim().strip_prefix("TeamIdentifier="))
        .map(str::trim)
        .filter(|t| !t.is_empty() && *t != "not set")
        .map(str::to_owned);
    if team.is_some() || output.lines().any(|l| l.trim().starts_with("Authority=")) {
        Signature::Identity { team }
    } else {
        Signature::Unsigned
    }
}

/// What is being installed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    NativeLibrary,
    JavaRuntime,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Signature fine: remove the quarantine attribute.
    Accept,
    /// Ad-hoc sign, then remove the quarantine attribute.
    AdHocSign,
    Reject(String),
}

/// The decision table of the module documentation.
pub fn decide(target: Target, signature: &Signature, verifies: bool) -> Action {
    match (target, signature) {
        (_, Signature::Identity { .. }) if verifies => Action::Accept,
        (Target::NativeLibrary, Signature::Identity { .. }) => {
            Action::Reject("its code signature does not verify (modified after signing)".into())
        }
        (Target::NativeLibrary, Signature::AdHoc | Signature::Unsigned) => Action::AdHocSign,
        (Target::JavaRuntime, Signature::Identity { .. }) => {
            Action::Reject("the runtime's code signature does not verify".into())
        }
        (Target::JavaRuntime, _) => {
            Action::Reject("the runtime is not signed by its vendor".into())
        }
    }
}

/// Arguments of the strict verification.
pub fn verify_args(path: &Path) -> Vec<String> {
    vec![
        "--verify".into(),
        "--deep".into(),
        "--strict".into(),
        path.display().to_string(),
    ]
}

/// Verifies a downloaded ADBC library (see the table); on rejection the
/// file is deleted. No-op outside macOS.
pub fn trust_native_library(path: &Path) -> Result<(), ManagerError> {
    trust(path, Target::NativeLibrary)
}

/// Verifies a downloaded Java runtime bundle; on rejection the bundle is
/// deleted. No-op outside macOS.
pub fn trust_runtime(bundle: &Path) -> Result<(), ManagerError> {
    trust(bundle, Target::JavaRuntime)
}

#[cfg(not(target_os = "macos"))]
fn trust(_path: &Path, _target: Target) -> Result<(), ManagerError> {
    Ok(())
}

#[cfg(target_os = "macos")]
fn trust(path: &Path, target: Target) -> Result<(), ManagerError> {
    use std::process::Command;

    let run = |cmd: &str, args: &[String]| -> Result<(bool, String), ManagerError> {
        let out = Command::new(cmd)
            .args(args)
            .output()
            .map_err(|e| ManagerError::Untrusted(format!("{cmd}: {e}")))?;
        let mut text = String::from_utf8_lossy(&out.stderr).into_owned();
        text.push_str(&String::from_utf8_lossy(&out.stdout));
        Ok((out.status.success(), text))
    };
    let p = path.display().to_string();
    let (_, info) = run("codesign", &["-dv".into(), "--verbose=2".into(), p.clone()])?;
    let signature = parse_signature(&info);
    let (verifies, detail) = run("codesign", &verify_args(path))?;
    let action = decide(target, &signature, verifies);
    tracing::info!(path = %p, ?signature, verifies, ?action, "code signature checked");
    match action {
        Action::Accept => {}
        Action::AdHocSign => {
            let (ok, out) = run(
                "codesign",
                &["--force".into(), "--sign".into(), "-".into(), p.clone()],
            )?;
            if !ok {
                return Err(ManagerError::Untrusted(format!(
                    "{p}: ad-hoc signing failed: {out}"
                )));
            }
        }
        Action::Reject(why) => {
            if path.is_dir() {
                let _ = std::fs::remove_dir_all(path);
            } else {
                let _ = std::fs::remove_file(path);
            }
            return Err(ManagerError::Untrusted(format!(
                "{p}: {why}. {}",
                detail.trim()
            )));
        }
    }
    if target == Target::JavaRuntime {
        // Informational: Gatekeeper's view of the bundle.
        if let Ok((ok, out)) = run(
            "spctl",
            &[
                "--assess".into(),
                "--type".into(),
                "execute".into(),
                p.clone(),
            ],
        ) {
            tracing::info!(accepted = ok, output = %out.trim(), "spctl assessment of the runtime");
        }
    }
    // Missing attribute = nothing to do; errors are not fatal.
    let _ = run("xattr", &["-dr".into(), "com.apple.quarantine".into(), p]);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEVELOPER: &str = "Executable=/x/Contents/Home/bin/java\nIdentifier=net.temurin.21.jre\nFormat=bundle with Mach-O thin (arm64)\nCodeDirectory v=20500 size=758 flags=0x10000(runtime)\nSignature size=8986\nAuthority=Developer ID Application: Eclipse Foundation, Inc. (JCDTMS22B4)\nAuthority=Developer ID Certification Authority\nAuthority=Apple Root CA\nTimestamp=12 Oct 2027\nTeamIdentifier=JCDTMS22B4\n";
    const ADHOC: &str = "Executable=/x/libadbc_driver_snowflake.dylib\nIdentifier=libadbc_driver_snowflake\nFormat=Mach-O thin (arm64)\nCodeDirectory v=20400 size=1 flags=0x20002(adhoc,linker-signed)\nSignature=adhoc\nTeamIdentifier=not set\n";
    const UNSIGNED: &str = "/x/lib.dylib: code object is not signed at all\n";

    #[test]
    fn signatures_are_parsed() {
        assert_eq!(
            parse_signature(DEVELOPER),
            Signature::Identity {
                team: Some("JCDTMS22B4".into())
            }
        );
        assert_eq!(parse_signature(ADHOC), Signature::AdHoc);
        assert_eq!(parse_signature(UNSIGNED), Signature::Unsigned);
        assert_eq!(parse_signature(""), Signature::Unsigned);
    }

    #[test]
    fn decision_table() {
        let dev = parse_signature(DEVELOPER);
        let lib = Target::NativeLibrary;
        let jre = Target::JavaRuntime;
        assert_eq!(decide(lib, &dev, true), Action::Accept);
        assert!(matches!(decide(lib, &dev, false), Action::Reject(_)));
        assert_eq!(decide(lib, &Signature::AdHoc, false), Action::AdHocSign);
        assert_eq!(decide(lib, &Signature::Unsigned, false), Action::AdHocSign);
        assert_eq!(decide(jre, &dev, true), Action::Accept);
        assert!(matches!(decide(jre, &dev, false), Action::Reject(_)));
        assert!(matches!(
            decide(jre, &Signature::AdHoc, true),
            Action::Reject(_)
        ));
        assert!(matches!(
            decide(jre, &Signature::Unsigned, false),
            Action::Reject(_)
        ));
        assert_eq!(
            verify_args(Path::new("/a/b.dylib")),
            ["--verify", "--deep", "--strict", "/a/b.dylib"]
        );
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn no_op_elsewhere() {
        let dir = std::env::temp_dir();
        assert!(trust_native_library(&dir.join("missing.dylib")).is_ok());
        assert!(trust_runtime(&dir.join("missing-jre")).is_ok());
    }
}

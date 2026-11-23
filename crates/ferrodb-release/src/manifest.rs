//! The update manifest and its Ed25519 signature.
//!
//! ```json
//! {
//!   "schema": 1,
//!   "releases": [
//!     {
//!       "version": "1.0.0",
//!       "channel": "stable",
//!       "date": "2027-12-20",
//!       "notes": "First stable release.",
//!       "platforms": {
//!         "windows-x86_64": { "url": "https://…/FerroDb_1.0.0_x64-setup.exe", "sha256": "…", "size": 41234567 },
//!         "macos-aarch64":  { "url": "https://…/FerroDb_1.0.0_aarch64.dmg", "sha256": "…", "size": 45678901 },
//!         "linux-x86_64":   { "url": "https://…/FerroDb_1.0.0_x86_64.AppImage", "sha256": "…", "size": 52345678 }
//!       }
//!     }
//!   ]
//! }
//! ```
//!
//! One file lists the newest release of each channel (`cargo xtask
//! release-manifest` keeps one entry per channel). The detached signature
//! `<manifest>.sig` is the base64 of the 64-byte Ed25519 signature of the
//! exact bytes of the manifest file; the public key (32 bytes, base64) is
//! embedded in the application at build time.

use std::collections::BTreeMap;

use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
use ferrodb_api::{UpdateChannel, UpdateInfo};
use ring::signature::{self, Ed25519KeyPair, KeyPair as _};
use serde::{Deserialize, Serialize};

use crate::version::Version;

pub const SCHEMA: u32 = 1;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    #[serde(default = "schema")]
    pub schema: u32,
    #[serde(default)]
    pub releases: Vec<Release>,
}

fn schema() -> u32 {
    SCHEMA
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Release {
    pub version: String,
    pub channel: UpdateChannel,
    pub date: String,
    #[serde(default)]
    pub notes: String,
    /// `<os>-<arch>` (Rust names: `windows-x86_64`, `macos-aarch64`…).
    pub platforms: BTreeMap<String, Package>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Package {
    pub url: String,
    pub sha256: String,
    pub size: u64,
}

/// Platform key of the running build.
pub fn current_target() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

#[derive(Debug, PartialEq, Eq)]
pub enum ManifestError {
    MissingSignature,
    BadSignature,
    BadKey,
    Parse(String),
    UnsupportedSchema(u32),
}

impl std::fmt::Display for ManifestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingSignature => f.write_str("the update manifest is not signed"),
            Self::BadSignature => {
                f.write_str("the update manifest signature is invalid (rejected)")
            }
            Self::BadKey => f.write_str("the embedded update key is invalid"),
            Self::Parse(e) => write!(f, "invalid update manifest: {e}"),
            Self::UnsupportedSchema(v) => write!(f, "unsupported update manifest schema {v}"),
        }
    }
}

impl std::error::Error for ManifestError {}

/// Decodes a base64 public key (32 bytes).
pub fn decode_public_key(b64: &str) -> Result<Vec<u8>, ManifestError> {
    let key = B64.decode(b64.trim()).map_err(|_| ManifestError::BadKey)?;
    if key.len() != 32 {
        return Err(ManifestError::BadKey);
    }
    Ok(key)
}

/// Verifies `sig_b64` over `manifest` with `public_key`, then parses it.
/// Nothing of an unsigned or tampered manifest is looked at.
pub fn verify(
    manifest: &[u8],
    sig_b64: Option<&str>,
    public_key: &[u8],
) -> Result<Manifest, ManifestError> {
    let sig_b64 = sig_b64
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or(ManifestError::MissingSignature)?;
    let sig = B64
        .decode(sig_b64)
        .map_err(|_| ManifestError::BadSignature)?;
    signature::UnparsedPublicKey::new(&signature::ED25519, public_key)
        .verify(manifest, &sig)
        .map_err(|_| ManifestError::BadSignature)?;
    let m: Manifest =
        serde_json::from_slice(manifest).map_err(|e| ManifestError::Parse(e.to_string()))?;
    if m.schema != SCHEMA {
        return Err(ManifestError::UnsupportedSchema(m.schema));
    }
    Ok(m)
}

/// Signs `manifest` with an Ed25519 PKCS#8 key; returns the base64 signature.
pub fn sign(manifest: &[u8], pkcs8: &[u8]) -> Result<String, ManifestError> {
    let pair = Ed25519KeyPair::from_pkcs8(pkcs8).map_err(|_| ManifestError::BadKey)?;
    Ok(B64.encode(pair.sign(manifest).as_ref()))
}

/// A new key pair: (PKCS#8 document, base64 public key).
pub fn generate_key() -> Result<(Vec<u8>, String), ManifestError> {
    let rng = ring::rand::SystemRandom::new();
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng).map_err(|_| ManifestError::BadKey)?;
    let pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).map_err(|_| ManifestError::BadKey)?;
    Ok((
        pkcs8.as_ref().to_vec(),
        B64.encode(pair.public_key().as_ref()),
    ))
}

/// The base64 public key of a PKCS#8 key.
pub fn public_key_of(pkcs8: &[u8]) -> Result<String, ManifestError> {
    let pair = Ed25519KeyPair::from_pkcs8(pkcs8).map_err(|_| ManifestError::BadKey)?;
    Ok(B64.encode(pair.public_key().as_ref()))
}

/// Channels a user of `channel` is offered.
pub fn channel_accepts(user: UpdateChannel, release: UpdateChannel) -> bool {
    match user {
        UpdateChannel::Stable => release == UpdateChannel::Stable,
        UpdateChannel::Beta => true,
    }
}

/// The newest release newer than `current` on `channel` with a package for
/// `target`. Downgrades and other channels are never offered; on the stable
/// channel a pre-release version is never offered even if mislabelled.
pub fn select(
    manifest: &Manifest,
    current: &str,
    channel: UpdateChannel,
    target: &str,
) -> Option<UpdateInfo> {
    let current = Version::parse(current)?;
    manifest
        .releases
        .iter()
        .filter(|r| channel_accepts(channel, r.channel))
        .filter_map(|r| Some((Version::parse(&r.version)?, r)))
        .filter(|(v, _)| channel == UpdateChannel::Beta || !v.is_prerelease())
        .filter(|(v, _)| *v > current)
        .filter_map(|(v, r)| Some((v, r, r.platforms.get(target)?)))
        .filter(|(_, _, p)| is_sha256(&p.sha256))
        .max_by(|a, b| a.0.cmp(&b.0))
        .map(|(_, r, p)| UpdateInfo {
            version: r.version.clone(),
            channel: r.channel,
            date: r.date.clone(),
            notes: r.notes.clone(),
            url: p.url.clone(),
            sha256: p.sha256.to_ascii_lowercase(),
            size: p.size,
        })
}

fn is_sha256(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Adds `release` to `manifest`, keeping the newest release per channel.
pub fn merge(manifest: &mut Manifest, release: Release) {
    manifest.releases.retain(|r| r.channel != release.channel);
    manifest.releases.push(release);
    manifest.releases.sort_by(|a, b| {
        let (va, vb) = (Version::parse(&a.version), Version::parse(&b.version));
        vb.cmp(&va)
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pkg(n: u8) -> Package {
        Package {
            url: format!("https://example.invalid/p{n}"),
            sha256: format!("{n:02x}").repeat(32),
            size: 10,
        }
    }

    fn release(v: &str, channel: UpdateChannel, targets: &[&str]) -> Release {
        Release {
            version: v.into(),
            channel,
            date: "2027-12-01".into(),
            notes: format!("notes {v}"),
            platforms: targets.iter().map(|t| (t.to_string(), pkg(1))).collect(),
        }
    }

    fn manifest() -> Manifest {
        Manifest {
            schema: SCHEMA,
            releases: vec![
                release(
                    "1.0.0",
                    UpdateChannel::Stable,
                    &["windows-x86_64", "linux-x86_64"],
                ),
                release("1.1.0-beta.2", UpdateChannel::Beta, &["windows-x86_64"]),
            ],
        }
    }

    #[test]
    fn signature_accepts_and_rejects() {
        let (pkcs8, pub_b64) = generate_key().unwrap();
        let key = decode_public_key(&pub_b64).unwrap();
        let body = serde_json::to_vec_pretty(&manifest()).unwrap();
        let sig = sign(&body, &pkcs8).unwrap();
        assert_eq!(verify(&body, Some(&sig), &key).unwrap(), manifest());
        assert_eq!(public_key_of(&pkcs8).unwrap(), pub_b64);

        // Tampered manifest.
        let mut tampered = body.clone();
        let pos = tampered.iter().position(|b| *b == b'1').unwrap();
        tampered[pos] = b'9';
        assert_eq!(
            verify(&tampered, Some(&sig), &key),
            Err(ManifestError::BadSignature)
        );
        // Wrong key.
        let (_, other) = generate_key().unwrap();
        let other = decode_public_key(&other).unwrap();
        assert_eq!(
            verify(&body, Some(&sig), &other),
            Err(ManifestError::BadSignature)
        );
        // Missing / empty / garbage signature.
        assert_eq!(
            verify(&body, None, &key),
            Err(ManifestError::MissingSignature)
        );
        assert_eq!(
            verify(&body, Some("  \n"), &key),
            Err(ManifestError::MissingSignature)
        );
        assert_eq!(
            verify(&body, Some("not base64!"), &key),
            Err(ManifestError::BadSignature)
        );
        assert_eq!(decode_public_key("AAAA"), Err(ManifestError::BadKey));
    }

    #[test]
    fn channels_and_downgrades() {
        let m = manifest();
        let pick = |cur: &str, ch, t: &str| select(&m, cur, ch, t).map(|u| u.version);
        let (s, b) = (UpdateChannel::Stable, UpdateChannel::Beta);
        assert_eq!(pick("0.9.0", s, "windows-x86_64").as_deref(), Some("1.0.0"));
        assert_eq!(
            pick("0.9.0", b, "windows-x86_64").as_deref(),
            Some("1.1.0-beta.2")
        );
        // Beta sees stable when no newer beta exists for the platform.
        assert_eq!(pick("0.9.0", b, "linux-x86_64").as_deref(), Some("1.0.0"));
        assert_eq!(
            pick("1.0.0-rc.1", s, "linux-x86_64").as_deref(),
            Some("1.0.0")
        );
        // No downgrade, no same version, no unknown platform.
        assert_eq!(pick("1.0.0", s, "windows-x86_64"), None);
        assert_eq!(pick("2.0.0", b, "windows-x86_64"), None);
        assert_eq!(pick("0.9.0", s, "macos-aarch64"), None);

        // A pre-release mislabelled as stable is not offered on stable.
        let mut bad = m.clone();
        bad.releases[1].channel = UpdateChannel::Stable;
        bad.releases.remove(0);
        assert_eq!(select(&bad, "0.9.0", s, "windows-x86_64"), None);
    }

    #[test]
    fn merge_keeps_one_release_per_channel() {
        let mut m = manifest();
        merge(
            &mut m,
            release("1.0.1", UpdateChannel::Stable, &["linux-x86_64"]),
        );
        let versions: Vec<_> = m.releases.iter().map(|r| r.version.as_str()).collect();
        assert_eq!(versions, ["1.1.0-beta.2", "1.0.1"]);
    }
}

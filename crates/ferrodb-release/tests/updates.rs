//! Update check → download → verify against a local HTTP server, with a
//! freshly generated signing key.

mod common;

use std::{path::PathBuf, sync::Arc};

use common::Server;
use ferrodb_api::{UpdateChannel, UpdateEvent, UpdateService as _};
use ferrodb_release::{
    ReleaseOptions, Updates,
    manifest::{self, Manifest, Package, Release, current_target},
    settings::SettingsFile,
};
use futures::executor::block_on;
use sha2::{Digest, Sha256};

struct Fixture {
    server: Server,
    pkcs8: Vec<u8>,
    updates: Updates,
    package: Vec<u8>,
    _config: tempfile::TempDir,
    data: tempfile::TempDir,
}

fn sha(b: &[u8]) -> String {
    Sha256::digest(b)
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect()
}

fn release(version: &str, channel: UpdateChannel, url: String, body: &[u8]) -> Release {
    Release {
        version: version.into(),
        channel,
        date: "2027-12-20".into(),
        notes: format!("Notes of {version}"),
        platforms: [(
            current_target(),
            Package {
                url,
                sha256: sha(body),
                size: body.len() as u64,
            },
        )]
        .into(),
    }
}

impl Fixture {
    fn new() -> Self {
        let server = Server::start();
        let (pkcs8, public) = manifest::generate_key().unwrap();
        let package: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        server.put("/pkg/FerroDb-1.2.0-setup.exe", package.clone());
        let config = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let options = ReleaseOptions {
            config_dir: Some(config.path().into()),
            data_dir: Some(data.path().into()),
            version: "1.0.0".into(),
            target: current_target(),
            update_url: Some(server.url("/manifest.json")),
            public_key: Some(manifest::decode_public_key(&public).unwrap()),
            crash_endpoint: None,
            allow_loopback_http: true,
        };
        let settings = Arc::new(SettingsFile::load(Some(config.path())));
        let f = Fixture {
            updates: Updates::new(options, settings),
            server,
            pkcs8,
            package,
            _config: config,
            data,
        };
        f.publish(&[release(
            "1.2.0",
            UpdateChannel::Stable,
            f.server.url("/pkg/FerroDb-1.2.0-setup.exe"),
            &f.package,
        )]);
        f
    }

    fn publish(&self, releases: &[Release]) {
        let m = Manifest {
            schema: manifest::SCHEMA,
            releases: releases.to_vec(),
        };
        let body = serde_json::to_vec_pretty(&m).unwrap();
        let sig = manifest::sign(&body, &self.pkcs8).unwrap();
        self.server.put("/manifest.json", body);
        self.server.put("/manifest.json.sig", sig);
    }

    fn download(&self, update: ferrodb_api::UpdateInfo) -> Vec<UpdateEvent> {
        let d = self.updates.download(update);
        let mut events = Vec::new();
        while let Ok(ev) = d.events.recv_blocking() {
            let end = !matches!(ev, UpdateEvent::Progress { .. });
            events.push(ev);
            if end {
                break;
            }
        }
        events
    }
}

#[test]
fn check_download_verify_and_reject_a_corrupt_package() {
    let f = Fixture::new();
    assert!(f.updates.check_due());
    let update = block_on(f.updates.check())
        .unwrap()
        .expect("update offered");
    assert_eq!(update.version, "1.2.0");
    assert_eq!(update.notes, "Notes of 1.2.0");
    assert_eq!(update.size, f.package.len() as u64);
    // The attempt is recorded: no automatic check for a day.
    assert!(!f.updates.check_due());

    let events = f.download(update.clone());
    assert!(matches!(
        events.first(),
        Some(UpdateEvent::Progress { done: 0, .. })
    ));
    let Some(UpdateEvent::Ready(path)) = events.last() else {
        panic!("not ready: {events:?}");
    };
    assert_eq!(std::fs::read(path).unwrap(), f.package);
    assert!(path.starts_with(f.data.path().join("updates")));
    let last_progress = events
        .iter()
        .rev()
        .filter_map(|e| match e {
            UpdateEvent::Progress { done, total } => Some((*done, *total)),
            _ => None,
        })
        .next()
        .unwrap();
    assert_eq!(last_progress.0, last_progress.1);

    // The server now hands out a corrupt package of the same size.
    let mut corrupt = f.package.clone();
    corrupt[1234] ^= 0xff;
    f.server.put("/pkg/FerroDb-1.2.0-setup.exe", corrupt);
    let events = f.download(update.clone());
    match events.last() {
        Some(UpdateEvent::Failed(e)) => assert!(e.contains("checksum mismatch"), "{e}"),
        other => panic!("{other:?}"),
    }
    let parts: Vec<PathBuf> = std::fs::read_dir(f.data.path().join("updates"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "part"))
        .collect();
    assert!(parts.is_empty(), "{parts:?}");

    // A truncated one is rejected on size.
    f.server
        .put("/pkg/FerroDb-1.2.0-setup.exe", f.package[..1000].to_vec());
    match f.download(update).last() {
        Some(UpdateEvent::Failed(e)) => assert!(e.contains("bytes"), "{e}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn unsigned_tampered_and_wrongly_signed_manifests_are_rejected() {
    let f = Fixture::new();
    // Missing signature.
    f.server.remove("/manifest.json.sig");
    let e = block_on(f.updates.check()).unwrap_err().to_string();
    assert!(e.contains("not signed"), "{e}");

    // Tampered body (version bumped after signing).
    f.publish(&[release(
        "1.2.0",
        UpdateChannel::Stable,
        f.server.url("/pkg/FerroDb-1.2.0-setup.exe"),
        &f.package,
    )]);
    let body = f.server.files.lock().unwrap()["/manifest.json"].clone();
    let tampered = String::from_utf8(body).unwrap().replace("1.2.0", "9.2.0");
    f.server.put("/manifest.json", tampered);
    let e = block_on(f.updates.check()).unwrap_err().to_string();
    assert!(e.contains("signature is invalid"), "{e}");

    // Signed with another key.
    let (other, _) = manifest::generate_key().unwrap();
    let body = serde_json::to_vec(&Manifest::default()).unwrap();
    f.server
        .put("/manifest.json.sig", manifest::sign(&body, &other).unwrap());
    f.server.put("/manifest.json", body);
    let e = block_on(f.updates.check()).unwrap_err().to_string();
    assert!(e.contains("signature is invalid"), "{e}");
}

#[test]
fn channels_downgrades_and_cancellation() {
    let f = Fixture::new();
    let url = f.server.url("/pkg/FerroDb-1.2.0-setup.exe");
    f.publish(&[
        release("1.2.0", UpdateChannel::Stable, url.clone(), &f.package),
        release("1.3.0-beta.1", UpdateChannel::Beta, url.clone(), &f.package),
    ]);
    let found = || block_on(f.updates.check()).unwrap().map(|u| u.version);
    assert_eq!(found().as_deref(), Some("1.2.0"));

    let mut s = f.updates.settings();
    s.channel = UpdateChannel::Beta;
    block_on(f.updates.save_settings(s)).unwrap();
    assert_eq!(found().as_deref(), Some("1.3.0-beta.1"));
    // Persisted in release.toml.
    let again = SettingsFile::load(Some(f._config.path()));
    assert_eq!(again.get().updates.channel, UpdateChannel::Beta);

    // Only older releases: nothing (no downgrade).
    f.publish(&[release("0.9.0", UpdateChannel::Stable, url, &f.package)]);
    assert_eq!(found(), None);

    // Cancelled before the first chunk.
    let update = ferrodb_api::UpdateInfo {
        version: "1.2.0".into(),
        channel: UpdateChannel::Stable,
        date: String::new(),
        notes: String::new(),
        url: f.server.url("/pkg/FerroDb-1.2.0-setup.exe"),
        sha256: sha(&f.package),
        size: f.package.len() as u64,
    };
    let d = f.updates.download(update);
    d.cancel.cancel();
    let mut last = None;
    while let Ok(ev) = d.events.recv_blocking() {
        last = Some(ev);
    }
    assert!(
        matches!(&last, Some(UpdateEvent::Failed(e)) if e == "cancelled")
            || matches!(&last, Some(UpdateEvent::Ready(_))),
        "{last:?}"
    );
}

#[test]
fn install_refuses_files_it_did_not_download() {
    let f = Fixture::new();
    let elsewhere = f._config.path().join("evil-setup.exe");
    std::fs::write(&elsewhere, b"x").unwrap();
    let e = block_on(f.updates.install(elsewhere))
        .unwrap_err()
        .to_string();
    assert!(e.contains("not a verified update package"), "{e}");
}

#[test]
fn nothing_configured_means_no_checks() {
    let updates = Updates::new(
        ReleaseOptions::in_memory(),
        Arc::new(SettingsFile::load(None)),
    );
    assert!(!updates.check_due());
    let e = block_on(updates.check()).unwrap_err().to_string();
    assert!(e.contains("no update URL configured"), "{e}");
}

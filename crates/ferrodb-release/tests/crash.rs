//! The panic hook writes a masked report; the service lists, sends (to a
//! local endpoint), dismisses and clears reports; consent persists.
//!
//! One test: the panic hook is process-wide.

mod common;

use std::sync::Arc;

use common::Server;
use ferrodb_api::{CrashConsent, CrashReportService as _};
use ferrodb_release::{
    CrashReports, CrashStore, crash, install_panic_hook, logbuf, settings::SettingsFile,
};
use futures::executor::block_on;

#[test]
fn panic_hook_report_service_and_consent() {
    let data = tempfile::tempdir().unwrap();
    let config = tempfile::tempdir().unwrap();
    install_panic_hook(data.path(), "1.0.0-test");
    logbuf::push_line("INFO connecting with Password=hunter2;Server=db");

    let joined = std::thread::Builder::new()
        .name("crash-test".into())
        .spawn(|| {
            let r = std::panic::catch_unwind(|| {
                panic!("connect postgres://app:s3cr3t@db.local/shop failed")
            });
            assert!(r.is_err());
        })
        .unwrap()
        .join();
    // Restore the default hook for whatever runs after this test.
    let _ = std::panic::take_hook();
    assert!(joined.is_ok());

    let dir = crash::crash_dir(data.path());
    let files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .collect();
    assert_eq!(files.len(), 1, "{files:?}");
    let text = std::fs::read_to_string(&files[0]).unwrap();
    assert!(!text.contains("s3cr3t"), "{text}");
    assert!(!text.contains("hunter2"), "{text}");
    let report: ferrodb_api::CrashReport = serde_json::from_str(&text).unwrap();
    assert_eq!(
        report.message,
        "connect postgres://app:***@db.local/shop failed"
    );
    assert_eq!(report.thread.as_deref(), Some("crash-test"));
    assert_eq!(report.app_version, "1.0.0-test");
    assert!(
        report
            .location
            .as_deref()
            .is_some_and(|l| l.contains("crash.rs")),
        "{:?}",
        report.location
    );
    assert!(!report.backtrace.is_empty());
    assert!(report.os.starts_with(std::env::consts::OS));
    assert!(
        report
            .log_tail
            .iter()
            .any(|l| l.contains("Password=***;Server=db")),
        "{:?}",
        report.log_tail
    );
    assert!(!report.sent);

    // The service (created after the crash: the report is from an earlier run).
    let server = Server::start();
    let settings = Arc::new(SettingsFile::load(Some(config.path())));
    let store = Arc::new(CrashStore::new(Some(dir.clone())));
    let without = CrashReports::new(store.clone(), settings.clone(), None, true);
    assert_eq!(without.consent(), CrashConsent::Ask);
    assert_eq!(without.pending().len(), 1);
    let e = block_on(without.send(vec![report.id.clone()], String::new()))
        .unwrap_err()
        .to_string();
    assert!(e.contains(crash::NO_ENDPOINT), "{e}");
    assert_eq!(without.directory(), Some(dir.clone()));

    let service = CrashReports::new(store, settings, Some(server.url("/crash")), true);
    block_on(service.send(
        vec![report.id.clone()],
        "It crashed after connecting, token=abc123".into(),
    ))
    .unwrap();
    let posts = server.posts.lock().unwrap().clone();
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0].0, "/crash");
    let sent: ferrodb_api::CrashReport = serde_json::from_slice(&posts[0].1).unwrap();
    assert_eq!(sent.id, report.id);
    assert_eq!(sent.comment, "It crashed after connecting, token=***");
    assert!(service.pending().is_empty());
    assert!(service.all()[0].sent);

    // Dismissed reports are kept but not offered.
    let mut other = report.clone();
    other.id = "20000101-000000-0001".into();
    other.time = "2000-01-01T00:00:00+00:00".into();
    crash::write_report(&dir, &other).unwrap();
    assert_eq!(service.pending().len(), 1);
    block_on(service.dismiss(vec![other.id.clone()])).unwrap();
    assert!(service.pending().is_empty());
    assert_eq!(service.all().len(), 2);

    // Consent persists in release.toml.
    block_on(service.set_consent(CrashConsent::Never)).unwrap();
    assert_eq!(
        SettingsFile::load(Some(config.path())).get().consent,
        CrashConsent::Never
    );

    block_on(service.clear()).unwrap();
    assert!(service.all().is_empty());
    assert!(std::fs::read_dir(&dir).unwrap().next().is_none());
}

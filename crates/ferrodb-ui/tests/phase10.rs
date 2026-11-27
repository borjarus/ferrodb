//! Phase 10 flows on the deterministic mock with fake update and crash
//! report services: the crash dialog at startup (send, don't send, always
//! send, send failure), consent Always / Never, the "Updates and Privacy"
//! settings page, update available → download → ready → install, cancel,
//! the beta program and the About lines.

use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

use ferrodb_api::{
    Backend, BoxFuture, CancelHandle, CrashConsent, CrashReport, CrashReportService,
    DataSourceService, DriverCatalogService, HistoryService, LanguageService, MetadataService,
    QueryService, SearchIndexService, TransferService, UpdateChannel, UpdateDownload, UpdateEvent,
    UpdateInfo, UpdateService, UpdateSettings,
};
use ferrodb_mock::MockBackend;
use ferrodb_ui::{
    AppState, Workspace,
    ai_settings::LastSettingsViews,
    project::Project,
    release::{self, DownloadState},
    settings_view::{self, SettingsPageId},
};
use ferrodb_ui_kit::kit::WindowExt as _;
use gpui_kit::{
    App, AppContext as _, Bounds, Entity, Point, TestAppContext, Window, WindowBounds,
    WindowHandle, WindowOptions, base::Root, px, size, test::TestWindowExt as _,
};

// ------------------------------------------------------------------ fakes

fn info(version: &str, channel: UpdateChannel) -> UpdateInfo {
    UpdateInfo {
        version: version.into(),
        channel,
        date: "2027-12-20".into(),
        notes: format!("What's new in {version}"),
        url: format!("https://example.invalid/FerroDb_{version}_x64-setup.exe"),
        sha256: "ab".repeat(32),
        size: 1000,
    }
}

struct FakeUpdates {
    settings: Mutex<UpdateSettings>,
    saved: Mutex<Vec<UpdateSettings>>,
    due: bool,
    checks: AtomicUsize,
    /// Events of the running download (the test feeds them).
    download: Mutex<Option<(async_channel::Sender<UpdateEvent>, CancelHandle)>>,
    installed: Mutex<Vec<PathBuf>>,
}

impl FakeUpdates {
    fn new(due: bool) -> Self {
        Self {
            settings: Mutex::new(UpdateSettings::default()),
            saved: Mutex::new(Vec::new()),
            due,
            checks: AtomicUsize::new(0),
            download: Mutex::new(None),
            installed: Mutex::new(Vec::new()),
        }
    }

    fn send(&self, ev: UpdateEvent) {
        let tx = self.download.lock().unwrap().as_ref().unwrap().0.clone();
        tx.try_send(ev).unwrap();
    }

    fn cancelled(&self) -> bool {
        self.download
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|(_, c)| c.is_cancelled())
    }
}

impl UpdateService for FakeUpdates {
    fn current_version(&self) -> String {
        "1.0.0".into()
    }
    fn settings(&self) -> UpdateSettings {
        self.settings.lock().unwrap().clone()
    }
    fn save_settings(&self, settings: UpdateSettings) -> BoxFuture<'_, ferrodb_api::Result<()>> {
        *self.settings.lock().unwrap() = settings.clone();
        self.saved.lock().unwrap().push(settings);
        Box::pin(async { Ok(()) })
    }
    fn check(&self) -> BoxFuture<'_, ferrodb_api::Result<Option<UpdateInfo>>> {
        self.checks.fetch_add(1, Ordering::SeqCst);
        let found = match self.settings.lock().unwrap().channel {
            UpdateChannel::Stable => info("1.1.0", UpdateChannel::Stable),
            UpdateChannel::Beta => info("1.2.0-beta.1", UpdateChannel::Beta),
        };
        Box::pin(async move { Ok(Some(found)) })
    }
    fn check_due(&self) -> bool {
        self.due
    }
    fn download(&self, _update: UpdateInfo) -> UpdateDownload {
        let (tx, rx) = async_channel::unbounded();
        let cancel = CancelHandle::new();
        *self.download.lock().unwrap() = Some((tx, cancel.clone()));
        UpdateDownload { events: rx, cancel }
    }
    fn install(&self, package: PathBuf) -> BoxFuture<'_, ferrodb_api::Result<()>> {
        self.installed.lock().unwrap().push(package);
        Box::pin(async { Ok(()) })
    }
}

fn report(id: &str) -> CrashReport {
    CrashReport {
        id: id.into(),
        time: "2027-11-08T10:15:00+00:00".into(),
        app_version: "1.0.0".into(),
        os: "windows x86_64 10.0.26100".into(),
        thread: Some("main".into()),
        message: "index out of bounds: the len is 3 but the index is 7".into(),
        location: Some("crates/ferrodb-ui/src/results/grid.rs:120:9".into()),
        backtrace: "0: std::panicking::begin_panic".into(),
        log_tail: vec!["INFO connecting to postgres://app:***@db/shop".into()],
        sent: false,
        comment: String::new(),
    }
}

struct FakeCrashes {
    consent: Mutex<CrashConsent>,
    reports: Mutex<Vec<CrashReport>>,
    sent: Mutex<Vec<(Vec<String>, String)>>,
    dismissed: Mutex<Vec<String>>,
    /// Like a build without a crash endpoint.
    fail: AtomicBool,
    cleared: AtomicBool,
}

impl FakeCrashes {
    fn new(consent: CrashConsent, reports: Vec<CrashReport>) -> Self {
        Self {
            consent: Mutex::new(consent),
            reports: Mutex::new(reports),
            sent: Mutex::new(Vec::new()),
            dismissed: Mutex::new(Vec::new()),
            fail: AtomicBool::new(false),
            cleared: AtomicBool::new(false),
        }
    }
}

impl CrashReportService for FakeCrashes {
    fn consent(&self) -> CrashConsent {
        *self.consent.lock().unwrap()
    }
    fn set_consent(&self, consent: CrashConsent) -> BoxFuture<'_, ferrodb_api::Result<()>> {
        *self.consent.lock().unwrap() = consent;
        Box::pin(async { Ok(()) })
    }
    fn pending(&self) -> Vec<CrashReport> {
        let dismissed = self.dismissed.lock().unwrap().clone();
        self.reports
            .lock()
            .unwrap()
            .iter()
            .filter(|r| !r.sent && !dismissed.contains(&r.id))
            .cloned()
            .collect()
    }
    fn all(&self) -> Vec<CrashReport> {
        self.reports.lock().unwrap().clone()
    }
    fn send(&self, ids: Vec<String>, comment: String) -> BoxFuture<'_, ferrodb_api::Result<()>> {
        if self.fail.load(Ordering::SeqCst) {
            return Box::pin(async {
                Err(ferrodb_api::Error::Other(
                    "no crash endpoint configured in this build".into(),
                ))
            });
        }
        for r in self.reports.lock().unwrap().iter_mut() {
            if ids.contains(&r.id) {
                r.sent = true;
            }
        }
        self.sent.lock().unwrap().push((ids, comment));
        Box::pin(async { Ok(()) })
    }
    fn dismiss(&self, ids: Vec<String>) -> BoxFuture<'_, ferrodb_api::Result<()>> {
        self.dismissed.lock().unwrap().extend(ids);
        Box::pin(async { Ok(()) })
    }
    fn clear(&self) -> BoxFuture<'_, ferrodb_api::Result<()>> {
        self.reports.lock().unwrap().clear();
        self.cleared.store(true, Ordering::SeqCst);
        Box::pin(async { Ok(()) })
    }
    fn directory(&self) -> Option<PathBuf> {
        None
    }
}

struct TestBackend {
    mock: MockBackend,
    updates: FakeUpdates,
    crashes: FakeCrashes,
}

impl Backend for TestBackend {
    fn name(&self) -> &str {
        "test"
    }
    fn data_sources(&self) -> &dyn DataSourceService {
        self.mock.data_sources()
    }
    fn drivers(&self) -> &dyn DriverCatalogService {
        self.mock.drivers()
    }
    fn queries(&self) -> &dyn QueryService {
        self.mock.queries()
    }
    fn metadata(&self) -> &dyn MetadataService {
        self.mock.metadata()
    }
    fn language(&self) -> &dyn LanguageService {
        self.mock.language()
    }
    fn history(&self) -> &dyn HistoryService {
        self.mock.history()
    }
    fn search(&self) -> &dyn SearchIndexService {
        self.mock.search()
    }
    fn transfers(&self) -> &dyn TransferService {
        self.mock.transfers()
    }
    fn updates(&self) -> &dyn UpdateService {
        &self.updates
    }
    fn crash_reports(&self) -> &dyn CrashReportService {
        &self.crashes
    }
}

// ------------------------------------------------------------------ harness

struct Harness {
    backend: Arc<TestBackend>,
    window: WindowHandle<Root>,
    _ws: Entity<Workspace>,
    _dir: tempfile::TempDir,
}

fn start(
    cx: &mut TestAppContext,
    consent: CrashConsent,
    reports: Vec<CrashReport>,
    update_due: bool,
) -> Harness {
    let backend = Arc::new(TestBackend {
        mock: MockBackend::deterministic(),
        updates: FakeUpdates::new(update_due),
        crashes: FakeCrashes::new(consent, reports),
    });
    let shared: Arc<dyn Backend> = backend.clone();
    cx.update(|cx: &mut App| {
        ferrodb_ui::init(
            AppState {
                backend: shared,
                scenarios: None,
                paths: None,
            },
            cx,
        );
        cx.set_reduce_motion(true);
    });
    let dir = tempfile::tempdir().unwrap();
    let project = Project::open(dir.path()).unwrap();
    let (window, ws) = cx.update(|cx| {
        let bounds = Bounds {
            origin: Point::default(),
            size: size(px(1400.), px(900.)),
        };
        let (window, ws) = gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            cx,
            |window, cx| cx.new(|cx| Workspace::new(project, window, cx)),
        )
        .expect("open window");
        (window.downcast::<Root>().expect("root"), ws)
    });
    let h = Harness {
        backend,
        window,
        _ws: ws,
        _dir: dir,
    };
    cx.run_until_parked();
    h.frame(cx);
    h
}

impl Harness {
    fn with<R>(&self, cx: &mut TestAppContext, f: impl FnOnce(&mut Window, &mut App) -> R) -> R {
        let r = cx
            .update_window(self.window.into(), |_, window, cx| f(window, cx))
            .unwrap();
        cx.run_until_parked();
        r
    }

    fn frame(&self, cx: &mut TestAppContext) {
        self.with(cx, |w, cx| w.render_frame(cx));
    }

    fn has_dialog(&self, cx: &mut TestAppContext) -> bool {
        self.with(cx, |w, cx| w.has_active_dialog(cx))
    }

    fn visible(&self, cx: &mut TestAppContext, id: &'static str) -> bool {
        self.frame(cx);
        self.with(cx, |w, _| w.find(id).visible())
    }

    fn crash_dialog(&self, cx: &mut TestAppContext) -> Entity<release::CrashDialog> {
        cx.update(|cx| {
            release::hub(cx)
                .and_then(|h| h.crash_dialog.clone())
                .expect("crash dialog open")
        })
    }

    fn download_state(&self, cx: &mut TestAppContext) -> DownloadState {
        cx.update(|cx| release::hub(cx).unwrap().download.clone())
    }
}

// ------------------------------------------------------------------ crash reports

#[gpui_kit::test]
async fn crash_dialog_sends_with_a_comment(cx: &mut TestAppContext) {
    let h = start(cx, CrashConsent::Ask, vec![report("r1")], false);
    assert!(h.has_dialog(cx));
    assert!(h.visible(cx, "crash-dialog"));
    let d = h.crash_dialog(cx);
    h.with(cx, |w, cx| {
        d.update(cx, |d, cx| {
            d.toggle("r1", cx);
            d.set_comment("Scrolling a large result", w, cx);
        })
    });
    cx.update(|cx| assert!(d.read(cx).expanded.contains("r1")));
    cx.update(|cx| assert!(d.read(cx).report_text().contains("index out of bounds")));
    h.with(cx, |w, cx| d.update(cx, |d, cx| d.send(false, w, cx)));
    h.frame(cx);
    assert_eq!(
        *h.backend.crashes.sent.lock().unwrap(),
        vec![(
            vec!["r1".to_string()],
            "Scrolling a large result".to_string()
        )]
    );
    assert!(!h.has_dialog(cx));
    assert_eq!(
        *h.backend.crashes.consent.lock().unwrap(),
        CrashConsent::Ask
    );
}

#[gpui_kit::test]
async fn crash_dialog_dont_send_dismisses(cx: &mut TestAppContext) {
    let h = start(
        cx,
        CrashConsent::Ask,
        vec![report("r1"), report("r2")],
        false,
    );
    let d = h.crash_dialog(cx);
    cx.update(|cx| assert_eq!(d.read(cx).reports.len(), 2));
    h.with(cx, |w, cx| d.update(cx, |d, cx| d.dont_send(w, cx)));
    h.frame(cx);
    assert!(!h.has_dialog(cx));
    assert!(h.backend.crashes.sent.lock().unwrap().is_empty());
    assert_eq!(*h.backend.crashes.dismissed.lock().unwrap(), ["r1", "r2"]);
}

#[gpui_kit::test]
async fn crash_dialog_always_send_changes_consent(cx: &mut TestAppContext) {
    let h = start(cx, CrashConsent::Ask, vec![report("r1")], false);
    let d = h.crash_dialog(cx);
    h.with(cx, |w, cx| d.update(cx, |d, cx| d.send(true, w, cx)));
    h.frame(cx);
    assert!(!h.has_dialog(cx));
    assert_eq!(
        *h.backend.crashes.consent.lock().unwrap(),
        CrashConsent::Always
    );
    assert_eq!(h.backend.crashes.sent.lock().unwrap().len(), 1);
}

#[gpui_kit::test]
async fn crash_dialog_offers_copy_when_sending_fails(cx: &mut TestAppContext) {
    let h = start(cx, CrashConsent::Ask, vec![report("r1")], false);
    h.backend.crashes.fail.store(true, Ordering::SeqCst);
    let d = h.crash_dialog(cx);
    h.with(cx, |w, cx| d.update(cx, |d, cx| d.send(false, w, cx)));
    // Still open, with the error and copy / open folder.
    assert!(h.has_dialog(cx));
    assert!(h.visible(cx, "crash-send-error"));
    cx.update(|cx| {
        assert!(
            d.read(cx)
                .error
                .as_deref()
                .is_some_and(|e| e.contains("no crash endpoint"))
        )
    });
}

#[gpui_kit::test]
async fn consent_always_sends_silently(cx: &mut TestAppContext) {
    let h = start(cx, CrashConsent::Always, vec![report("r1")], false);
    assert!(!h.has_dialog(cx));
    assert_eq!(
        *h.backend.crashes.sent.lock().unwrap(),
        vec![(vec!["r1".to_string()], String::new())]
    );
}

#[gpui_kit::test]
async fn consent_never_does_nothing(cx: &mut TestAppContext) {
    let h = start(cx, CrashConsent::Never, vec![report("r1")], false);
    assert!(!h.has_dialog(cx));
    assert!(h.backend.crashes.sent.lock().unwrap().is_empty());
    assert!(h.backend.crashes.dismissed.lock().unwrap().is_empty());
}

// ------------------------------------------------------------------ settings

#[gpui_kit::test]
async fn updates_and_privacy_settings_round_trip(cx: &mut TestAppContext) {
    let h = start(cx, CrashConsent::Never, vec![report("r1")], false);
    h.with(cx, |w, cx| {
        settings_view::open_page(SettingsPageId::Updates, w, cx)
    });
    let page = cx.update(|cx| cx.global::<LastSettingsViews>().release.clone().unwrap());
    assert!(h.visible(cx, "release-settings"));
    cx.update(|cx| {
        let p = page.read(cx);
        assert_eq!(p.settings.channel, UpdateChannel::Stable);
        assert!(p.settings.check_automatically);
        assert_eq!(p.consent, CrashConsent::Never);
    });

    h.with(cx, |_, cx| {
        page.update(cx, |p, cx| {
            p.set_channel(UpdateChannel::Beta, cx);
            p.set_check_automatically(false, cx);
            p.set_consent(CrashConsent::Ask, cx);
        })
    });
    let saved = h.backend.updates.saved.lock().unwrap().clone();
    assert_eq!(
        saved.last(),
        Some(&UpdateSettings {
            channel: UpdateChannel::Beta,
            check_automatically: false,
        })
    );
    assert_eq!(
        *h.backend.crashes.consent.lock().unwrap(),
        CrashConsent::Ask
    );
    cx.update(|cx| assert!(matches!(page.read(cx).outcome, Some(Ok(_)))));

    // Check Now (beta channel: the beta release).
    h.with(cx, |w, cx| page.update(cx, |p, cx| p.check_now(w, cx)));
    cx.update(|cx| {
        let p = page.read(cx);
        assert!(!p.checking);
        assert_eq!(
            p.check.clone().unwrap().unwrap().map(|u| u.version),
            Some("1.2.0-beta.1".into())
        );
        assert_eq!(
            release::hub(cx)
                .unwrap()
                .update
                .as_ref()
                .map(|u| u.version.as_str()),
            Some("1.2.0-beta.1")
        );
    });

    // View Reports, then Clear.
    h.with(cx, |_, cx| page.update(cx, |p, cx| p.toggle_reports(cx)));
    assert!(h.visible(cx, "crash-report-list"));
    cx.update(|cx| assert_eq!(page.read(cx).reports.len(), 1));
    h.with(cx, |_, cx| {
        page.update(cx, |p, cx| p.toggle_report("r1", cx))
    });
    cx.update(|cx| assert_eq!(page.read(cx).open_report.as_deref(), Some("r1")));
    h.with(cx, |_, cx| page.update(cx, |p, cx| p.clear_reports(cx)));
    assert!(h.backend.crashes.cleared.load(Ordering::SeqCst));
    cx.update(|cx| assert!(page.read(cx).reports.is_empty()));
}

// ------------------------------------------------------------------ updates

#[gpui_kit::test]
async fn update_available_download_ready_install(cx: &mut TestAppContext) {
    // Due at startup: checked in the background, announced.
    let h = start(cx, CrashConsent::Ask, vec![], true);
    assert_eq!(h.backend.updates.checks.load(Ordering::SeqCst), 1);
    cx.update(|cx| {
        assert_eq!(
            release::hub(cx)
                .unwrap()
                .update
                .as_ref()
                .map(|u| u.version.as_str()),
            Some("1.1.0")
        )
    });
    assert!(!h.has_dialog(cx), "an automatic check only notifies");
    assert!(h.visible(cx, "status-update"));

    h.with(cx, release::open_update_dialog);
    assert!(h.visible(cx, "update-dialog"));
    h.with(cx, release::start_download);
    assert_eq!(
        h.download_state(cx),
        DownloadState::Running {
            done: 0,
            total: 1000
        }
    );
    h.backend.updates.send(UpdateEvent::Progress {
        done: 400,
        total: 1000,
    });
    cx.run_until_parked();
    assert_eq!(
        h.download_state(cx),
        DownloadState::Running {
            done: 400,
            total: 1000
        }
    );
    let path = PathBuf::from("/tmp/updates/FerroDb_1.1.0_x64-setup.exe");
    h.backend.updates.send(UpdateEvent::Ready(path.clone()));
    cx.run_until_parked();
    assert_eq!(h.download_state(cx), DownloadState::Ready(path.clone()));
    assert!(h.visible(cx, "update-ready"));

    h.with(cx, release::install_and_restart);
    assert_eq!(*h.backend.updates.installed.lock().unwrap(), vec![path]);
}

#[gpui_kit::test]
async fn download_can_be_cancelled(cx: &mut TestAppContext) {
    let h = start(cx, CrashConsent::Ask, vec![], false);
    assert_eq!(h.backend.updates.checks.load(Ordering::SeqCst), 0);
    // Help ▸ Check for Updates: shows the dialog.
    h.with(cx, |w, cx| release::check(true, w, cx));
    assert!(h.has_dialog(cx));
    assert!(h.visible(cx, "update-dialog"));
    h.with(cx, release::start_download);
    h.with(cx, |_, cx| release::cancel_download(cx));
    assert!(h.backend.updates.cancelled());
    h.backend
        .updates
        .send(UpdateEvent::Failed("cancelled".into()));
    cx.run_until_parked();
    assert_eq!(
        h.download_state(cx),
        DownloadState::Failed("cancelled".into())
    );
}

#[gpui_kit::test]
async fn join_beta_switches_channel_and_checks(cx: &mut TestAppContext) {
    let h = start(cx, CrashConsent::Ask, vec![], false);
    h.with(cx, release::join_beta);
    assert_eq!(
        h.backend.updates.settings.lock().unwrap().channel,
        UpdateChannel::Beta
    );
    assert_eq!(h.backend.updates.checks.load(Ordering::SeqCst), 1);
    cx.update(|cx| {
        assert_eq!(
            release::hub(cx).unwrap().update.as_ref().map(|u| u.channel),
            Some(UpdateChannel::Beta)
        );
        let about = release::about_lines(cx).join("\n");
        assert!(about.contains("1.0.0"), "{about}");
        assert!(about.contains("API version 8"), "{about}");
        assert!(about.contains("Beta"), "{about}");
        assert!(release::diagnostics(cx).contains("FerroDb 1.0.0"));
    });
    // Already in the beta program: nothing saved again.
    let before = h.backend.updates.saved.lock().unwrap().len();
    h.with(cx, release::join_beta);
    assert_eq!(h.backend.updates.saved.lock().unwrap().len(), before);
}

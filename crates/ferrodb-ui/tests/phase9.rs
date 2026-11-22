//! Phase 9 flows on the deterministic mock with fake AI, agent and script
//! services: asking the assistant and inserting its query, the disabled
//! state, console AI actions, the consent dialog, the settings pages, the
//! export wizard and "Copy As" through extractor scripts, and the grid's
//! aggregator picker.

use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime},
};

use ferrodb_api::{
    AgentAccess, AgentEvent, AgentPolicy, AgentService, AiProvider, AiSettings, AssistantAnswer,
    AssistantEvent, AssistantHandle, AssistantRequest, AssistantService, AssistantTask, AuditEntry,
    AuditOutcome, Backend, BoxFuture, CancelHandle, ConsentDecision, ConsentMode, ConsentRequest,
    DataFormat, DataSourceId, DataSourceService, DriverCatalogService, ExtScriptInfo,
    ExtScriptKind, HistoryService, LanguageService, McpConnectionInfo, McpSettings, McpStatus,
    MetadataService, QueryLanguage, QueryService, RiskKind, ScriptService, ScriptTable,
    ScriptValue, SearchIndexService, SuggestedQuery, TransferService, TransferTarget,
};
use ferrodb_mock::MockBackend;
use ferrodb_ui::{
    AppState, Workspace, agents,
    ai_settings::{AgentField, AiField, LastSettingsViews},
    panels::{
        AssistantPanel, ConsolePanel,
        assistant::{self, AssistantState},
        console::{AiRequest, RunTarget},
    },
    project::Project,
    results::ResultView,
    settings_view::{self, SettingsPageId},
    views::wizard::{self, ExportSource, LastWizard, WizardKind},
    workspace,
};
use ferrodb_ui_kit::kit::WindowExt as _;
use gpui_kit::{
    App, AppContext as _, Bounds, Entity, Point, TestAppContext, Window, WindowBounds,
    WindowHandle, WindowOptions, base::Root, px, size, test::TestWindowExt as _,
};

// ------------------------------------------------------------------ fakes

const ANSWER: &str =
    "Top customers by revenue:\n\n```sql\nSELECT * FROM orders;\n```\nRun it in a console.";

struct FakeAssistant {
    settings: Mutex<AiSettings>,
    saved: Mutex<Vec<(AiSettings, Option<String>)>>,
    requests: Mutex<Vec<AssistantRequest>>,
}

impl FakeAssistant {
    fn new(enabled: bool) -> Self {
        Self {
            settings: Mutex::new(AiSettings {
                enabled,
                has_key: true,
                ..Default::default()
            }),
            saved: Mutex::new(Vec::new()),
            requests: Mutex::new(Vec::new()),
        }
    }
}

impl AssistantService for FakeAssistant {
    fn settings(&self) -> AiSettings {
        self.settings.lock().unwrap().clone()
    }
    fn save_settings(
        &self,
        settings: AiSettings,
        api_key: Option<String>,
    ) -> BoxFuture<'_, ferrodb_api::Result<()>> {
        let has_key = match &api_key {
            Some(k) => !k.is_empty(),
            None => self.settings.lock().unwrap().has_key,
        };
        *self.settings.lock().unwrap() = AiSettings {
            has_key,
            ..settings.clone()
        };
        self.saved.lock().unwrap().push((settings, api_key));
        Box::pin(async { Ok(()) })
    }
    fn test(&self) -> BoxFuture<'_, ferrodb_api::Result<String>> {
        Box::pin(async { Ok("fake-model".into()) })
    }
    fn models(&self) -> BoxFuture<'_, ferrodb_api::Result<Vec<String>>> {
        Box::pin(async { Ok(vec!["fake-small".into(), "fake-large".into()]) })
    }
    fn ask(&self, request: AssistantRequest) -> AssistantHandle {
        self.requests.lock().unwrap().push(request);
        let (tx, rx) = async_channel::unbounded();
        let (first, rest) = ANSWER.split_at(30);
        for ev in [
            AssistantEvent::Context {
                objects: vec!["orders".into(), "customers".into()],
                chars: 8400,
            },
            AssistantEvent::Delta(first.into()),
            AssistantEvent::Delta(rest.into()),
            AssistantEvent::Finished(AssistantAnswer {
                text: ANSWER.into(),
                queries: vec![SuggestedQuery {
                    text: "SELECT * FROM orders;".into(),
                    language: QueryLanguage::Cql,
                    modifies: false,
                    risks: vec![],
                    problems: vec![],
                }],
                usage: None,
                model: "fake-model".into(),
            }),
        ] {
            tx.try_send(ev).unwrap();
        }
        AssistantHandle {
            events: rx,
            cancel: CancelHandle::new(),
        }
    }
}

struct FakeAgents {
    tx: async_channel::Sender<AgentEvent>,
    rx: async_channel::Receiver<AgentEvent>,
    decided: Mutex<Vec<(u64, ConsentDecision)>>,
    policy: Mutex<AgentPolicy>,
    mcp: Mutex<McpSettings>,
}

impl FakeAgents {
    fn new() -> Self {
        let (tx, rx) = async_channel::unbounded();
        Self {
            tx,
            rx,
            decided: Mutex::new(Vec::new()),
            policy: Mutex::new(AgentPolicy::default()),
            mcp: Mutex::new(McpSettings::default()),
        }
    }
}

fn running_status() -> McpStatus {
    McpStatus {
        running: true,
        url: Some("http://127.0.0.1:7391/mcp".into()),
        clients: vec!["claude-code".into(), "cursor".into()],
        error: None,
    }
}

impl AgentService for FakeAgents {
    fn policy(&self) -> AgentPolicy {
        self.policy.lock().unwrap().clone()
    }
    fn save_policy(&self, policy: AgentPolicy) -> BoxFuture<'_, ferrodb_api::Result<()>> {
        *self.policy.lock().unwrap() = policy;
        Box::pin(async { Ok(()) })
    }
    fn mcp_settings(&self) -> McpSettings {
        self.mcp.lock().unwrap().clone()
    }
    fn save_mcp_settings(&self, settings: McpSettings) -> BoxFuture<'_, ferrodb_api::Result<()>> {
        *self.mcp.lock().unwrap() = settings;
        Box::pin(async { Ok(()) })
    }
    fn status(&self) -> McpStatus {
        running_status()
    }
    fn connection_info(&self) -> Option<McpConnectionInfo> {
        Some(McpConnectionInfo {
            url: "http://127.0.0.1:7391/mcp".into(),
            token: "secret-token".into(),
            client_config: "{\"mcpServers\":{}}".into(),
        })
    }
    fn regenerate_token(&self) -> BoxFuture<'_, ferrodb_api::Result<()>> {
        Box::pin(async { Ok(()) })
    }
    fn subscribe(&self) -> async_channel::Receiver<AgentEvent> {
        self.rx.clone()
    }
    fn pending(&self) -> Vec<ConsentRequest> {
        Vec::new()
    }
    fn decide(&self, id: u64, decision: ConsentDecision) {
        self.decided.lock().unwrap().push((id, decision));
    }
    fn audit(&self, _limit: usize) -> BoxFuture<'_, ferrodb_api::Result<Vec<AuditEntry>>> {
        let entry = |secs: u64, tool: &str, outcome| AuditEntry {
            at: SystemTime::UNIX_EPOCH + Duration::from_secs(secs),
            client: "claude-code".into(),
            tool: tool.into(),
            data_source: Some(DataSourceId::new("shop-dev")),
            text: Some("select * from orders".into()),
            outcome,
            items: Some(3),
            error: None,
            elapsed_ms: Some(5),
        };
        let entries = vec![
            entry(100, "list_tables", AuditOutcome::AllowedByPolicy),
            entry(200, "run_query", AuditOutcome::DeniedByUser),
        ];
        Box::pin(async move { Ok(entries) })
    }
}

struct FakeScripts {
    extracted: Mutex<Vec<(String, ScriptTable)>>,
}

fn script(id: &str, kind: ExtScriptKind, name: &str, ext: &str) -> ExtScriptInfo {
    ExtScriptInfo {
        id: id.into(),
        kind,
        name: name.into(),
        description: String::new(),
        extension: ext.into(),
        path: None,
        builtin: true,
        error: None,
    }
}

impl ScriptService for FakeScripts {
    fn list(&self, kind: ExtScriptKind) -> Vec<ExtScriptInfo> {
        match kind {
            ExtScriptKind::Extractor => {
                vec![script("builtin:sql-in-list", kind, "SQL IN list", "sql")]
            }
            ExtScriptKind::Aggregator => {
                vec![script("builtin:distinct", kind, "Distinct count", "")]
            }
        }
    }
    fn reload(&self) -> BoxFuture<'_, ferrodb_api::Result<()>> {
        Box::pin(async { Ok(()) })
    }
    fn directory(&self) -> Option<PathBuf> {
        None
    }
    fn create(
        &self,
        _kind: ExtScriptKind,
        _name: &str,
    ) -> BoxFuture<'_, ferrodb_api::Result<PathBuf>> {
        Box::pin(async { Err(ferrodb_api::Error::Unsupported("create".into())) })
    }
    fn aggregate(
        &self,
        _id: &str,
        cells: ScriptTable,
    ) -> BoxFuture<'_, ferrodb_api::Result<String>> {
        let mut values: Vec<String> = cells.rows.iter().map(|r| format!("{:?}", r[0])).collect();
        values.sort();
        values.dedup();
        let text = format!("distinct {}", values.len());
        Box::pin(async move { Ok(text) })
    }
    fn extract(&self, id: &str, table: ScriptTable) -> BoxFuture<'_, ferrodb_api::Result<String>> {
        let first: Vec<String> = table
            .rows
            .iter()
            .map(|r| match &r[0] {
                ScriptValue::Int(i) => i.to_string(),
                v => format!("{v:?}"),
            })
            .collect();
        let text = format!("IN ({})", first.join(", "));
        self.extracted.lock().unwrap().push((id.to_owned(), table));
        Box::pin(async move { Ok(text) })
    }
}

/// The mock with phase 9 services replaced by the fakes.
struct TestBackend {
    mock: MockBackend,
    assistant: FakeAssistant,
    agents: FakeAgents,
    scripts: FakeScripts,
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
    fn assistant(&self) -> &dyn AssistantService {
        &self.assistant
    }
    fn agents(&self) -> &dyn AgentService {
        &self.agents
    }
    fn scripts(&self) -> &dyn ScriptService {
        &self.scripts
    }
}

// ------------------------------------------------------------------ harness

struct Harness {
    backend: Arc<TestBackend>,
    window: WindowHandle<Root>,
    ws: Entity<Workspace>,
    _dir: tempfile::TempDir,
}

fn start_with(cx: &mut TestAppContext, assistant_enabled: bool) -> Harness {
    let backend = Arc::new(TestBackend {
        mock: MockBackend::deterministic(),
        assistant: FakeAssistant::new(assistant_enabled),
        agents: FakeAgents::new(),
        scripts: FakeScripts {
            extracted: Mutex::new(Vec::new()),
        },
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
        ws,
        _dir: dir,
    };
    h.frame(cx);
    h
}

fn start(cx: &mut TestAppContext) -> Harness {
    start_with(cx, true)
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

    fn console(&self, cx: &mut TestAppContext, ds: &str, text: &str) -> Entity<ConsolePanel> {
        let ws = self.ws.clone();
        let (ds, text) = (ds.to_owned(), text.to_owned());
        self.with(cx, |w, cx| {
            ws.update(cx, |ws, cx| {
                ws.add_console(Some(DataSourceId::new(ds)), text, w, cx)
            })
        })
    }

    fn run(&self, cx: &mut TestAppContext, ds: &str, text: &str) -> Entity<ConsolePanel> {
        let console = self.console(cx, ds, text);
        self.with(cx, |w, cx| {
            console.update(cx, |c, cx| c.run(RunTarget::Script, w, cx))
        });
        self.frame(cx);
        console
    }

    fn assistant(&self, cx: &mut TestAppContext) -> Entity<AssistantPanel> {
        let panel = self.with(cx, |w, cx| workspace::show_assistant(w, cx).expect("panel"));
        self.frame(cx);
        panel
    }

    fn text(&self, cx: &mut TestAppContext, console: &Entity<ConsolePanel>) -> String {
        cx.update(|cx| console.read(cx).editor().read(cx).value().to_string())
    }

    fn emit(&self, cx: &mut TestAppContext, ev: AgentEvent) {
        self.backend.agents.tx.try_send(ev).unwrap();
        cx.run_until_parked();
        self.frame(cx);
    }

    fn has_dialog(&self, cx: &mut TestAppContext) -> bool {
        self.with(cx, |w, cx| w.has_active_dialog(cx))
    }
}

fn consent(id: u64, modifies: bool) -> ConsentRequest {
    ConsentRequest {
        id,
        client: "claude-code".into(),
        tool: "run_query".into(),
        data_source: Some(DataSourceId::new("shop-dev")),
        data_source_name: Some("Shop (dev)".into()),
        text: Some(if modifies {
            "delete from orders".into()
        } else {
            "select * from orders".into()
        }),
        modifies,
        risks: if modifies {
            vec![RiskKind::DeleteAll]
        } else {
            vec![]
        },
        summary: "Run a query on Shop (dev)".into(),
        at: SystemTime::now(),
    }
}

// ------------------------------------------------------------------ assistant

#[gpui_kit::test]
async fn assistant_answers_and_its_query_goes_into_the_console(cx: &mut TestAppContext) {
    let h = start(cx);
    let console = h.console(cx, "shop-dev", "");
    let panel = h.assistant(cx);
    h.with(cx, |w, cx| {
        panel.update(cx, |p, cx| {
            p.set_prompt("top customers by revenue", w, cx);
            p.send(w, cx);
        })
    });
    h.frame(cx);
    cx.update(|cx| {
        let p = panel.read(cx);
        assert!(!p.is_running());
        assert_eq!(p.exchanges.len(), 1);
        let e = &p.exchanges[0];
        assert!(e.done && e.error.is_none());
        assert_eq!(e.sent, Some((2, 8400)), "context line data");
        assert_eq!(e.answer, ANSWER, "streamed deltas add up to the answer");
        assert_eq!(e.queries.len(), 1);
        assert!(p.prompt_text(cx).is_empty(), "the input is cleared");
    });
    h.with(cx, |w, _| {
        assert!(w.find("ai-chat").visible());
    });
    {
        let reqs = h.backend.assistant.requests.lock().unwrap();
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].task, AssistantTask::Generate);
        assert_eq!(
            reqs[0].data_source,
            Some(DataSourceId::new("shop-dev")),
            "follows the active console"
        );
        assert!(reqs[0].history.is_empty());
    }
    // Insert the suggested query at the console's cursor.
    h.with(cx, |w, cx| {
        panel.update(cx, |p, cx| p.insert_query(0, 0, w, cx))
    });
    assert!(h.text(cx, &console).contains("SELECT * FROM orders;"));
    // Copy puts it on the clipboard.
    h.with(cx, |w, cx| {
        panel.update(cx, |p, cx| p.copy_query(0, 0, w, cx))
    });
    let clip = cx.read_from_clipboard().and_then(|c| c.text());
    assert_eq!(clip.as_deref(), Some("SELECT * FROM orders;"));
    // A follow-up carries the conversation.
    h.with(cx, |w, cx| {
        panel.update(cx, |p, cx| {
            p.set_prompt("only this year", w, cx);
            p.send(w, cx);
        })
    });
    {
        let reqs = h.backend.assistant.requests.lock().unwrap();
        assert_eq!(reqs.len(), 2);
        assert_eq!(reqs[1].task, AssistantTask::Chat);
        assert_eq!(reqs[1].history.len(), 2, "user + assistant turn");
        assert_eq!(reqs[1].history[1].text, ANSWER);
    }
    // Open in a new console.
    let before = cx.update(|cx| h.ws.read(cx).consoles().len());
    h.with(cx, |w, cx| {
        panel.update(cx, |p, cx| p.open_query(1, 0, w, cx))
    });
    let consoles = cx.update(|cx| h.ws.read(cx).consoles());
    assert_eq!(consoles.len(), before + 1);
    let last = consoles.last().unwrap().clone();
    assert_eq!(h.text(cx, &last), "SELECT * FROM orders;");
}

#[gpui_kit::test]
async fn disabled_assistant_shows_the_settings_button(cx: &mut TestAppContext) {
    let h = start_with(cx, false);
    let panel = h.assistant(cx);
    cx.update(|cx| assert_eq!(assistant::state(cx), AssistantState::Disabled));
    h.with(cx, |w, _| {
        assert!(w.find("ai-empty").visible());
        assert!(w.find("ai-open-settings").visible());
    });
    // Asking does nothing while disabled.
    h.with(cx, |_, cx| {
        panel.update(cx, |p, cx| {
            p.ask(AssistantTask::Generate, "x".into(), None, cx)
        })
    });
    assert!(h.backend.assistant.requests.lock().unwrap().is_empty());
    // The button opens the AI settings page.
    h.with(cx, |w, cx| w.click("ai-open-settings", cx));
    h.frame(cx);
    assert!(h.has_dialog(cx));
    h.with(cx, |w, _| assert!(w.find("ai-settings").visible()));
}

#[gpui_kit::test]
async fn console_explains_and_fixes_through_the_assistant(cx: &mut TestAppContext) {
    let h = start(cx);
    let console = h.run(cx, "shop-dev", "select error from t");
    cx.update(|cx| {
        let c = console.read(cx);
        let (stmt, err) = c.last_error.clone().expect("a failed statement");
        assert!(stmt.contains("select error"));
        assert!(err.contains("syntax error"));
    });
    h.with(cx, |w, _| assert!(w.find("console-fix-ai").visible()));
    h.with(cx, |w, cx| {
        console.update(cx, |c, cx| c.ask_ai(AiRequest::Fix, w, cx))
    });
    h.with(cx, |w, cx| {
        console.update(cx, |c, cx| c.ask_ai(AiRequest::Explain, w, cx))
    });
    let reqs = h.backend.assistant.requests.lock().unwrap().clone();
    assert_eq!(reqs.len(), 2);
    match &reqs[0].task {
        AssistantTask::Fix { error } => assert!(error.contains("syntax error")),
        t => panic!("expected Fix, got {t:?}"),
    }
    assert!(reqs[0].context.as_deref().unwrap().contains("select error"));
    assert_eq!(reqs[1].task, AssistantTask::Explain);
    assert_eq!(reqs[1].context.as_deref(), Some("select error from t"));
    assert_eq!(reqs[1].data_source, Some(DataSourceId::new("shop-dev")));
    let panel = cx.update(|cx| {
        cx.global::<assistant::ActiveAssistant>()
            .0
            .upgrade()
            .expect("assistant opened")
    });
    cx.update(|cx| assert_eq!(panel.read(cx).exchanges.len(), 2));
}

// ------------------------------------------------------------------ agents

#[gpui_kit::test]
async fn consent_dialog_allows_denies_and_closes_on_timeout(cx: &mut TestAppContext) {
    let h = start(cx);
    // MCP running: the status bar shows it.
    h.with(cx, |w, _| assert!(w.find("status-mcp").visible()));

    h.emit(cx, AgentEvent::ConsentRequested(consent(1, false)));
    assert!(h.has_dialog(cx));
    h.with(cx, |w, _| assert!(w.find("consent-dialog").visible()));
    h.with(cx, |w, cx| {
        agents::decide(ConsentDecision::AllowOnce, w, cx)
    });
    assert!(!h.has_dialog(cx));

    // Two at once: they queue. A modifying call shows the warning and is
    // never allowed for the whole session.
    h.emit(cx, AgentEvent::ConsentRequested(consent(2, true)));
    h.emit(cx, AgentEvent::ConsentRequested(consent(3, false)));
    cx.update(|cx| {
        let hub = agents::hub(cx).unwrap();
        assert_eq!(hub.showing.as_ref().map(|r| r.id), Some(2));
        assert_eq!(hub.queue.len(), 1);
    });
    h.with(cx, |w, _| assert!(w.find("consent-modifies").visible()));
    h.with(cx, |w, cx| {
        agents::decide(ConsentDecision::AllowForSession, w, cx)
    });
    h.frame(cx);
    cx.update(|cx| {
        assert_eq!(
            agents::hub(cx).unwrap().showing.as_ref().map(|r| r.id),
            Some(3)
        )
    });
    // The backend closes #3 (timeout): its dialog goes away.
    h.emit(cx, AgentEvent::ConsentClosed { id: 3 });
    assert!(!h.has_dialog(cx));
    cx.update(|cx| assert!(agents::hub(cx).unwrap().showing.is_none()));

    // Escape denies.
    h.emit(cx, AgentEvent::ConsentRequested(consent(4, false)));
    h.with(cx, |w, cx| w.press("escape", cx));
    h.frame(cx);
    assert!(!h.has_dialog(cx));

    assert_eq!(
        *h.backend.agents.decided.lock().unwrap(),
        vec![
            (1, ConsentDecision::AllowOnce),
            (2, ConsentDecision::AllowOnce),
            (4, ConsentDecision::Deny),
        ]
    );
}

// ------------------------------------------------------------------ settings

#[gpui_kit::test]
async fn settings_pages_save_through_the_backend(cx: &mut TestAppContext) {
    let h = start(cx);
    h.with(cx, |w, cx| {
        settings_view::open_page(SettingsPageId::Ai, w, cx)
    });
    h.frame(cx);
    let views = cx.update(|cx| {
        let v = cx.global::<LastSettingsViews>();
        (v.ai.clone().unwrap(), v.agents.clone().unwrap())
    });
    let (ai, ag) = views;
    h.with(cx, |w, _| assert!(w.find("ai-settings").visible()));
    h.with(cx, |w, cx| {
        ai.update(cx, |v, cx| {
            v.set_provider(AiProvider::Ollama, w, cx);
            v.set_field(AiField::Model, "llama3", w, cx);
            v.set_field(AiField::ApiKey, "sk-test", w, cx);
            v.set_field(AiField::MaxTokens, "2048", w, cx);
            v.set_share_samples(true, cx);
            v.save(false, w, cx);
        })
    });
    {
        let saved = h.backend.assistant.saved.lock().unwrap();
        let (s, key) = saved.last().unwrap();
        assert_eq!(s.provider, AiProvider::Ollama);
        assert_eq!(s.model, "llama3");
        assert_eq!(s.max_tokens, 2048);
        assert!(s.share_samples && s.enabled);
        assert_eq!(key.as_deref(), Some("sk-test"));
    }
    cx.update(|cx| assert!(matches!(ai.read(cx).outcome, Some(Ok(_)))));
    // Saving again keeps the key; Clear removes it.
    h.with(cx, |w, cx| ai.update(cx, |v, cx| v.save(false, w, cx)));
    h.with(cx, |w, cx| {
        ai.update(cx, |v, cx| {
            v.clear_api_key(w, cx);
            v.save(true, w, cx);
        })
    });
    {
        let saved = h.backend.assistant.saved.lock().unwrap();
        assert_eq!(saved[1].1, None, "empty field keeps the key");
        assert_eq!(saved[2].1.as_deref(), Some(""), "Clear removes it");
    }
    cx.update(|cx| {
        let v = ai.read(cx);
        assert!(!v.draft.has_key);
        assert_eq!(v.outcome, Some(Ok("Connected: fake-model".into())));
    });
    h.with(cx, |w, cx| ai.update(cx, |v, cx| v.load_models(w, cx)));
    cx.update(|cx| {
        assert_eq!(
            ai.read(cx).models,
            Some(Ok(vec!["fake-small".to_string(), "fake-large".to_string()]))
        )
    });

    // Agents and MCP.
    cx.update(|cx| assert_eq!(ag.read(cx).audit.len(), 2));
    cx.update(|cx| assert_eq!(ag.read(cx).audit[0].tool, "run_query", "newest first"));
    h.with(cx, |w, cx| {
        ag.update(cx, |v, cx| {
            v.set_mcp_enabled(true, cx);
            v.set_field(AgentField::Port, "7400", w, cx);
            v.set_field(AgentField::MaxRows, "50", w, cx);
            v.set_default_access(AgentAccess::MetadataOnly, cx);
            v.set_source_access(DataSourceId::new("shop-dev"), Some(AgentAccess::Hidden), cx);
            v.set_consent(ConsentMode::WritesOnly, cx);
            v.save(w, cx);
        })
    });
    let mcp = h.backend.agents.mcp.lock().unwrap().clone();
    assert!(mcp.enabled);
    assert_eq!(mcp.port, 7400);
    let policy = h.backend.agents.policy.lock().unwrap().clone();
    assert_eq!(policy.default_access, AgentAccess::MetadataOnly);
    assert_eq!(policy.consent, ConsentMode::WritesOnly);
    assert_eq!(policy.max_rows, 50);
    assert_eq!(
        policy.access(&DataSourceId::new("shop-dev")),
        AgentAccess::Hidden
    );
    // Connection info goes to the clipboard.
    h.with(cx, |w, cx| {
        ag.update(cx, |v, cx| {
            v.copy(ferrodb_ui::ai_settings::CopyWhat::Token, w, cx)
        })
    });
    let clip = cx.read_from_clipboard().and_then(|c| c.text());
    assert_eq!(clip.as_deref(), Some("secret-token"));
}

// ------------------------------------------------------------------ scripts

#[gpui_kit::test]
async fn export_wizard_offers_extractor_scripts(cx: &mut TestAppContext) {
    let h = start(cx);
    let source = ExportSource::Result {
        data_source: DataSourceId::new("shop-dev"),
        statement: "select * from orders".into(),
        title: "orders".into(),
    };
    h.with(cx, |w, cx| {
        wizard::open_with(WizardKind::Export, Some(source), None, w, cx)
    });
    h.frame(cx);
    let wz = cx.update(|cx| cx.global::<LastWizard>().0.clone().unwrap());
    cx.update(|cx| {
        let wz = wz.read(cx);
        assert_eq!(wz.scripts.len(), 1);
        assert_eq!(wz.scripts[0].name, "SQL IN list");
    });
    h.with(cx, |w, cx| {
        wz.update(cx, |wz, cx| wz.set_script("builtin:sql-in-list", w, cx))
    });
    cx.update(|cx| {
        let wz = wz.read(cx);
        assert_eq!(wz.format, DataFormat::Script);
        assert_eq!(wz.format_label(), "SQL IN list");
        let req = wz.request(cx).expect("a request");
        match req.target {
            TransferTarget::File {
                path,
                format,
                options,
            } => {
                assert_eq!(format, DataFormat::Script);
                assert_eq!(options.script.as_deref(), Some("builtin:sql-in-list"));
                assert_eq!(path.extension().and_then(|e| e.to_str()), Some("sql"));
            }
            t => panic!("unexpected target {t:?}"),
        }
    });
    // Back to a built-in format drops the script.
    h.with(cx, |w, cx| {
        wz.update(cx, |wz, cx| wz.set_format(DataFormat::Json, w, cx))
    });
    cx.update(|cx| {
        let wz = wz.read(cx);
        assert_eq!(wz.script, None);
        match wz.request(cx).unwrap().target {
            TransferTarget::File { options, .. } => assert_eq!(options.script, None),
            t => panic!("unexpected target {t:?}"),
        }
    });
}

fn grid(h: &Harness, cx: &mut TestAppContext) -> Entity<ferrodb_ui::results::grid::GridView> {
    let console = h.run(cx, "shop-dev", "select * from orders");
    match cx.update(|cx| console.read(cx).result_view(1).cloned().expect("a result")) {
        ResultView::Grid(v) => v,
        _ => panic!("a grid"),
    }
}

#[gpui_kit::test]
async fn copy_as_runs_an_extractor_on_the_rows(cx: &mut TestAppContext) {
    let h = start(cx);
    let g = grid(&h, cx);
    h.with(cx, |w, cx| {
        g.update(cx, |g, cx| {
            g.copy_as("builtin:sql-in-list".into(), vec![0, 1], w, cx)
        })
    });
    let (id, table) = h.backend.scripts.extracted.lock().unwrap()[0].clone();
    assert_eq!(id, "builtin:sql-in-list");
    assert_eq!(table.rows.len(), 2);
    assert!(!table.columns.is_empty());
    assert_eq!(table.columns.len(), table.types.len());
    let clip = cx.read_from_clipboard().and_then(|c| c.text()).unwrap();
    assert!(clip.starts_with("IN ("), "{clip}");
}

#[gpui_kit::test]
async fn aggregator_picker_switches_to_a_script(cx: &mut TestAppContext) {
    let h = start(cx);
    let g = grid(&h, cx);
    h.with(cx, |_, cx| g.update(cx, |g, cx| g.select(0, 0, cx)));
    h.frame(cx);
    let builtin = cx.update(|cx| g.read(cx).aggregate_text(cx)).unwrap();
    assert!(builtin.starts_with("count"), "{builtin}");
    h.with(cx, |w, _| assert!(w.find("grid-aggregator").visible()));
    h.with(cx, |_, cx| {
        g.update(cx, |g, cx| {
            g.set_aggregator(Some("builtin:distinct".into()), cx)
        })
    });
    let text = cx.update(|cx| g.read(cx).aggregate_text(cx)).unwrap();
    assert!(text.starts_with("distinct "), "{text}");
    h.frame(cx);
    h.with(cx, |w, _| assert!(w.find("grid-aggregate").visible()));
    h.with(cx, |_, cx| g.update(cx, |g, cx| g.set_aggregator(None, cx)));
    let back = cx.update(|cx| g.read(cx).aggregate_text(cx)).unwrap();
    assert_eq!(back, builtin);
}

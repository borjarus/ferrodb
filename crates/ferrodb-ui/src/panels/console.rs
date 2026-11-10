//! Query console (plan 2.3): editor with highlighting for the source
//! language, completions and diagnostics from the language service, current
//! statement highlight, execution of the statement under the cursor / the
//! selection / the whole script, cost estimate before running (sources with
//! `cost_estimate`), and result tabs below the editor.

use std::{
    collections::HashMap,
    ops::Range,
    rc::Rc,
    time::{Duration, Instant},
};

use ferrodb_api::{
    CancelHandle, DataSourceId, DataSourceInfo, Environment, ExecMode, ExecOptions, ExplainSupport,
    NodeRef, QueryEvent, QueryLanguage, Risk, RiskKind, Script, SessionId, TxMode,
};
use ferrodb_ui_kit::{
    gpui::{prelude::*, *},
    icons,
    kit::{
        ActiveTheme, Disableable as _, Sizable as _, WindowExt as _,
        button::{Button, ButtonVariants as _},
        dialog::DialogFooter,
        dock::{BasePanel, Panel, PanelInfo, PanelState},
        h_flex,
        input::{
            Editor, EditorState, Input, InputEvent, InputState, RangeDecoration,
            RangeDecorationCollection, RangeDecorationStyle,
        },
        menu::{DropdownMenu as _, PopupMenuItem},
        notification::Notification,
        resizable::{resizable_panel, v_resizable},
        tab::{Tab, TabBar},
        v_flex,
    },
    theme::environment_color,
};
use serde::{Deserialize, Serialize};

use super::panel_boilerplate;
use crate::{
    AppState,
    actions::{self, CONSOLE_CONTEXT},
    activity::{self, RunningQuery},
    completion::{self, BackendCompletion},
    i18n::{t, t_args},
    output::{self, Level},
    results::{ResultData, ResultModel, ResultView, ResultViewEvent, format},
    views::plan::PlanView,
};

pub const NAME: &str = "Console";

/// Debounce of diagnostics and of the current-statement highlight (plan 5.5).
const ANALYSIS_DEBOUNCE: Duration = Duration::from_millis(150);

#[derive(Default, Serialize, Deserialize)]
struct SavedConsole {
    data_source: Option<DataSourceId>,
    #[serde(default)]
    schema: Option<String>,
    text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageLevel {
    Info,
    Warning,
    Error,
}

#[derive(Clone, Debug)]
pub struct Message {
    pub level: MessageLevel,
    pub text: String,
}

pub enum ConsoleTab {
    Result {
        title: String,
        view: ResultView,
        _sub: Subscription,
    },
    Plan {
        title: String,
        view: Entity<PlanView>,
    },
}

impl ConsoleTab {
    fn title(&self) -> &str {
        match self {
            Self::Result { title, .. } | Self::Plan { title, .. } => title,
        }
    }
}

/// What to run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunTarget {
    /// Selection, or the statement under the cursor.
    Statement,
    Script,
    /// Explicit text (re-run from a result view's filter fields).
    Text(String),
}

pub struct ConsolePanel {
    focus: FocusHandle,
    editor: Entity<EditorState>,
    data_source: Option<DataSourceId>,
    /// Schema / database used for unqualified names (shown in the header).
    schema: Option<String>,
    schemas: Vec<String>,
    session: Option<SessionId>,
    running: Option<CancelHandle>,
    pub messages: Vec<Message>,
    pub tabs: Vec<ConsoleTab>,
    /// 0 = messages, n = tabs[n - 1].
    pub active_tab: usize,
    /// Last cost estimate, in bytes.
    pub cost: Option<u64>,
    statement_highlight: Option<RangeDecorationCollection>,
    analysis: Option<Task<()>>,
    /// Parameter values of the run being started (`ExecOptions::params`).
    pending_params: Vec<(String, Option<String>)>,
    /// Last values typed per parameter name, offered again next time.
    param_values: HashMap<String, String>,
    _subs: Vec<Subscription>,
}

fn risk_label(kind: RiskKind) -> String {
    t(match kind {
        RiskKind::DeleteAll => "risk-delete-all",
        RiskKind::UpdateAll => "risk-update-all",
        RiskKind::Drop => "risk-drop",
        RiskKind::Truncate => "risk-truncate",
        RiskKind::Modification => "risk-modification",
    })
}

/// Editor language id (tree-sitter grammar) for a query language.
pub fn editor_language(lang: Option<&QueryLanguage>) -> &'static str {
    match lang {
        Some(QueryLanguage::Mql) => "javascript",
        _ => "sql",
    }
}

/// The qualified name under the cursor (`sales.orders` → `["sales",
/// "orders"]`), with quotes removed; parts after the cursor are dropped.
pub fn identifier_at(text: &str, cursor: usize) -> Vec<String> {
    let cursor = cursor.min(text.len());
    let word = |c: char| c.is_alphanumeric() || matches!(c, '_' | '$' | '.' | '"' | '`');
    let start = text[..cursor]
        .char_indices()
        .rev()
        .take_while(|(_, c)| word(*c))
        .last()
        .map_or(cursor, |(i, _)| i);
    let end = text[cursor..]
        .char_indices()
        .find(|(_, c)| !word(*c) || *c == '.')
        .map_or(text.len(), |(i, _)| cursor + i);
    text[start..end]
        .split('.')
        .map(|p| p.trim_matches(|c| c == '"' || c == '`').to_owned())
        .filter(|p| !p.is_empty())
        .collect()
}

/// The statement containing `cursor` (or the last one before it).
pub fn statement_at(spans: &[Range<usize>], cursor: usize) -> Option<Range<usize>> {
    spans
        .iter()
        .find(|r| r.start <= cursor && cursor <= r.end)
        .or_else(|| spans.iter().rev().find(|r| r.end <= cursor))
        .or(spans.first())
        .cloned()
}

impl ConsolePanel {
    pub fn new(
        data_source: Option<DataSourceId>,
        text: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let lang = data_source
            .as_ref()
            .and_then(|id| AppState::backend(cx).data_sources().get(id))
            .map(|s| s.capabilities.language);
        let line_numbers = crate::settings::SettingsStore::get(cx).show_line_numbers;
        let editor = cx.new(|cx| {
            EditorState::new(window, cx)
                .language(editor_language(lang.as_ref()))
                .default_value(text)
                .line_number(line_numbers)
        });
        let subs = vec![
            cx.subscribe(&editor, |this, _, ev: &InputEvent, cx| {
                if matches!(ev, InputEvent::Change) {
                    this.schedule_analysis(true, cx);
                }
            }),
            cx.observe(&editor, |this, _, cx| this.schedule_analysis(false, cx)),
        ];
        let mut this = Self {
            focus: cx.focus_handle(),
            editor,
            data_source: None,
            schema: None,
            schemas: Vec::new(),
            session: None,
            running: None,
            messages: Vec::new(),
            tabs: Vec::new(),
            active_tab: 0,
            cost: None,
            statement_highlight: None,
            analysis: None,
            pending_params: Vec::new(),
            param_values: HashMap::new(),
            _subs: subs,
        };
        this.set_data_source(data_source, cx);
        this
    }

    pub fn restore(state: &PanelState, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let saved: SavedConsole = match &state.info {
            PanelInfo::Panel(v) => serde_json::from_value(v.clone()).unwrap_or_default(),
            _ => SavedConsole::default(),
        };
        let mut this = Self::new(saved.data_source, saved.text, window, cx);
        if saved.schema.is_some() {
            this.schema = saved.schema;
        }
        this
    }

    pub fn editor(&self) -> &Entity<EditorState> {
        &self.editor
    }

    pub fn data_source(&self) -> Option<&DataSourceId> {
        self.data_source.as_ref()
    }

    pub fn is_running(&self) -> bool {
        self.running.is_some()
    }

    fn source(&self, cx: &App) -> Option<DataSourceInfo> {
        let id = self.data_source.as_ref()?;
        AppState::backend(cx).data_sources().get(id)
    }

    fn source_name(&self, cx: &App) -> Option<String> {
        self.source(cx).map(|s| s.config.name)
    }

    /// Switches the console to another source (language, completions, schemas).
    pub fn set_data_source(&mut self, ds: Option<DataSourceId>, cx: &mut Context<Self>) {
        if ds == self.data_source && ds.is_some() {
            return;
        }
        self.data_source = ds;
        self.session = None;
        self.schema = None;
        self.schemas.clear();
        let Some(src) = self.source(cx) else {
            cx.notify();
            return;
        };
        let lang = src.capabilities.language.clone();
        let ds = src.config.id.clone();
        self.editor.update(cx, |e, cx| {
            e.set_highlighter(editor_language(Some(&lang)), cx);
            e.lsp_mut().completion_provider = Some(Rc::new(BackendCompletion {
                data_source: Some(ds.clone()),
                language: lang,
            }));
        });
        // Schemas / databases for the picker: first level of the hierarchy.
        let backend = AppState::backend(cx);
        let task = cx
            .background_executor()
            .spawn(async move { backend.metadata().children(NodeRef::root(ds)).await });
        cx.spawn(async move |this, cx| {
            if let Ok(objs) = task.await {
                let _ = this.update(cx, |this, cx| {
                    this.schemas = objs.into_iter().map(|o| o.name).collect();
                    if this.schema.is_none() {
                        this.schema = this.schemas.first().cloned();
                    }
                    cx.notify();
                });
            }
        })
        .detach();
        cx.notify();
    }

    /// Debounced language analysis: diagnostics after edits, current
    /// statement highlight after cursor moves.
    fn schedule_analysis(&mut self, edited: bool, cx: &mut Context<Self>) {
        if !edited && self.analysis.is_some() {
            return;
        }
        let Some(src) = self.source(cx) else { return };
        let backend = AppState::backend(cx);
        let lang = src.capabilities.language.clone();
        let ds = src.config.id.clone();
        self.analysis = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(ANALYSIS_DEBOUNCE).await;
            let Ok((text, cursor)) = this.update(cx, |this, cx| {
                let e = this.editor.read(cx);
                (e.value().to_string(), e.cursor())
            }) else {
                return;
            };
            let doc = completion::snapshot(&text, &lang, Some(&ds));
            let spans: Vec<Range<usize>> = backend
                .language()
                .statements(&doc)
                .into_iter()
                .map(|s| s.range)
                .collect();
            let diags = if edited {
                Some(backend.language().diagnostics(doc).await)
            } else {
                None
            };
            let _ = this.update(cx, |this, cx| {
                this.analysis = None;
                this.highlight_statement(statement_at(&spans, cursor), cx);
                if let Some(diags) = diags {
                    this.editor.update(cx, |e, cx| {
                        let rope = e.text().clone();
                        let converted = completion::editor_diagnostics(&rope, &diags);
                        if let Some(set) = e.diagnostics_mut() {
                            set.reset(&rope);
                            set.extend(converted);
                        }
                        cx.notify();
                    });
                }
            });
        }));
    }

    fn highlight_statement(&mut self, range: Option<Range<usize>>, cx: &mut Context<Self>) {
        let color = cx.theme().primary.opacity(0.06);
        let decorations: Vec<RangeDecoration> = range
            .into_iter()
            .map(|r| {
                RangeDecoration::new(r)
                    .with_style(RangeDecorationStyle::Fill)
                    .with_color(color)
            })
            .collect();
        match &self.statement_highlight {
            Some(c) => c.set(decorations, cx),
            None => {
                let c = self.editor.update(cx, |e, cx| {
                    e.create_range_decorations_collection(decorations, cx)
                });
                self.statement_highlight = Some(c);
            }
        }
    }

    /// Text to run and its offset in the document.
    pub fn script_for(&self, target: &RunTarget, cx: &App) -> Option<(String, usize, ExecMode)> {
        let e = self.editor.read(cx);
        let text = e.value().to_string();
        match target {
            RunTarget::Text(t) => Some((t.clone(), 0, ExecMode::WholeScript)),
            RunTarget::Script => Some((text, 0, ExecMode::WholeScript)),
            RunTarget::Statement => {
                let sel = e.selected_range();
                if sel.start < sel.end {
                    return Some((text[sel.clone()].to_owned(), sel.start, ExecMode::Selection));
                }
                let src = self.source(cx)?;
                let doc =
                    completion::snapshot(&text, &src.capabilities.language, Some(&src.config.id));
                let spans: Vec<Range<usize>> = AppState::backend(cx)
                    .language()
                    .statements(&doc)
                    .into_iter()
                    .map(|s| s.range)
                    .collect();
                let r = statement_at(&spans, e.cursor())?;
                Some((
                    text[r.clone()].to_owned(),
                    r.start,
                    ExecMode::StatementUnderCursor,
                ))
            }
        }
    }

    pub fn run(&mut self, target: RunTarget, window: &mut Window, cx: &mut Context<Self>) {
        let Some(src) = self.source(cx) else {
            window.push_notification(Notification::warning(t("console-no-data-source")), cx);
            return;
        };
        if self.running.is_some() {
            return;
        }
        let Some((text, base, mode)) = self.script_for(&target, cx) else {
            return;
        };
        if text.trim().is_empty() {
            return;
        }
        activity::snapshot(
            self.data_source.clone(),
            self.editor.read(cx).value().to_string(),
            "local-history-executed",
            cx,
        );
        let lang = src.capabilities.language.clone();
        let production = src.config.environment == Environment::Production;
        let risks = AppState::backend(cx)
            .language()
            .risks(&text, &lang, production);
        if risks.is_empty() {
            self.ask_params(text, base, mode, window, cx);
        } else {
            self.confirm_risks(risks, production, text, base, mode, window, cx);
        }
    }

    /// Plan 2.5: warnings for dangerous operations before they run.
    #[allow(clippy::too_many_arguments)]
    fn confirm_risks(
        &mut self,
        risks: Vec<Risk>,
        production: bool,
        text: String,
        base: usize,
        mode: ExecMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let weak = cx.entity().downgrade();
        let lines: Vec<String> = risks
            .iter()
            .map(|r| {
                let stmt = first_line(text.get(r.range.clone()).unwrap_or_default());
                format!("{}: {stmt}", risk_label(r.kind))
            })
            .collect();
        window.open_dialog(cx, move |dialog, _, _| {
            let (weak, text) = (weak.clone(), text.clone());
            dialog
                .title(t("risk-title"))
                .w(px(520.))
                .child(
                    v_flex()
                        .gap_2()
                        .when(production, |d| d.child(t("risk-production")))
                        .child(t("risk-description"))
                        .children(lines.iter().map(|l| div().text_sm().child(l.clone()))),
                )
                .footer(
                    DialogFooter::new()
                        .child(
                            Button::new("risk-cancel")
                                .label(t("button-cancel"))
                                .on_click(|_, w, cx| w.close_dialog(cx)),
                        )
                        .child(
                            Button::new("risk-run")
                                .danger()
                                .label(t("risk-run"))
                                .on_click(move |_, window, cx| {
                                    window.close_dialog(cx);
                                    let _ = weak.update(cx, |this, cx| {
                                        this.ask_params(text.clone(), base, mode, window, cx)
                                    });
                                }),
                        ),
                )
        });
    }

    /// Plan 2.5: script parameters (`:name`, `?`, `$1`) are asked before running.
    fn ask_params(
        &mut self,
        text: String,
        base: usize,
        mode: ExecMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.pending_params.clear();
        let Some(src) = self.source(cx) else { return };
        let params = AppState::backend(cx)
            .language()
            .parameters(&text, &src.capabilities.language);
        if params.is_empty() {
            self.launch(text, base, mode, window, cx);
            return;
        }
        let inputs: Vec<(String, Entity<InputState>)> = params
            .into_iter()
            .map(|p| {
                let last = self.param_values.get(&p.name).cloned().unwrap_or_default();
                let input = cx.new(|cx| {
                    let mut i = InputState::new(window, cx).placeholder(t("params-null"));
                    i.set_value(last, window, cx);
                    i
                });
                (p.name, input)
            })
            .collect();
        let weak = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, _, _| {
            let (weak, text, inputs2) = (weak.clone(), text.clone(), inputs.clone());
            dialog
                .title(t("params-title"))
                .w(px(480.))
                .child(
                    v_flex()
                        .gap_2()
                        .child(div().text_sm().child(t("params-description")))
                        .children(inputs.iter().map(|(name, input)| {
                            h_flex()
                                .gap_2()
                                .child(div().w(px(120.)).child(name.clone()))
                                .child(Input::new(input).small())
                        })),
                )
                .footer(
                    DialogFooter::new()
                        .child(
                            Button::new("params-cancel")
                                .label(t("button-cancel"))
                                .on_click(|_, w, cx| w.close_dialog(cx)),
                        )
                        .child(
                            Button::new("params-run")
                                .primary()
                                .label(t("params-run"))
                                .on_click(move |_, window, cx| {
                                    let values: Vec<(String, String)> = inputs2
                                        .iter()
                                        .map(|(n, i)| (n.clone(), i.read(cx).value().to_string()))
                                        .collect();
                                    window.close_dialog(cx);
                                    let _ = weak.update(cx, |this, cx| {
                                        this.param_values.extend(values.iter().cloned());
                                        this.pending_params = values
                                            .into_iter()
                                            .map(|(n, v)| (n, (!v.is_empty()).then_some(v)))
                                            .collect();
                                        this.launch(text.clone(), base, mode, window, cx);
                                    });
                                }),
                        ),
                )
        });
    }

    fn launch(
        &mut self,
        text: String,
        base: usize,
        mode: ExecMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(src) = self.source(cx) else { return };
        if src.capabilities.cost_estimate {
            self.estimate_then_run(text, base, mode, window, cx);
        } else {
            self.start(text, base, mode, window, cx);
        }
    }

    async fn ensure_session(
        this: &WeakEntity<Self>,
        ds: &DataSourceId,
        cx: &mut AsyncWindowContext,
    ) -> Result<SessionId, String> {
        if let Ok(Some(s)) = this.update(cx, |this, _| this.session) {
            return Ok(s);
        }
        let backend = cx
            .update(|_, cx| AppState::backend(cx))
            .map_err(|e| e.to_string())?;
        let ds2 = ds.clone();
        let s = cx
            .background_executor()
            .spawn(async move { backend.queries().open_session(&ds2).await })
            .await
            .map_err(|e| e.to_string())?;
        let _ = this.update(cx, |this, _| this.session = Some(s));
        Ok(s)
    }

    /// Plan 2.3: for sources with cost estimation, show the cost before running.
    fn estimate_then_run(
        &mut self,
        text: String,
        base: usize,
        mode: ExecMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ds) = self.data_source.clone() else {
            return;
        };
        let backend = AppState::backend(cx);
        cx.spawn_in(window, async move |this, cx| {
            let session = match Self::ensure_session(&this, &ds, cx).await {
                Ok(s) => s,
                Err(e) => {
                    let _ = this.update_in(cx, |this, w, cx| this.fail(e, w, cx));
                    return;
                }
            };
            let script = Script::new(text.clone());
            let estimate = cx
                .background_executor()
                .spawn(async move { backend.queries().estimate_cost(session, script).await })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                let bytes = estimate.ok().flatten();
                this.cost = bytes;
                cx.notify();
                let Some(bytes) = bytes else {
                    this.start(text, base, mode, window, cx);
                    return;
                };
                let weak = cx.entity().downgrade();
                let text = text.clone();
                window.open_dialog(cx, move |dialog, _, _| {
                    let (weak, text) = (weak.clone(), text.clone());
                    dialog
                        .title(t("cost-title"))
                        .w(px(440.))
                        .child(t_args(
                            "cost-description",
                            &[("size", format::bytes(bytes).into())],
                        ))
                        .footer(
                            DialogFooter::new()
                                .child(
                                    Button::new("cost-cancel")
                                        .label(t("button-cancel"))
                                        .on_click(|_, w, cx| w.close_dialog(cx)),
                                )
                                .child(
                                    Button::new("cost-run")
                                        .primary()
                                        .label(t("cost-run"))
                                        .on_click(move |_, window, cx| {
                                            window.close_dialog(cx);
                                            let _ = weak.update(cx, |this, cx| {
                                                this.start(text.clone(), base, mode, window, cx)
                                            });
                                        }),
                                ),
                        )
                });
            });
        })
        .detach();
    }

    fn fail(&mut self, e: String, window: &mut Window, cx: &mut Context<Self>) {
        self.running = None;
        self.push_message(MessageLevel::Error, e.clone());
        output::log(Level::Error, e.clone(), cx);
        window.push_notification(Notification::error(e), cx);
        cx.notify();
    }

    fn push_message(&mut self, level: MessageLevel, text: String) {
        self.messages.push(Message { level, text });
        if self.messages.len() > 1000 {
            self.messages.remove(0);
        }
    }

    /// Executes `text` and streams events into result tabs.
    pub fn start(
        &mut self,
        text: String,
        base: usize,
        mode: ExecMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(src) = self.source(cx) else { return };
        let ds = src.config.id.clone();
        let backend = AppState::backend(cx);
        let cancel = CancelHandle::new();
        self.running = Some(cancel.clone());
        self.tabs.clear();
        self.active_tab = 0;
        self.push_message(
            MessageLevel::Info,
            format!("{}> {}", src.config.name, first_line(&text)),
        );
        self.editor.update(cx, |e, cx| {
            if let Some(d) = e.diagnostics_mut() {
                d.clear();
            }
            cx.notify();
        });
        cx.notify();
        let caps = src.capabilities.clone();
        let read_only = src.config.read_only;
        let page_size = 500;
        let params = std::mem::take(&mut self.pending_params);
        let schema = self.schema.clone();
        cx.spawn_in(window, async move |this, cx| {
            let started = Instant::now();
            let session = match Self::ensure_session(&this, &ds, cx).await {
                Ok(s) => s,
                Err(e) => {
                    let _ = this.update_in(cx, |this, w, cx| this.fail(e, w, cx));
                    return;
                }
            };
            let script = Script {
                text: text.clone(),
                base_offset: base,
            };
            let opts = ExecOptions {
                mode,
                page_size,
                schema,
                params,
                // Risks were confirmed in `confirm_risks`.
                confirmed: true,
                ..Default::default()
            };
            let handle = backend.queries().execute(session, script, opts);
            let qid = handle.id;
            // Stop must reach the executor even while no event arrives (a
            // statement blocked on the server): hand the UI the executor's
            // own cancel handle.
            if cancel.is_cancelled() {
                handle.cancel.cancel();
            }
            let cancel = handle.cancel.clone();
            let _ = this.update(cx, |this, _| {
                if this.running.is_some() {
                    this.running = Some(cancel.clone());
                }
            });
            let _ = cx.update(|_, cx| {
                activity::started(
                    RunningQuery {
                        id: qid,
                        data_source: ds.clone(),
                        text: text.clone(),
                        started,
                        cancel: cancel.clone(),
                    },
                    cx,
                )
            });
            let mut models: HashMap<ferrodb_api::ResultId, Entity<ResultModel>> = HashMap::new();
            let mut finished = false;
            let mut result_ix = 0usize;
            let mut statement_text = text.clone();
            // Keep listening after `Finished`: fetch_more delivers on this handle.
            while let Ok(ev) = handle.events.recv().await {
                if cancel.is_cancelled() {
                    handle.cancel.cancel();
                }
                let alive = this.update_in(cx, |this, window, cx| match ev {
                    QueryEvent::StatementStarted { range, .. } => {
                        let local =
                            range.start.saturating_sub(base)..range.end.saturating_sub(base);
                        statement_text = text.get(local).unwrap_or(&text).to_owned();
                    }
                    QueryEvent::CostEstimate { bytes } => {
                        this.cost = Some(bytes);
                        this.push_message(
                            MessageLevel::Info,
                            t_args("cost-processed", &[("size", format::bytes(bytes).into())]),
                        );
                    }
                    QueryEvent::ResultStarted {
                        result,
                        shape,
                        title,
                    } => {
                        result_ix += 1;
                        let title = title.unwrap_or_else(|| {
                            t_args("console-result-title", &[("n", result_ix.into())])
                        });
                        let model = cx.new(|_| ResultModel {
                            id: result,
                            session,
                            data_source: ds.clone(),
                            capabilities: caps.clone(),
                            read_only,
                            title: title.clone(),
                            statement: statement_text.clone(),
                            data: ResultData::from_shape(&shape),
                            has_more: false,
                            fetching: true,
                            page_size,
                        });
                        models.insert(result, model.clone());
                        this.add_result_tab(title, model, window, cx);
                    }
                    QueryEvent::Data { result, chunk } => {
                        if let Some(m) = models.get(&result) {
                            m.update(cx, |m, cx| {
                                m.append(chunk);
                                cx.notify();
                            });
                        }
                    }
                    QueryEvent::ResultPaused { result, has_more } => {
                        if let Some(m) = models.get(&result) {
                            m.update(cx, |m, cx| {
                                m.paused(has_more);
                                cx.notify();
                            });
                        }
                    }
                    QueryEvent::Notice(n) => {
                        let level = match n.level {
                            ferrodb_api::NoticeLevel::Info => MessageLevel::Info,
                            ferrodb_api::NoticeLevel::Warning => MessageLevel::Warning,
                        };
                        this.push_message(level, n.message);
                    }
                    QueryEvent::StatementFinished {
                        affected, elapsed, ..
                    } => {
                        let ms = elapsed.as_millis() as u64;
                        let text = match affected {
                            Some(n) => t_args(
                                "console-affected-ms",
                                &[("count", n.into()), ("ms", ms.into())],
                            ),
                            None => t_args("console-statement-ms", &[("ms", ms.into())]),
                        };
                        this.push_message(MessageLevel::Info, text);
                    }
                    QueryEvent::Error(e) => {
                        if let Some(r) = e.range.clone() {
                            this.show_error_at(r, e.message.clone(), cx);
                        }
                        this.push_message(MessageLevel::Error, e.to_string());
                        this.active_tab = 0;
                        output::log(Level::Error, e.to_string(), cx);
                        window.push_notification(Notification::error(e.to_string()), cx);
                    }
                    QueryEvent::Finished => {
                        finished = true;
                        this.running = None;
                        let ms = started.elapsed().as_millis() as u64;
                        this.push_message(
                            MessageLevel::Info,
                            t_args("console-finished", &[("ms", ms.into())]),
                        );
                        output::info(t_args("console-finished", &[("ms", ms.into())]), cx);
                        activity::finished(qid, cx);
                    }
                });
                if alive.is_err() {
                    break;
                }
                let _ = this.update(cx, |_, cx| cx.notify());
            }
            let _ = this.update(cx, |this, cx| {
                if !finished {
                    this.running = None;
                    activity::finished(qid, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Marks an error position in the editor (offsets in the document).
    fn show_error_at(&mut self, range: Range<usize>, message: String, cx: &mut Context<Self>) {
        let diag = ferrodb_api::Diagnostic {
            range,
            severity: ferrodb_api::Severity::Error,
            message,
        };
        self.editor.update(cx, |e, cx| {
            let rope = e.text().clone();
            let d = completion::editor_diagnostics(&rope, &[diag]);
            if let Some(set) = e.diagnostics_mut() {
                set.reset(&rope);
                set.extend(d);
            }
            cx.notify();
        });
    }

    fn add_result_tab(
        &mut self,
        title: String,
        model: Entity<ResultModel>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let view = ResultView::new(model, window, cx);
        let sub = match &view {
            ResultView::Grid(v) => cx.subscribe_in(v, window, Self::on_result_event),
            ResultView::Documents(v) => cx.subscribe_in(v, window, Self::on_result_event),
            ResultView::Keys(v) => cx.subscribe_in(v, window, Self::on_result_event),
        };
        self.tabs.push(ConsoleTab::Result {
            title,
            view,
            _sub: sub,
        });
        if self.tabs.len() == 1 || self.active_tab == 0 {
            self.active_tab = self.tabs.len();
        }
    }

    fn on_result_event<V>(
        &mut self,
        _: &Entity<V>,
        ev: &ResultViewEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match ev {
            ResultViewEvent::Rerun(text) => self.run(RunTarget::Text(text.clone()), window, cx),
            ResultViewEvent::Applied => {
                self.push_message(MessageLevel::Info, t("changes-applied-short"));
                cx.notify();
            }
        }
    }

    /// The result view in tab `n` (1-based), for tests and actions.
    pub fn result_view(&self, n: usize) -> Option<&ResultView> {
        match self.tabs.get(n.checked_sub(1)?)? {
            ConsoleTab::Result { view, .. } => Some(view),
            ConsoleTab::Plan { .. } => None,
        }
    }

    pub fn explain(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(src) = self.source(cx) else { return };
        if src.capabilities.explain == ExplainSupport::None {
            window.push_notification(Notification::info(t("explain-unsupported")), cx);
            return;
        }
        let Some((text, _, _)) = self.script_for(&RunTarget::Statement, cx) else {
            return;
        };
        let ds = src.config.id.clone();
        let backend = AppState::backend(cx);
        cx.spawn_in(window, async move |this, cx| {
            let session = match Self::ensure_session(&this, &ds, cx).await {
                Ok(s) => s,
                Err(e) => {
                    let _ = this.update_in(cx, |this, w, cx| this.fail(e, w, cx));
                    return;
                }
            };
            let script = Script::new(text);
            let plan = cx
                .background_executor()
                .spawn(async move { backend.queries().explain(session, script).await })
                .await;
            let _ = this.update_in(cx, |this, window, cx| match plan {
                Ok(plan) => {
                    let view = cx.new(|cx| PlanView::new(plan, window, cx));
                    this.tabs.push(ConsoleTab::Plan {
                        title: t("explain-tab"),
                        view,
                    });
                    this.active_tab = this.tabs.len();
                    cx.notify();
                }
                Err(e) => this.fail(e.to_string(), window, cx),
            });
        })
        .detach();
    }

    fn transaction(&mut self, commit: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = self.session else {
            window.push_notification(Notification::info(t("tx-no-session")), cx);
            return;
        };
        let backend = AppState::backend(cx);
        let task = cx.background_executor().spawn(async move {
            if commit {
                backend.queries().commit(session).await
            } else {
                backend.queries().rollback(session).await
            }
        });
        cx.spawn_in(window, async move |this, cx| {
            let r = task.await;
            let _ = this.update_in(cx, |this, window, cx| match r {
                Ok(()) => {
                    let msg = t(if commit {
                        "tx-committed"
                    } else {
                        "tx-rolled-back"
                    });
                    this.push_message(MessageLevel::Info, msg.clone());
                    window.push_notification(Notification::success(msg), cx);
                    cx.notify();
                }
                Err(e) => this.fail(e.to_string(), window, cx),
            });
        })
        .detach();
    }

    fn stop(&mut self, _: &actions::StopQuery, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(c) = &self.running {
            c.cancel();
            self.push_message(MessageLevel::Warning, t("console-cancelled"));
            output::log(Level::Warning, t("console-cancelled"), cx);
            cx.notify();
        }
    }

    fn format_code(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(src) = self.source(cx) else { return };
        let text = self.editor.read(cx).value().to_string();
        match AppState::backend(cx).language().format(
            &text,
            &src.capabilities.language,
            &ferrodb_api::FormatStyle::default(),
        ) {
            Ok(f) => self.editor.update(cx, |e, cx| e.replace_all(f, window, cx)),
            Err(e) => window.push_notification(Notification::error(e.to_string()), cx),
        }
    }

    /// "Go to declaration" (reveal in the explorer) or "Quick documentation"
    /// of the name under the cursor, resolved against cached metadata.
    fn declaration(&mut self, doc: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(src) = self.source(cx) else { return };
        let (text, cursor) = {
            let e = self.editor.read(cx);
            (e.value().to_string(), e.cursor())
        };
        let mut parts = identifier_at(&text, cursor);
        if src.capabilities.language == ferrodb_api::QueryLanguage::Mql
            && parts.first().is_some_and(|p| p == "db")
        {
            parts.remove(0);
            // `db.coll.find` → the collection.
            parts.truncate(1);
        }
        if parts.is_empty() {
            return;
        }
        let hits = AppState::backend(cx)
            .metadata()
            .resolve(&src.config.id, &parts);
        let Some(hit) = hits.into_iter().next() else {
            window.push_notification(
                Notification::info(t_args(
                    "goto-not-found",
                    &[("name", parts.join(".").into())],
                )),
                cx,
            );
            return;
        };
        if doc {
            crate::workspace::show_documentation(hit.node, cx);
        } else {
            crate::workspace::reveal_in_explorer(hit.node, cx);
        }
    }

    fn not_yet(&mut self, phase: u8, window: &mut Window, cx: &mut Context<Self>) {
        window.push_notification(
            Notification::info(t_args("not-implemented-phase", &[("phase", phase.into())])),
            cx,
        );
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let source = self.source(cx);
        let marker = source
            .as_ref()
            .and_then(|s| environment_color(s.config.environment, cx));
        let running = self.running.is_some();
        let manual_tx = source.as_ref().is_some_and(|s| {
            s.config.tx_mode == TxMode::Manual && s.capabilities.supports_transactions()
        });
        let explain = source
            .as_ref()
            .is_some_and(|s| s.capabilities.explain != ExplainSupport::None);
        let sources = AppState::backend(cx).data_sources().list();
        let weak = cx.entity().downgrade();
        let tool = |id: &'static str, icon: icons::Lucide, tip: &str| {
            Button::new(id).ghost().xsmall().icon(icon).tooltip(t(tip))
        };
        h_flex()
            .gap_1()
            .px_1()
            .py_0p5()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                tool("console-run", icons::Lucide::Play, "action-execute")
                    .loading(running)
                    .on_click(|_, window, cx| {
                        window.dispatch_action(Box::new(actions::ExecuteStatement), cx)
                    }),
            )
            .child(
                tool(
                    "console-run-script",
                    icons::Lucide::ListVideo,
                    "action-execute-script",
                )
                .disabled(running)
                .on_click(|_, window, cx| {
                    window.dispatch_action(Box::new(actions::ExecuteScript), cx)
                }),
            )
            .child(
                tool("console-stop", icons::Lucide::Square, "action-stop")
                    .disabled(!running)
                    .on_click(|_, window, cx| {
                        window.dispatch_action(Box::new(actions::StopQuery), cx)
                    }),
            )
            .when(explain, |d| {
                d.child(
                    tool("console-explain", icons::Lucide::Network, "action-explain").on_click(
                        |_, window, cx| window.dispatch_action(Box::new(actions::ExplainPlan), cx),
                    ),
                )
            })
            .when(manual_tx, |d| {
                d.child(
                    tool("console-commit", icons::Lucide::Check, "action-commit")
                        .on_click(cx.listener(|this, _, w, cx| this.transaction(true, w, cx))),
                )
                .child(
                    tool("console-rollback", icons::Lucide::Undo2, "action-rollback")
                        .on_click(cx.listener(|this, _, w, cx| this.transaction(false, w, cx))),
                )
            })
            .when_some(self.cost, |d, bytes| {
                d.child(
                    div()
                        .id("console-cost")
                        .test_support()
                        .text_xs()
                        .text_color(cx.theme().warning)
                        .child(t_args(
                            "cost-label",
                            &[("size", format::bytes(bytes).into())],
                        )),
                )
            })
            .child(div().flex_1())
            .when_some(marker, |d, c| {
                d.child(div().size(px(8.)).rounded_full().bg(c))
            })
            .child(
                Button::new("console-source")
                    .ghost()
                    .xsmall()
                    .label(match &source {
                        Some(s) => format!(
                            "{} · {}",
                            s.config.name,
                            s.capabilities.language.display_name()
                        ),
                        None => t("console-no-data-source"),
                    })
                    .dropdown_menu(move |menu, _, _| {
                        sources.iter().fold(menu, |menu, s| {
                            let (id, weak) = (s.config.id.clone(), weak.clone());
                            menu.item(PopupMenuItem::new(s.config.name.clone()).on_click(
                                move |_, _, cx| {
                                    let _ = weak.update(cx, |this, cx| {
                                        this.set_data_source(Some(id.clone()), cx)
                                    });
                                },
                            ))
                        })
                    }),
            )
            .when(!self.schemas.is_empty(), |d| {
                let schemas = self.schemas.clone();
                let weak = cx.entity().downgrade();
                d.child(
                    Button::new("console-schema")
                        .ghost()
                        .xsmall()
                        .icon(icons::Lucide::Layers)
                        .label(self.schema.clone().unwrap_or_default())
                        .dropdown_menu(move |menu, _, _| {
                            schemas.iter().fold(menu, |menu, s| {
                                let (name, weak) = (s.clone(), weak.clone());
                                menu.item(PopupMenuItem::new(s.clone()).on_click(
                                    move |_, _, cx| {
                                        let _ = weak.update(cx, |this, cx| {
                                            this.schema = Some(name.clone());
                                            cx.notify();
                                        });
                                    },
                                ))
                            })
                        }),
                )
            })
    }

    fn render_results(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let body = match self.active_tab {
            0 => v_flex()
                .id("console-messages")
                .test_support()
                .size_full()
                .overflow_y_scroll()
                .p_1()
                .font_family(theme.mono_font_family.clone())
                .text_sm()
                .children(self.messages.iter().map(|m| {
                    let color = match m.level {
                        MessageLevel::Info => theme.foreground,
                        MessageLevel::Warning => theme.warning,
                        MessageLevel::Error => theme.danger,
                    };
                    div().text_color(color).child(m.text.clone())
                }))
                .into_any_element(),
            n => match self.tabs.get(n - 1) {
                Some(ConsoleTab::Result { view, .. }) => view.element(),
                Some(ConsoleTab::Plan { view, .. }) => view.clone().into_any_element(),
                None => div().into_any_element(),
            },
        };
        v_flex()
            .size_full()
            .child(
                TabBar::new("console-result-tabs")
                    .xsmall()
                    .selected_index(self.active_tab)
                    .child(Tab::new().label(t("console-output-tab")))
                    .children(
                        self.tabs
                            .iter()
                            .map(|tab| Tab::new().label(tab.title().to_owned())),
                    )
                    .on_click(cx.listener(|this, ix: &usize, _, cx| {
                        this.active_tab = *ix;
                        cx.notify();
                    })),
            )
            .child(div().flex_1().min_h_0().child(body))
    }
}

fn first_line(s: &str) -> String {
    let l = s.trim().lines().next().unwrap_or_default();
    if l.len() > 120 {
        format!("{}…", &l[..l.floor_char_boundary(120)])
    } else {
        l.to_owned()
    }
}

panel_boilerplate!(ConsolePanel);

impl BasePanel for ConsolePanel {
    fn panel_name(&self) -> &'static str {
        NAME
    }

    fn dump(&self, cx: &App) -> PanelState {
        let mut s = PanelState::new(NAME);
        let saved = SavedConsole {
            data_source: self.data_source.clone(),
            schema: self.schema.clone(),
            text: self.editor.read(cx).value().to_string(),
        };
        s.info = PanelInfo::panel(serde_json::to_value(saved).unwrap_or_default());
        s
    }
}

impl Panel for ConsolePanel {
    fn title(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let name = self.source_name(cx).unwrap_or_else(|| t("panel-console"));
        h_flex()
            .gap_1()
            .child(icons::console_icon().small())
            .child(name)
    }
}

impl Render for ConsolePanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let has_results = !self.tabs.is_empty() || !self.messages.is_empty();
        v_flex()
            .id("console")
            .test_support()
            .key_context(CONSOLE_CONTEXT)
            .track_focus(&self.focus)
            .size_full()
            .on_action(cx.listener(|this, _: &actions::ExecuteStatement, w, cx| {
                this.run(RunTarget::Statement, w, cx)
            }))
            .on_action(cx.listener(|this, _: &actions::ExecuteScript, w, cx| {
                this.run(RunTarget::Script, w, cx)
            }))
            .on_action(cx.listener(Self::stop))
            .on_action(cx.listener(|this, _: &actions::ExplainPlan, w, cx| this.explain(w, cx)))
            .on_action(cx.listener(|this, _: &actions::FormatCode, w, cx| this.format_code(w, cx)))
            .on_action(cx.listener(|this, _: &actions::Rename, w, cx| this.not_yet(5, w, cx)))
            .on_action(cx.listener(|this, _: &actions::GoToDeclaration, w, cx| {
                this.declaration(false, w, cx)
            }))
            .on_action(
                cx.listener(|this, _: &actions::LiveTemplates, w, cx| this.not_yet(5, w, cx)),
            )
            .on_action(cx.listener(|this, _: &actions::QuickDocumentation, w, cx| {
                this.declaration(true, w, cx)
            }))
            .child(self.render_header(cx))
            .child(
                div().flex_1().min_h_0().child(
                    v_resizable("console-split")
                        .child(resizable_panel().child(Editor::new(&self.editor).size_full()))
                        .when(has_results, |g| {
                            g.child(
                                resizable_panel()
                                    .size(px(380.))
                                    .child(self.render_results(cx)),
                            )
                        }),
                ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn names_under_the_cursor() {
        let s = "select * from sales.\"Orders\" o where o.id = 1";
        assert_eq!(identifier_at(s, 16), vec!["sales"]);
        assert_eq!(identifier_at(s, 23), vec!["sales", "Orders"]);
        assert_eq!(identifier_at(s, 40), vec!["o", "id"]);
        assert_eq!(identifier_at("db.orders.find({})", 5), vec!["db", "orders"]);
        assert!(identifier_at("a  b", 2).is_empty());
    }

    #[::core::prelude::v1::test]
    fn statement_under_cursor() {
        let spans = [0..8, 10..20, 25..30];
        assert_eq!(statement_at(&spans, 3), Some(0..8));
        assert_eq!(statement_at(&spans, 8), Some(0..8), "end of statement");
        assert_eq!(statement_at(&spans, 22), Some(10..20), "after a statement");
        assert_eq!(statement_at(&spans, 40), Some(25..30));
        assert_eq!(statement_at(&[], 0), None);
    }
}

//! Settings pages of phase 9: "AI Assistant" (provider, model, key, what is
//! shared) and "Agents & MCP" (server, connection, policy, audit log).
//! Both edit a draft and save it through the backend.

use ferrodb_api::{
    AgentAccess, AgentPolicy, AiProvider, AiSettings, AuditEntry, AuditOutcome, ConsentMode,
    DataSourceId, McpSettings,
};
use ferrodb_ui_kit::{
    gpui::{prelude::*, *},
    icons,
    kit::{
        ActiveTheme, Disableable as _, Sizable as _, WindowExt as _,
        button::{Button, ButtonVariants as _},
        h_flex,
        input::{Input, InputState},
        menu::{DropdownMenu as _, PopupMenuItem},
        notification::Notification,
        switch::Switch,
        v_flex,
    },
};

use crate::{
    AppState, agents,
    i18n::{t, t_args},
    output,
    panels::history::time_of_day,
};

/// Views of the last opened settings dialog (UI tests).
#[derive(Default)]
pub struct LastSettingsViews {
    pub ai: Option<Entity<AiSettingsView>>,
    pub agents: Option<Entity<AgentsSettingsView>>,
    /// "Updates and Privacy" (phase 10).
    pub release: Option<Entity<crate::release::ReleaseSettingsView>>,
}
impl Global for LastSettingsViews {}

pub(crate) fn label_cell(text: String, cx: &App) -> Div {
    div()
        .w(px(170.))
        .flex_shrink_0()
        .text_sm()
        .text_color(cx.theme().foreground)
        .child(text)
}

pub(crate) fn field_row(label: String, control: impl IntoElement, cx: &App) -> Div {
    h_flex()
        .gap_2()
        .items_center()
        .child(label_cell(label, cx))
        .child(div().flex_1().min_w_0().child(control))
}

pub(crate) fn section(title: String, cx: &App) -> Div {
    div()
        .pt_2()
        .text_sm()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(cx.theme().muted_foreground)
        .child(title)
}

pub(crate) fn outcome(r: &Result<String, String>, cx: &App) -> Div {
    let theme = cx.theme();
    match r {
        Ok(m) => div().text_sm().text_color(theme.success).child(m.clone()),
        Err(e) => div().text_sm().text_color(theme.danger).child(e.clone()),
    }
}

/// A button showing the current choice with a menu of options.
pub(crate) fn choice<V: 'static, T: Copy + 'static>(
    id: impl Into<ElementId>,
    current: String,
    options: Vec<(T, String)>,
    view: WeakEntity<V>,
    on: fn(&mut V, T, &mut Context<V>),
) -> impl IntoElement {
    Button::new(id)
        .small()
        .outline()
        .label(current)
        .dropdown_menu(move |menu, _, _| {
            options.iter().fold(menu, |menu, (value, label)| {
                let (value, view) = (*value, view.clone());
                menu.item(PopupMenuItem::new(label.clone()).on_click(move |_, _, cx| {
                    let _ = view.update(cx, |v, cx| {
                        on(v, value, cx);
                        cx.notify();
                    });
                }))
            })
        })
}

// ---------------------------------------------------------------------------
// AI Assistant
// ---------------------------------------------------------------------------

pub struct AiSettingsView {
    pub draft: AiSettings,
    endpoint: Entity<InputState>,
    model: Entity<InputState>,
    api_key: Entity<InputState>,
    max_tokens: Entity<InputState>,
    schema_budget: Entity<InputState>,
    /// "Clear" was pressed: the stored key is removed on save.
    pub clear_key: bool,
    /// Models offered by the provider ("Load models").
    pub models: Option<Result<Vec<String>, String>>,
    /// Outcome of the last save / test.
    pub outcome: Option<Result<String, String>>,
    pub busy: bool,
}

impl AiSettingsView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let draft = AppState::backend(cx).assistant().settings();
        let input = |value: String, placeholder: String, window: &mut Window, cx: &mut App| {
            cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(placeholder)
                    .default_value(value)
            })
        };
        let endpoint = input(
            draft.endpoint.clone(),
            draft.provider.default_endpoint().into(),
            window,
            cx,
        );
        let model = input(
            draft.model.clone(),
            draft.provider.default_model().unwrap_or_default().into(),
            window,
            cx,
        );
        let api_key = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder(t("ai-key-placeholder"))
        });
        let max_tokens = input(draft.max_tokens.to_string(), String::new(), window, cx);
        let schema_budget = input(draft.schema_budget.to_string(), String::new(), window, cx);
        Self {
            draft,
            endpoint,
            model,
            api_key,
            max_tokens,
            schema_budget,
            clear_key: false,
            models: None,
            outcome: None,
            busy: false,
        }
    }

    pub fn set_enabled(&mut self, v: bool, cx: &mut Context<Self>) {
        self.draft.enabled = v;
        cx.notify();
    }

    pub fn set_provider(&mut self, p: AiProvider, window: &mut Window, cx: &mut Context<Self>) {
        self.draft.provider = p;
        self.models = None;
        self.endpoint.update(cx, |i, cx| {
            i.set_placeholder(p.default_endpoint(), window, cx)
        });
        self.model.update(cx, |i, cx| {
            i.set_placeholder(p.default_model().unwrap_or_default(), window, cx)
        });
        cx.notify();
    }

    pub fn set_share_schema(&mut self, v: bool, cx: &mut Context<Self>) {
        self.draft.share_schema = v;
        cx.notify();
    }

    pub fn set_share_samples(&mut self, v: bool, cx: &mut Context<Self>) {
        self.draft.share_samples = v;
        cx.notify();
    }

    /// Removes the stored API key on the next save.
    pub fn clear_api_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.clear_key = true;
        self.api_key
            .update(cx, |i, cx| i.set_value(String::new(), window, cx));
        cx.notify();
    }

    pub fn set_field(&mut self, field: AiField, value: &str, window: &mut Window, cx: &mut App) {
        let input = match field {
            AiField::Endpoint => &self.endpoint,
            AiField::Model => &self.model,
            AiField::ApiKey => &self.api_key,
            AiField::MaxTokens => &self.max_tokens,
            AiField::SchemaBudget => &self.schema_budget,
        };
        input.update(cx, |i, cx| i.set_value(value.to_owned(), window, cx));
    }

    /// The settings and the key change to save (`None` = keep the key).
    pub fn collect(&self, cx: &App) -> (AiSettings, Option<String>) {
        let mut s = self.draft.clone();
        s.endpoint = self.endpoint.read(cx).value().trim().to_owned();
        s.model = self.model.read(cx).value().trim().to_owned();
        if let Ok(n) = self.max_tokens.read(cx).value().trim().parse::<u32>() {
            s.max_tokens = n.max(1);
        }
        if let Ok(n) = self.schema_budget.read(cx).value().trim().parse::<usize>() {
            s.schema_budget = n;
        }
        let typed = self.api_key.read(cx).value().trim().to_owned();
        let key = if !typed.is_empty() {
            Some(typed)
        } else if self.clear_key {
            Some(String::new())
        } else {
            None
        };
        (s, key)
    }

    /// Saves; with `test` also checks the provider afterwards.
    pub fn save(&mut self, test: bool, window: &mut Window, cx: &mut Context<Self>) {
        let (settings, key) = self.collect(cx);
        let backend = AppState::backend(cx);
        self.busy = true;
        self.outcome = None;
        cx.notify();
        let task = cx.background_executor().spawn(async move {
            if let Err(e) = backend.assistant().save_settings(settings, key).await {
                return (false, Err(e.to_string()));
            }
            if !test {
                return (true, Ok(None));
            }
            let r = backend.assistant().test().await;
            (true, r.map(Some).map_err(|e| e.to_string()))
        });
        cx.spawn_in(window, async move |this, cx| {
            let (saved, r) = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                if saved {
                    this.draft.has_key = AppState::backend(cx).assistant().settings().has_key;
                    this.clear_key = false;
                    this.api_key
                        .update(cx, |i, cx| i.set_value(String::new(), window, cx));
                    agents::bump_ai_settings(cx);
                }
                this.outcome = Some(match r {
                    Ok(Some(model)) => Ok(t_args("ai-test-ok", &[("model", model.into())])),
                    Ok(None) => Ok(t("settings-saved")),
                    Err(e) => {
                        output::log(output::Level::Error, format!("AI: {e}"), cx);
                        Err(e)
                    }
                });
                cx.notify();
            });
        })
        .detach();
    }

    /// Loads the provider's models for the picker (saves first, so the
    /// provider and endpoint in the form are used).
    pub fn load_models(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (settings, key) = self.collect(cx);
        let backend = AppState::backend(cx);
        self.models = None;
        self.busy = true;
        cx.notify();
        let task = cx.background_executor().spawn(async move {
            backend
                .assistant()
                .save_settings(settings, key)
                .await
                .map_err(|e| e.to_string())?;
            backend
                .assistant()
                .models()
                .await
                .map_err(|e| e.to_string())
        });
        cx.spawn_in(window, async move |this, cx| {
            let r = task.await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                this.models = Some(r);
                cx.notify();
            });
        })
        .detach();
    }

    pub fn choose_model(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.model
            .update(cx, |i, cx| i.set_value(name.to_owned(), window, cx));
        cx.notify();
    }
}

/// Text fields of the AI settings page (tests type into them).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AiField {
    Endpoint,
    Model,
    ApiKey,
    MaxTokens,
    SchemaBudget,
}

impl Render for AiSettingsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let backend = AppState::backend(cx);
        if !backend.assistant().available() {
            return v_flex()
                .id("ai-settings")
                .test_support()
                .p_2()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(t("ai-unavailable"));
        }
        let weak = cx.entity().downgrade();
        let d = self.draft.clone();
        let providers: Vec<(AiProvider, String)> = AiProvider::ALL
            .into_iter()
            .map(|p| (p, p.label().to_owned()))
            .collect();
        let key_state = if self.clear_key {
            t("ai-key-cleared")
        } else if d.has_key {
            t("ai-key-stored")
        } else if d.provider.needs_key() {
            t("ai-key-missing")
        } else {
            t("ai-key-optional")
        };
        let models = match &self.models {
            Some(Ok(list)) if !list.is_empty() => {
                let w = weak.clone();
                let list = list.clone();
                Some(
                    Button::new("ai-model-pick")
                        .small()
                        .ghost()
                        .icon(icons::Lucide::ChevronDown)
                        .tooltip(t("ai-models-choose"))
                        .dropdown_menu(move |menu, _, _| {
                            list.iter().fold(menu, |menu, m| {
                                let (m, w) = (m.clone(), w.clone());
                                menu.item(PopupMenuItem::new(m.clone()).on_click(
                                    move |_, window, cx| {
                                        let _ =
                                            w.update(cx, |v, cx| v.choose_model(&m, window, cx));
                                    },
                                ))
                            })
                        })
                        .into_any_element(),
                )
            }
            Some(Ok(_)) => Some(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(t("ai-models-none"))
                    .into_any_element(),
            ),
            Some(Err(e)) => Some(
                div()
                    .text_xs()
                    .text_color(cx.theme().danger)
                    .child(e.clone())
                    .into_any_element(),
            ),
            None => None,
        };
        v_flex()
            .id("ai-settings")
            .test_support()
            .gap_2()
            .p_1()
            .child(field_row(
                t("ai-enabled"),
                Switch::new("ai-enabled")
                    .checked(d.enabled)
                    .on_click(cx.listener(|this, v: &bool, _, cx| this.set_enabled(*v, cx))),
                cx,
            ))
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(t("ai-enabled-description")),
            )
            .child(section(t("ai-provider-section"), cx))
            .child(field_row(
                t("ai-provider"),
                Button::new("ai-provider")
                    .small()
                    .outline()
                    .label(d.provider.label())
                    .dropdown_menu({
                        let weak = weak.clone();
                        move |menu, _, _| {
                            providers.iter().fold(menu, |menu, (p, label)| {
                                let (p, w) = (*p, weak.clone());
                                menu.item(PopupMenuItem::new(label.clone()).on_click(
                                    move |_, window, cx| {
                                        let _ = w.update(cx, |v, cx| v.set_provider(p, window, cx));
                                    },
                                ))
                            })
                        }
                    }),
                cx,
            ))
            .child(field_row(
                t("ai-endpoint"),
                Input::new(&self.endpoint).small(),
                cx,
            ))
            .child(field_row(
                t("ai-model"),
                h_flex()
                    .gap_1()
                    .child(div().flex_1().child(Input::new(&self.model).small()))
                    .child(
                        Button::new("ai-load-models")
                            .small()
                            .label(t("ai-load-models"))
                            .disabled(self.busy)
                            .on_click(cx.listener(|this, _, w, cx| this.load_models(w, cx))),
                    )
                    .children(models),
                cx,
            ))
            .child(field_row(
                t("ai-api-key"),
                h_flex()
                    .gap_1()
                    .child(div().flex_1().child(Input::new(&self.api_key).small()))
                    .child(
                        div()
                            .id("ai-key-state")
                            .test_support()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(key_state),
                    )
                    .child(
                        Button::new("ai-clear-key")
                            .small()
                            .ghost()
                            .label(t("ai-clear-key"))
                            .disabled(!d.has_key || self.clear_key)
                            .on_click(cx.listener(|this, _, w, cx| this.clear_api_key(w, cx))),
                    ),
                cx,
            ))
            .child(section(t("ai-privacy-section"), cx))
            .child(field_row(
                t("ai-share-schema"),
                Switch::new("ai-share-schema")
                    .checked(d.share_schema)
                    .on_click(cx.listener(|this, v: &bool, _, cx| this.set_share_schema(*v, cx))),
                cx,
            ))
            .child(field_row(
                t("ai-share-samples"),
                Switch::new("ai-share-samples")
                    .checked(d.share_samples)
                    .on_click(cx.listener(|this, v: &bool, _, cx| this.set_share_samples(*v, cx))),
                cx,
            ))
            .child(field_row(
                t("ai-max-tokens"),
                Input::new(&self.max_tokens).small(),
                cx,
            ))
            .child(field_row(
                t("ai-schema-budget"),
                Input::new(&self.schema_budget).small(),
                cx,
            ))
            .child(
                h_flex()
                    .gap_2()
                    .pt_2()
                    .child(
                        Button::new("ai-save")
                            .small()
                            .primary()
                            .label(t("settings-save"))
                            .loading(self.busy)
                            .on_click(cx.listener(|this, _, w, cx| this.save(false, w, cx))),
                    )
                    .child(
                        Button::new("ai-test")
                            .small()
                            .label(t("ai-test"))
                            .disabled(self.busy)
                            .on_click(cx.listener(|this, _, w, cx| this.save(true, w, cx))),
                    )
                    .children(self.outcome.as_ref().map(|o| outcome(o, cx))),
            )
    }
}

// ---------------------------------------------------------------------------
// Agents & MCP
// ---------------------------------------------------------------------------

fn access_label(a: AgentAccess) -> String {
    t(match a {
        AgentAccess::Hidden => "agent-access-hidden",
        AgentAccess::MetadataOnly => "agent-access-metadata",
        AgentAccess::ReadOnly => "agent-access-read-only",
        AgentAccess::ReadWrite => "agent-access-read-write",
    })
}

const ACCESS: [AgentAccess; 4] = [
    AgentAccess::Hidden,
    AgentAccess::MetadataOnly,
    AgentAccess::ReadOnly,
    AgentAccess::ReadWrite,
];

fn consent_label(c: ConsentMode) -> String {
    t(match c {
        ConsentMode::EveryCall => "agent-consent-every-call",
        ConsentMode::QueriesOnly => "agent-consent-queries",
        ConsentMode::WritesOnly => "agent-consent-writes",
    })
}

fn outcome_label(o: AuditOutcome) -> String {
    t(match o {
        AuditOutcome::AllowedByPolicy => "audit-allowed-policy",
        AuditOutcome::AllowedByUser => "audit-allowed-user",
        AuditOutcome::DeniedByPolicy => "audit-denied-policy",
        AuditOutcome::DeniedByUser => "audit-denied-user",
        AuditOutcome::TimedOut => "audit-timed-out",
    })
}

/// Number of audit entries shown.
const AUDIT_LIMIT: usize = 200;

/// What "Copy" copies from the connection info.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopyWhat {
    Url,
    Token,
    ClientConfig,
}

pub struct AgentsSettingsView {
    pub mcp: McpSettings,
    pub policy: AgentPolicy,
    port: Entity<InputState>,
    max_rows: Entity<InputState>,
    timeout: Entity<InputState>,
    pub audit: Vec<AuditEntry>,
    audit_rev: u64,
    pub outcome: Option<Result<String, String>>,
    pub busy: bool,
    _subs: Vec<Subscription>,
}

impl AgentsSettingsView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let backend = AppState::backend(cx);
        let mcp = backend.agents().mcp_settings();
        let policy = backend.agents().policy();
        let input = |value: String, window: &mut Window, cx: &mut App| {
            cx.new(|cx| InputState::new(window, cx).default_value(value))
        };
        let port = input(mcp.port.to_string(), window, cx);
        let max_rows = input(policy.max_rows.to_string(), window, cx);
        let timeout = input(policy.consent_timeout_secs.to_string(), window, cx);
        let subs = vec![
            cx.observe_global::<agents::AgentsHub>(|this: &mut Self, cx| {
                let rev = agents::hub(cx).map_or(0, |h| h.audit_rev);
                if rev != this.audit_rev {
                    this.audit_rev = rev;
                    this.reload_audit(cx);
                }
                cx.notify();
            }),
        ];
        let mut this = Self {
            mcp,
            policy,
            port,
            max_rows,
            timeout,
            audit: Vec::new(),
            audit_rev: agents::hub(cx).map_or(0, |h| h.audit_rev),
            outcome: None,
            busy: false,
            _subs: subs,
        };
        if backend.agents().available() {
            this.reload_audit(cx);
        }
        this
    }

    pub fn reload_audit(&mut self, cx: &mut Context<Self>) {
        let backend = AppState::backend(cx);
        let task = cx
            .background_executor()
            .spawn(async move { backend.agents().audit(AUDIT_LIMIT).await });
        cx.spawn(async move |this, cx| {
            if let Ok(mut entries) = task.await {
                // Newest first, whatever order the backend used.
                entries.sort_by_key(|e| std::cmp::Reverse(e.at));
                let _ = this.update(cx, |this, cx| {
                    this.audit = entries;
                    cx.notify();
                });
            }
        })
        .detach();
    }

    pub fn set_mcp_enabled(&mut self, v: bool, cx: &mut Context<Self>) {
        self.mcp.enabled = v;
        cx.notify();
    }

    pub fn set_default_access(&mut self, a: AgentAccess, cx: &mut Context<Self>) {
        self.policy.default_access = a;
        cx.notify();
    }

    /// Access of one source; `None` = follow the default.
    pub fn set_source_access(
        &mut self,
        ds: DataSourceId,
        a: Option<AgentAccess>,
        cx: &mut Context<Self>,
    ) {
        self.policy.sources.retain(|(id, _)| *id != ds);
        if let Some(a) = a {
            self.policy.sources.push((ds, a));
        }
        cx.notify();
    }

    pub fn set_consent(&mut self, c: ConsentMode, cx: &mut Context<Self>) {
        self.policy.consent = c;
        cx.notify();
    }

    pub fn set_field(&mut self, field: AgentField, value: &str, window: &mut Window, cx: &mut App) {
        let input = match field {
            AgentField::Port => &self.port,
            AgentField::MaxRows => &self.max_rows,
            AgentField::Timeout => &self.timeout,
        };
        input.update(cx, |i, cx| i.set_value(value.to_owned(), window, cx));
    }

    /// The MCP settings and policy in the form.
    pub fn collect(&self, cx: &App) -> (McpSettings, AgentPolicy) {
        let mut mcp = self.mcp.clone();
        if let Ok(p) = self.port.read(cx).value().trim().parse::<u16>() {
            mcp.port = p;
        }
        let mut policy = self.policy.clone();
        if let Ok(n) = self.max_rows.read(cx).value().trim().parse::<usize>() {
            policy.max_rows = n.max(1);
        }
        if let Ok(n) = self.timeout.read(cx).value().trim().parse::<u64>() {
            policy.consent_timeout_secs = n.max(1);
        }
        (mcp, policy)
    }

    /// Saves the policy and the MCP settings (the server starts / stops).
    pub fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (mcp, policy) = self.collect(cx);
        let backend = AppState::backend(cx);
        self.busy = true;
        self.outcome = None;
        cx.notify();
        let task = cx.background_executor().spawn(async move {
            backend.agents().save_policy(policy).await?;
            backend.agents().save_mcp_settings(mcp).await
        });
        cx.spawn_in(window, async move |this, cx| {
            let r = task.await;
            let _ = this.update_in(cx, |this, _, cx| {
                this.busy = false;
                let backend = AppState::backend(cx);
                this.mcp = backend.agents().mcp_settings();
                this.policy = backend.agents().policy();
                let status = backend.agents().status();
                cx.update_global::<agents::AgentsHub, _>(|h, _| h.status = status);
                this.outcome = Some(match r {
                    Ok(()) => Ok(t("settings-saved")),
                    Err(e) => {
                        output::log(output::Level::Error, format!("MCP: {e}"), cx);
                        Err(e.to_string())
                    }
                });
                cx.notify();
            });
        })
        .detach();
    }

    pub fn copy(&mut self, what: CopyWhat, window: &mut Window, cx: &mut Context<Self>) {
        let Some(info) = AppState::backend(cx).agents().connection_info() else {
            window.push_notification(Notification::info(t("mcp-not-running")), cx);
            return;
        };
        let text = match what {
            CopyWhat::Url => info.url,
            CopyWhat::Token => info.token,
            CopyWhat::ClientConfig => info.client_config,
        };
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        window.push_notification(Notification::info(t("mcp-copied")), cx);
    }

    pub fn regenerate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let backend = AppState::backend(cx);
        let task = cx
            .background_executor()
            .spawn(async move { backend.agents().regenerate_token().await });
        cx.spawn_in(window, async move |this, cx| {
            let r = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                match r {
                    Ok(()) => window
                        .push_notification(Notification::success(t("mcp-token-regenerated")), cx),
                    Err(e) => {
                        this.outcome = Some(Err(e.to_string()));
                        window.push_notification(Notification::error(e.to_string()), cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn render_status(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let status = agents::hub(cx)
            .map(|h| h.status.clone())
            .unwrap_or_else(|| AppState::backend(cx).agents().status());
        let theme = cx.theme().clone();
        let running = status.running;
        let tool = |id: &'static str, label: &str| Button::new(id).xsmall().label(t(label));
        v_flex()
            .id("mcp-status")
            .test_support()
            .gap_1()
            .child(
                h_flex()
                    .gap_2()
                    .text_sm()
                    .child(div().size(px(8.)).rounded_full().bg(if running {
                        theme.success
                    } else {
                        theme.muted_foreground
                    }))
                    .child(if running {
                        t("mcp-running")
                    } else {
                        t("mcp-stopped")
                    })
                    .children(
                        status
                            .url
                            .clone()
                            .map(|u| div().font_family(theme.mono_font_family.clone()).child(u)),
                    ),
            )
            .child(div().text_sm().text_color(theme.muted_foreground).child(
                if status.clients.is_empty() {
                    t("mcp-no-clients")
                } else {
                    t_args(
                        "mcp-clients",
                        &[("names", status.clients.join(", ").into())],
                    )
                },
            ))
            .children(
                status
                    .error
                    .clone()
                    .map(|e| div().text_sm().text_color(theme.danger).child(e)),
            )
            .child(
                h_flex()
                    .gap_1()
                    .flex_wrap()
                    .child(
                        tool("mcp-copy-url", "mcp-copy-url")
                            .disabled(!running)
                            .on_click(
                                cx.listener(|this, _, w, cx| this.copy(CopyWhat::Url, w, cx)),
                            ),
                    )
                    .child(
                        tool("mcp-copy-token", "mcp-copy-token")
                            .disabled(!running)
                            .on_click(
                                cx.listener(|this, _, w, cx| this.copy(CopyWhat::Token, w, cx)),
                            ),
                    )
                    .child(
                        tool("mcp-copy-config", "mcp-copy-config")
                            .disabled(!running)
                            .on_click(cx.listener(|this, _, w, cx| {
                                this.copy(CopyWhat::ClientConfig, w, cx)
                            })),
                    )
                    .child(
                        tool("mcp-regenerate", "mcp-regenerate")
                            .on_click(cx.listener(|this, _, w, cx| this.regenerate(w, cx))),
                    ),
            )
    }

    fn render_sources(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let sources = AppState::backend(cx).data_sources().list();
        let weak = cx.entity().downgrade();
        let theme = cx.theme().clone();
        v_flex()
            .id("agent-sources")
            .test_support()
            .gap_0p5()
            .children(sources.into_iter().enumerate().map(|(i, s)| {
                let id = s.config.id.clone();
                let explicit = self
                    .policy
                    .sources
                    .iter()
                    .find(|(d, _)| *d == id)
                    .map(|(_, a)| *a);
                let current = match explicit {
                    Some(a) => access_label(a),
                    None => t_args(
                        "agent-access-default",
                        &[("access", access_label(self.policy.default_access).into())],
                    ),
                };
                let mut options: Vec<(Option<AgentAccess>, String)> = vec![(
                    None,
                    t_args(
                        "agent-access-default",
                        &[("access", access_label(self.policy.default_access).into())],
                    ),
                )];
                options.extend(ACCESS.iter().map(|a| (Some(*a), access_label(*a))));
                let w = weak.clone();
                h_flex()
                    .gap_2()
                    .text_sm()
                    .child(
                        div()
                            .flex_1()
                            .overflow_hidden()
                            .child(s.config.name.clone()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(s.capabilities.language.display_name()),
                    )
                    .child(
                        Button::new(("agent-source", i))
                            .xsmall()
                            .outline()
                            .label(current)
                            .dropdown_menu(move |menu, _, _| {
                                options.iter().fold(menu, |menu, (a, label)| {
                                    let (a, w, id) = (*a, w.clone(), id.clone());
                                    menu.item(PopupMenuItem::new(label.clone()).on_click(
                                        move |_, _, cx| {
                                            let _ = w.update(cx, |v, cx| {
                                                v.set_source_access(id.clone(), a, cx)
                                            });
                                        },
                                    ))
                                })
                            }),
                    )
            }))
    }

    fn render_audit(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let sources = AppState::backend(cx).data_sources().list();
        v_flex()
            .id("agent-audit")
            .test_support()
            .gap_0p5()
            .max_h(px(220.))
            .overflow_y_scroll()
            .text_xs()
            .when(self.audit.is_empty(), |d| {
                d.child(
                    div()
                        .text_color(theme.muted_foreground)
                        .child(t("audit-empty")),
                )
            })
            .children(self.audit.iter().map(|e| {
                let source = e
                    .data_source
                    .as_ref()
                    .map(|id| {
                        sources
                            .iter()
                            .find(|s| &s.config.id == id)
                            .map(|s| s.config.name.clone())
                            .unwrap_or_else(|| id.to_string())
                    })
                    .unwrap_or_default();
                let denied = matches!(
                    e.outcome,
                    AuditOutcome::DeniedByPolicy
                        | AuditOutcome::DeniedByUser
                        | AuditOutcome::TimedOut
                );
                let query = e
                    .text
                    .as_deref()
                    .map(|q| {
                        let l = q.trim().lines().next().unwrap_or_default();
                        if l.chars().count() > 80 {
                            format!("{}…", l.chars().take(80).collect::<String>())
                        } else {
                            l.to_owned()
                        }
                    })
                    .unwrap_or_default();
                v_flex()
                    .py_0p5()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                div()
                                    .w(px(56.))
                                    .text_color(theme.muted_foreground)
                                    .child(time_of_day(e.at)),
                            )
                            .child(div().w(px(90.)).overflow_hidden().child(e.client.clone()))
                            .child(div().w(px(90.)).overflow_hidden().child(e.tool.clone()))
                            .child(div().w(px(90.)).overflow_hidden().child(source))
                            .child(
                                div()
                                    .text_color(if denied { theme.danger } else { theme.success })
                                    .child(outcome_label(e.outcome)),
                            ),
                    )
                    .when(!query.is_empty(), |d| {
                        d.child(
                            div()
                                .pl(px(64.))
                                .font_family(theme.mono_font_family.clone())
                                .text_color(theme.muted_foreground)
                                .child(query),
                        )
                    })
                    .children(
                        e.error
                            .clone()
                            .map(|err| div().pl(px(64.)).text_color(theme.danger).child(err)),
                    )
            }))
    }
}

/// Number fields of the Agents settings page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentField {
    Port,
    MaxRows,
    Timeout,
}

impl Render for AgentsSettingsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !AppState::backend(cx).agents().available() {
            return v_flex()
                .id("agents-settings")
                .test_support()
                .p_2()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(t("agents-unavailable"));
        }
        let weak = cx.entity().downgrade();
        let access: Vec<(AgentAccess, String)> =
            ACCESS.iter().map(|a| (*a, access_label(*a))).collect();
        let consent: Vec<(ConsentMode, String)> = [
            ConsentMode::EveryCall,
            ConsentMode::QueriesOnly,
            ConsentMode::WritesOnly,
        ]
        .into_iter()
        .map(|c| (c, consent_label(c)))
        .collect();
        v_flex()
            .id("agents-settings")
            .test_support()
            .gap_2()
            .p_1()
            .child(section(t("mcp-section"), cx))
            .child(field_row(
                t("mcp-enabled"),
                Switch::new("mcp-enabled")
                    .checked(self.mcp.enabled)
                    .on_click(cx.listener(|this, v: &bool, _, cx| this.set_mcp_enabled(*v, cx))),
                cx,
            ))
            .child(field_row(
                t("mcp-port"),
                div().w(px(120.)).child(Input::new(&self.port).small()),
                cx,
            ))
            .child(self.render_status(cx))
            .child(section(t("agent-policy-section"), cx))
            .child(field_row(
                t("agent-default-access"),
                choice(
                    "agent-default-access",
                    access_label(self.policy.default_access),
                    access,
                    weak.clone(),
                    |v: &mut Self, a, cx| v.set_default_access(a, cx),
                ),
                cx,
            ))
            .child(field_row(
                t("agent-consent"),
                choice(
                    "agent-consent",
                    consent_label(self.policy.consent),
                    consent,
                    weak,
                    |v: &mut Self, c, cx| v.set_consent(c, cx),
                ),
                cx,
            ))
            .child(field_row(
                t("agent-max-rows"),
                div().w(px(120.)).child(Input::new(&self.max_rows).small()),
                cx,
            ))
            .child(field_row(
                t("agent-timeout"),
                div().w(px(120.)).child(Input::new(&self.timeout).small()),
                cx,
            ))
            .child(div().text_sm().child(t("agent-sources")))
            .child(self.render_sources(cx))
            .child(
                h_flex()
                    .gap_2()
                    .pt_2()
                    .child(
                        Button::new("agents-save")
                            .small()
                            .primary()
                            .label(t("settings-save"))
                            .loading(self.busy)
                            .on_click(cx.listener(|this, _, w, cx| this.save(w, cx))),
                    )
                    .children(self.outcome.as_ref().map(|o| outcome(o, cx))),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(section(t("audit-section"), cx))
                    .child(div().flex_1())
                    .child(
                        Button::new("audit-refresh")
                            .xsmall()
                            .ghost()
                            .icon(icons::Lucide::RefreshCw)
                            .tooltip(t("audit-refresh"))
                            .on_click(cx.listener(|this, _, _, cx| this.reload_audit(cx))),
                    ),
            )
            .child(self.render_audit(cx))
    }
}

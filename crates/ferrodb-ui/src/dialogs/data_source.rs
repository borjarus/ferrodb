//! "New / edit data source" dialog (plan 2.2): the 26 databases grouped and
//! searchable, templates, a form generated from the driver description
//! (fields per authentication method), General / SSH / SSL / Options /
//! Schemas tabs, environment, read-only and transaction mode, the driver
//! status with an in-place download, and "Test connection".

use std::collections::HashMap;

use ferrodb_api::{
    AuthMethod, DataSourceConfig, DataSourceId, DriverCategory, DriverDescriptor, DriverEvent,
    DriverId, DriverStatus, Environment, SecretValues, TxMode,
};
use ferrodb_ui_kit::{
    gpui::{prelude::*, *},
    icons,
    kit::{
        ActiveTheme, Disableable as _, Selectable as _, Sizable as _, WindowExt as _,
        button::{Button, ButtonGroup, ButtonVariants as _},
        checkbox::Checkbox,
        dialog::DialogFooter,
        h_flex,
        input::{Input, InputState},
        list::ListItem,
        notification::Notification,
        tab::{Tab, TabBar},
        v_flex,
    },
    theme::environment_color,
};

use crate::{
    AppState,
    i18n::{t, t_args},
    output,
};

/// Ready-made starting points (driver + property overrides).
/// (message id, driver id, property overrides).
pub type Template = (
    &'static str,
    &'static str,
    &'static [(&'static str, &'static str)],
);

pub const TEMPLATES: &[Template] = &[
    (
        "template-pg-docker",
        "postgresql",
        &[
            ("host", "localhost"),
            ("port", "5432"),
            ("database", "postgres"),
            ("user", "postgres"),
        ],
    ),
    (
        "template-mongo-atlas",
        "mongodb",
        &[("uri", "mongodb+srv://cluster0.example.mongodb.net")],
    ),
    (
        "template-redis-local",
        "redis",
        &[("host", "localhost"), ("port", "6379"), ("database", "0")],
    ),
    ("template-sqlite-file", "sqlite", &[("file", "data.db")]),
];

#[derive(Clone, Debug, PartialEq)]
pub enum TestState {
    Idle,
    Running,
    Ok(String),
    Failed(String),
}

pub struct DataSourceForm {
    editing: Option<DataSourceId>,
    drivers: Vec<DriverDescriptor>,
    statuses: HashMap<DriverId, DriverStatus>,
    search: Entity<InputState>,
    pub driver: Option<DriverId>,
    name: Entity<InputState>,
    folder: Entity<InputState>,
    pub environment: Environment,
    pub read_only: bool,
    pub tx_mode: TxMode,
    pub auth: AuthMethod,
    /// Property inputs of the selected driver, by key.
    fields: Vec<(ferrodb_api::DriverProperty, Entity<InputState>)>,
    ssh_enabled: bool,
    ssh: [Entity<InputState>; 3],
    ssl_mode: usize,
    options: Entity<InputState>,
    schemas: Entity<InputState>,
    pub tab: usize,
    pub test: TestState,
    _subs: Vec<Subscription>,
    _events: Task<()>,
}

const SSL_MODES: [&str; 4] = ["disable", "prefer", "require", "verify-full"];

impl DataSourceForm {
    pub fn new(
        editing: Option<DataSourceConfig>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let backend = AppState::backend(cx);
        let drivers = backend.drivers().list();
        let statuses = drivers
            .iter()
            .map(|d| (d.id.clone(), backend.drivers().status(&d.id)))
            .collect();
        let input = |ph: String, window: &mut Window, cx: &mut Context<Self>| {
            cx.new(|cx| InputState::new(window, cx).placeholder(ph))
        };
        let search = input(t("ds-search-drivers"), window, cx);
        let name = input(t("ds-name"), window, cx);
        let folder = input(t("ds-folder-placeholder"), window, cx);
        let ssh = [
            input("ssh.example.com:22".into(), window, cx),
            input(t("ds-user"), window, cx),
            input(t("ds-ssh-key"), window, cx),
        ];
        let options = input(
            "connectTimeout=10; applicationName=FerroDb".into(),
            window,
            cx,
        );
        let schemas = input("public, sales_*".into(), window, cx);
        let rx = backend.drivers().subscribe();
        let events = cx.spawn(async move |this, cx| {
            while let Ok(ev) = rx.recv().await {
                let ok = this.update(cx, |this, cx| {
                    if let DriverEvent::StatusChanged(id, st) = ev {
                        this.statuses.insert(id, st);
                        cx.notify();
                    }
                });
                if ok.is_err() {
                    break;
                }
            }
        });
        let subs = vec![cx.observe(&search, |_, _, cx| cx.notify())];
        let mut this = Self {
            editing: None,
            drivers,
            statuses,
            search,
            driver: None,
            name,
            folder,
            environment: Environment::None,
            read_only: false,
            tx_mode: TxMode::Auto,
            auth: AuthMethod::Password,
            fields: Vec::new(),
            ssh_enabled: false,
            ssh,
            ssl_mode: 1,
            options,
            schemas,
            tab: 0,
            test: TestState::Idle,
            _subs: subs,
            _events: events,
        };
        if let Some(c) = editing {
            this.load(c, window, cx);
        }
        this
    }

    fn load(&mut self, c: DataSourceConfig, window: &mut Window, cx: &mut Context<Self>) {
        self.select_driver(c.driver.clone(), window, cx);
        self.editing = Some(c.id.clone());
        self.environment = c.environment;
        self.read_only = c.read_only;
        self.tx_mode = c.tx_mode;
        self.name
            .update(cx, |i, cx| i.set_value(c.name.clone(), window, cx));
        let folder = c.folder.join("/");
        self.folder
            .update(cx, |i, cx| i.set_value(folder, window, cx));
        for (k, v) in &c.properties {
            if let Some((_, input)) = self.fields.iter().find(|(p, _)| &p.key == k) {
                input.update(cx, |i, cx| i.set_value(v.clone(), window, cx));
            }
            if k == "auth"
                && let Some(a) = self.descriptor().and_then(|d| {
                    d.capabilities
                        .auth_methods
                        .iter()
                        .find(|m| format!("{m:?}") == *v)
                        .copied()
                })
            {
                self.auth = a;
            }
        }
    }

    pub fn drivers(&self) -> &[DriverDescriptor] {
        &self.drivers
    }

    /// Drivers matching the search field (name or id).
    pub fn matching_drivers(&self, cx: &App) -> Vec<&DriverDescriptor> {
        let q = self.search.read(cx).value().trim().to_lowercase();
        self.drivers
            .iter()
            .filter(|d| {
                q.is_empty() || d.name.to_lowercase().contains(&q) || d.id.as_str().contains(&q)
            })
            .collect()
    }

    pub fn set_search(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let text = text.to_owned();
        self.search
            .update(cx, |i, cx| i.set_value(text, window, cx));
        cx.notify();
    }

    pub fn set_name(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let text = text.to_owned();
        self.name.update(cx, |i, cx| i.set_value(text, window, cx));
    }

    pub fn descriptor(&self) -> Option<&DriverDescriptor> {
        let id = self.driver.as_ref()?;
        self.drivers.iter().find(|d| &d.id == id)
    }

    pub fn status(&self) -> Option<&DriverStatus> {
        self.statuses.get(self.driver.as_ref()?)
    }

    /// Selects a driver and rebuilds the generated form.
    pub fn select_driver(&mut self, id: DriverId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(d) = self.drivers.iter().find(|d| d.id == id).cloned() else {
            return;
        };
        self.driver = Some(id);
        self.auth = d
            .capabilities
            .auth_methods
            .first()
            .copied()
            .unwrap_or(AuthMethod::None);
        self.fields = d
            .properties
            .iter()
            .map(|p| {
                let ph = if p.secret {
                    t("ds-secret-placeholder")
                } else {
                    p.label.clone()
                };
                let default = p.default.clone().unwrap_or_default();
                let input = cx.new(|cx| {
                    let i = InputState::new(window, cx)
                        .placeholder(ph)
                        .default_value(default);
                    if p.secret { i.masked(true) } else { i }
                });
                (p.clone(), input)
            })
            .collect();
        if self.name.read(cx).value().is_empty() || self.editing.is_none() {
            let name = d.name.clone();
            self.name.update(cx, |i, cx| i.set_value(name, window, cx));
        }
        self.test = TestState::Idle;
        cx.notify();
    }

    pub fn apply_template(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let (label, driver, props) = TEMPLATES[ix];
        self.select_driver(DriverId::new(driver), window, cx);
        for (k, v) in props {
            if let Some((_, input)) = self.fields.iter().find(|(p, _)| p.key == *k) {
                input.update(cx, |i, cx| i.set_value(v.to_string(), window, cx));
            }
        }
        self.name
            .update(cx, |i, cx| i.set_value(t(label), window, cx));
    }

    /// Sets a property input (tests, templates).
    pub fn set_property(
        &mut self,
        key: &str,
        value: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some((_, input)) = self.fields.iter().find(|(p, _)| p.key == key) {
            input.update(cx, |i, cx| i.set_value(value.to_owned(), window, cx));
        }
    }

    fn visible_fields(
        &self,
    ) -> impl Iterator<Item = &(ferrodb_api::DriverProperty, Entity<InputState>)> {
        self.fields
            .iter()
            .filter(|(p, _)| p.auth.is_empty() || p.auth.contains(&self.auth))
    }

    /// The configuration described by the form. Secrets are not part of it:
    /// they go to the keyring and are referenced as `secret://<id>`.
    pub fn config(&self, cx: &App) -> Option<DataSourceConfig> {
        let d = self.descriptor()?;
        let name = self.name.read(cx).value().trim().to_string();
        let name = if name.is_empty() {
            d.name.clone()
        } else {
            name
        };
        let id = self.editing.clone().unwrap_or_else(|| new_id(&name, cx));
        let mut properties: Vec<(String, String)> = self
            .visible_fields()
            .filter(|(p, _)| !p.secret)
            .map(|(p, i)| (p.key.clone(), i.read(cx).value().to_string()))
            .filter(|(_, v)| !v.is_empty())
            .collect();
        properties.push(("auth".into(), format!("{:?}", self.auth)));
        if self.ssh_enabled {
            let [host, user, key] = &self.ssh;
            properties.push(("ssh.host".into(), host.read(cx).value().to_string()));
            properties.push(("ssh.user".into(), user.read(cx).value().to_string()));
            properties.push(("ssh.key".into(), key.read(cx).value().to_string()));
        }
        properties.push(("ssl.mode".into(), SSL_MODES[self.ssl_mode].into()));
        for opt in self.options.read(cx).value().split(';') {
            if let Some((k, v)) = opt.split_once('=') {
                properties.push((format!("option.{}", k.trim()), v.trim().to_owned()));
            }
        }
        let schemas = self.schemas.read(cx).value().trim().to_string();
        if !schemas.is_empty() {
            properties.push(("schema.filter".into(), schemas));
        }
        let folder = self
            .folder
            .read(cx)
            .value()
            .split('/')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect();
        Some(DataSourceConfig {
            secret_ref: Some(format!("secret://{id}")),
            id,
            name,
            driver: d.id.clone(),
            folder,
            environment: self.environment,
            read_only: self.read_only,
            tx_mode: self.tx_mode,
            properties,
        })
    }

    /// Secret fields typed into the form (empty ones keep the stored value).
    pub fn secrets(&self, cx: &App) -> SecretValues {
        SecretValues(
            self.visible_fields()
                .filter(|(p, _)| p.secret)
                .map(|(p, i)| (p.key.clone(), i.read(cx).value().to_string()))
                .filter(|(_, v)| !v.is_empty())
                .collect(),
        )
    }

    /// Connection URL preview from the driver's template.
    pub fn url_preview(&self, cx: &App) -> Option<String> {
        let mut url = self.descriptor()?.url_template.clone()?;
        for (p, i) in &self.fields {
            if !p.secret {
                url = url.replace(&format!("{{{}}}", p.key), &i.read(cx).value());
            }
        }
        Some(url)
    }

    pub fn test_connection(&mut self, cx: &mut Context<Self>) {
        let Some(config) = self.config(cx) else {
            return;
        };
        self.test = TestState::Running;
        cx.notify();
        let secrets = self.secrets(cx);
        let backend = AppState::backend(cx);
        let task = cx
            .background_executor()
            .spawn(async move { backend.data_sources().test(config, secrets).await });
        cx.spawn(async move |this, cx| {
            let r = task.await;
            let _ = this.update(cx, |this, cx| {
                this.test = match r {
                    Ok(rep) => TestState::Ok(t_args(
                        "ds-test-ok",
                        &[
                            ("version", rep.server_version.into()),
                            ("ms", rep.latency_ms.into()),
                        ],
                    )),
                    Err(e) => TestState::Failed(e.to_string()),
                };
                cx.notify();
            });
        })
        .detach();
    }

    pub fn download_driver(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.driver.clone() else {
            return;
        };
        download(id, None, cx);
    }

    /// Saves the data source; closes the dialog on success.
    pub fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(config) = self.config(cx) else {
            window.push_notification(Notification::warning(t("ds-choose-driver")), cx);
            return;
        };
        let missing: Vec<String> = self
            .visible_fields()
            .filter(|(p, i)| p.required && !p.secret && i.read(cx).value().trim().is_empty())
            .map(|(p, _)| p.label.clone())
            .collect();
        if !missing.is_empty() {
            window.push_notification(
                Notification::warning(t_args(
                    "ds-required",
                    &[("fields", missing.join(", ").into())],
                )),
                cx,
            );
            return;
        }
        let backend = AppState::backend(cx);
        let name = config.name.clone();
        let secrets = self.secrets(cx);
        let task = cx
            .background_executor()
            .spawn(async move { backend.data_sources().save(config, secrets).await });
        cx.spawn_in(window, async move |_, cx| {
            let r = task.await;
            let _ = cx.update(|window, cx| match r {
                Ok(()) => {
                    window.close_dialog(cx);
                    output::info(t_args("ds-saved", &[("name", name.clone().into())]), cx);
                    window.push_notification(
                        Notification::success(t_args("ds-saved", &[("name", name.into())])),
                        cx,
                    );
                }
                Err(e) => window.push_notification(Notification::error(e.to_string()), cx),
            });
        })
        .detach();
    }

    fn render_driver_list(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let q = self.search.read(cx).value().trim().to_lowercase();
        let matching: Vec<DriverDescriptor> =
            self.matching_drivers(cx).into_iter().cloned().collect();
        let theme = cx.theme().clone();
        let mut list = v_flex().gap_0p5();
        if q.is_empty() && self.editing.is_none() {
            list = list.child(group_header(&t("ds-templates"), &theme));
            for (ix, (label, driver, _)) in TEMPLATES.iter().enumerate() {
                let model = self
                    .drivers
                    .iter()
                    .find(|d| d.id.as_str() == *driver)
                    .map(|d| d.capabilities.data_model);
                list = list.child(
                    ListItem::new(("ds-template", ix))
                        .child(
                            h_flex()
                                .gap_1p5()
                                .when_some(model, |d, m| {
                                    d.child(
                                        icons::data_source_icon(m)
                                            .xsmall()
                                            .text_color(theme.muted_foreground),
                                    )
                                })
                                .child(t(label)),
                        )
                        .on_click(
                            cx.listener(move |this, _, w, cx| this.apply_template(ix, w, cx)),
                        ),
                );
            }
        }
        for (cat, key) in [
            (DriverCategory::Relational, "ds-category-relational"),
            (DriverCategory::AnalyticalCloud, "ds-category-analytical"),
            (DriverCategory::NoSql, "ds-category-nosql"),
        ] {
            let drivers: Vec<&DriverDescriptor> =
                matching.iter().filter(|d| d.category == cat).collect();
            if drivers.is_empty() {
                continue;
            }
            list = list.child(group_header(&t(key), &theme));
            for d in drivers {
                let id = d.id.clone();
                let selected = self.driver.as_ref() == Some(&d.id);
                let ready = self.statuses.get(&d.id).is_some_and(DriverStatus::is_ready);
                list =
                    list.child(
                        ListItem::new(SharedString::from(format!("ds-driver-{}", d.id)))
                            .selected(selected)
                            .child(
                                h_flex()
                                    .gap_1p5()
                                    .child(
                                        icons::data_source_icon(d.capabilities.data_model)
                                            .xsmall()
                                            .text_color(theme.muted_foreground),
                                    )
                                    .child(d.name.clone())
                                    .when(!ready, |b| {
                                        b.child(
                                            icons::Icon::from(icons::Lucide::Download)
                                                .xsmall()
                                                .text_color(theme.muted_foreground),
                                        )
                                    }),
                            )
                            .on_click(cx.listener(move |this, _, w, cx| {
                                this.select_driver(id.clone(), w, cx)
                            })),
                    );
            }
        }
        v_flex()
            .w(px(230.))
            .h_full()
            .gap_1()
            .pr_2()
            .border_r_1()
            .border_color(theme.border)
            .child(
                Input::new(&self.search)
                    .xsmall()
                    .prefix(icons::Icon::from(icons::Lucide::Search).xsmall()),
            )
            .child(
                div()
                    .id("ds-driver-list")
                    .flex_1()
                    .overflow_y_scroll()
                    .child(list),
            )
    }

    fn render_status(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let d = self.descriptor()?;
        let st = self.status()?.clone();
        let theme = cx.theme();
        let label = status_text(&st);
        let needs = !st.is_ready() && !matches!(st, DriverStatus::Downloading { .. });
        Some(
            h_flex()
                .id("ds-driver-status")
                .test_support()
                .gap_2()
                .p_1()
                .rounded_md()
                .bg(if st.is_ready() {
                    theme.muted
                } else {
                    theme.warning.opacity(0.15)
                })
                .text_sm()
                .child(format!("{} · {:?}", d.family, d.access()))
                .child(div().flex_1().child(label))
                .when(needs, |row| {
                    row.child(
                        Button::new("ds-download")
                            .xsmall()
                            .primary()
                            .icon(icons::Lucide::Download)
                            .label(t("drivers-download"))
                            .on_click(cx.listener(|this, _, _, cx| this.download_driver(cx))),
                    )
                })
                .into_any_element(),
        )
    }

    fn render_general(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let d = self.descriptor().cloned();
        let theme = cx.theme().clone();
        let mut col = v_flex().gap_2();
        col = col.child(field(&t("ds-name"), Input::new(&self.name).small()));
        col = col.child(field(&t("ds-folder"), Input::new(&self.folder).small()));
        if let Some(d) = &d
            && d.capabilities.auth_methods.len() > 1
        {
            let methods = d.capabilities.auth_methods.clone();
            let current = methods.iter().position(|m| *m == self.auth).unwrap_or(0);
            col = col.child(field(
                &t("ds-auth"),
                ButtonGroup::new("ds-auth")
                    .xsmall()
                    .children(methods.iter().enumerate().map(|(i, m)| {
                        Button::new(("ds-auth-m", i))
                            .label(t(&format!("auth-{}", format!("{m:?}").to_lowercase())))
                            .selected(i == current)
                    }))
                    .on_click(cx.listener(move |this, sel: &Vec<usize>, _, cx| {
                        if let Some(i) = sel.first() {
                            this.auth = methods[*i];
                            cx.notify();
                        }
                    })),
            ));
        }
        for (p, input) in self.visible_fields() {
            let label = if p.required {
                format!("{} *", p.label)
            } else {
                p.label.clone()
            };
            col = col.child(field(
                &label,
                Input::new(input)
                    .small()
                    .when(p.secret, |i| i.mask_toggle()),
            ));
        }
        if let Some(url) = self.url_preview(cx) {
            col = col.child(field(
                "URL",
                div()
                    .text_sm()
                    .font_family(theme.mono_font_family.clone())
                    .text_color(theme.muted_foreground)
                    .child(url),
            ));
        }
        let envs = [
            Environment::None,
            Environment::Development,
            Environment::Test,
            Environment::Staging,
            Environment::Production,
        ];
        let env_ix = envs
            .iter()
            .position(|e| *e == self.environment)
            .unwrap_or(0);
        col = col.child(field(
            &t("ds-environment"),
            ButtonGroup::new("ds-env")
                .xsmall()
                .children(envs.iter().enumerate().map(|(i, e)| {
                    let color = environment_color(*e, cx);
                    Button::new(("ds-env-b", i))
                        .label(t(&format!("env-{}", format!("{e:?}").to_lowercase())))
                        .selected(i == env_ix)
                        .when_some(color, |b, c| {
                            b.child(div().size(px(8.)).rounded_full().bg(c))
                        })
                }))
                .on_click(cx.listener(move |this, sel: &Vec<usize>, _, cx| {
                    if let Some(i) = sel.first() {
                        this.environment = envs[*i];
                        if this.environment == Environment::Production {
                            this.read_only = true;
                        }
                        cx.notify();
                    }
                })),
        ));
        col.child(
            h_flex()
                .gap_4()
                .child(
                    Checkbox::new("ds-read-only")
                        .label(t("ds-read-only"))
                        .checked(self.read_only)
                        .on_click(cx.listener(|this, v: &bool, _, cx| {
                            this.read_only = *v;
                            cx.notify();
                        })),
                )
                .child(
                    Checkbox::new("ds-manual-tx")
                        .label(t("ds-manual-tx"))
                        .checked(self.tx_mode == TxMode::Manual)
                        .on_click(cx.listener(|this, v: &bool, _, cx| {
                            this.tx_mode = if *v { TxMode::Manual } else { TxMode::Auto };
                            cx.notify();
                        })),
                ),
        )
    }

    fn render_tab(&self, cx: &mut Context<Self>) -> AnyElement {
        match self.tab {
            0 => self.render_general(cx).into_any_element(),
            1 => {
                let [host, user, key] = &self.ssh;
                v_flex()
                    .gap_2()
                    .child(
                        Checkbox::new("ds-ssh")
                            .label(t("ds-ssh-enable"))
                            .checked(self.ssh_enabled)
                            .on_click(cx.listener(|this, v: &bool, _, cx| {
                                this.ssh_enabled = *v;
                                cx.notify();
                            })),
                    )
                    .child(field(
                        &t("ds-ssh-host"),
                        Input::new(host).small().disabled(!self.ssh_enabled),
                    ))
                    .child(field(
                        &t("ds-user"),
                        Input::new(user).small().disabled(!self.ssh_enabled),
                    ))
                    .child(field(
                        &t("ds-ssh-key"),
                        Input::new(key).small().disabled(!self.ssh_enabled),
                    ))
                    .into_any_element()
            }
            2 => field(
                &t("ds-ssl-mode"),
                ButtonGroup::new("ds-ssl")
                    .xsmall()
                    .children(SSL_MODES.iter().enumerate().map(|(i, m)| {
                        Button::new(("ds-ssl-m", i))
                            .label(*m)
                            .selected(i == self.ssl_mode)
                    }))
                    .on_click(cx.listener(|this, sel: &Vec<usize>, _, cx| {
                        if let Some(i) = sel.first() {
                            this.ssl_mode = *i;
                            cx.notify();
                        }
                    })),
            )
            .into_any_element(),
            3 => field(&t("ds-options"), Input::new(&self.options).small()).into_any_element(),
            _ => v_flex()
                .gap_1()
                .child(field(
                    &t("ds-schema-filter"),
                    Input::new(&self.schemas).small(),
                ))
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(t("ds-schema-filter-hint")),
                )
                .into_any_element(),
        }
    }
}

fn group_header(label: &str, theme: &ferrodb_ui_kit::theme::Theme) -> impl IntoElement {
    div()
        .pt_2()
        .pb_0p5()
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme.muted_foreground)
        .child(label.to_uppercase())
}

fn field(label: &str, input: impl IntoElement) -> impl IntoElement {
    h_flex()
        .gap_2()
        .child(
            div()
                .w(px(130.))
                .flex_shrink_0()
                .text_sm()
                .child(label.to_owned()),
        )
        .child(div().flex_1().child(input))
}

/// Human-readable driver status.
pub fn status_text(st: &DriverStatus) -> String {
    match st {
        DriverStatus::BuiltIn => t("drivers-builtin"),
        DriverStatus::NotDownloaded => t("drivers-not-downloaded"),
        DriverStatus::Downloading { progress_permille } => t_args(
            "drivers-downloading",
            &[("percent", (progress_permille / 10).into())],
        ),
        DriverStatus::Downloaded { version } => {
            t_args("drivers-downloaded", &[("version", version.clone().into())])
        }
        DriverStatus::UpdateAvailable { installed, latest } => t_args(
            "drivers-update",
            &[
                ("installed", installed.clone().into()),
                ("latest", latest.clone().into()),
            ],
        ),
        DriverStatus::Custom { files } => t_args("drivers-custom", &[("count", (*files).into())]),
        DriverStatus::Failed(e) => e.clone(),
    }
}

/// Starts a driver download in the background (progress arrives as events).
pub fn download(id: DriverId, version: Option<String>, cx: &mut App) {
    let backend = AppState::backend(cx);
    let task = cx
        .background_executor()
        .spawn(async move { backend.drivers().download(&id, version).await.map(|_| id) });
    cx.spawn(async move |cx| {
        let r = task.await;
        cx.update(|cx| match r {
            Ok(id) => output::info(
                t_args("drivers-download-done", &[("id", id.to_string().into())]),
                cx,
            ),
            Err(e) => output::log(output::Level::Error, e.to_string(), cx),
        });
    })
    .detach();
}

/// A unique id derived from the name (`shop-dev`, `shop-dev-2`…).
fn new_id(name: &str, cx: &App) -> DataSourceId {
    let base: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let base = if base.is_empty() {
        "data-source".to_owned()
    } else {
        base
    };
    let existing: Vec<DataSourceId> = AppState::backend(cx)
        .data_sources()
        .list()
        .into_iter()
        .map(|s| s.config.id)
        .collect();
    let mut id = DataSourceId::new(base.clone());
    let mut n = 2;
    while existing.contains(&id) {
        id = DataSourceId::new(format!("{base}-{n}"));
        n += 1;
    }
    id
}

impl Render for DataSourceForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let has_driver = self.driver.is_some();
        let status = self.render_status(cx);
        let test = match &self.test {
            TestState::Idle => None,
            TestState::Running => Some((t("ds-testing"), cx.theme().muted_foreground)),
            TestState::Ok(m) => Some((m.clone(), cx.theme().success)),
            TestState::Failed(e) => Some((e.clone(), cx.theme().danger)),
        };
        let body = if has_driver {
            v_flex()
                .flex_1()
                .gap_2()
                .min_w_0()
                .children(status)
                .child(
                    TabBar::new("ds-tabs")
                        .xsmall()
                        .selected_index(self.tab)
                        .child(Tab::new().label(t("ds-tab-general")))
                        .child(Tab::new().label("SSH"))
                        .child(Tab::new().label("SSL"))
                        .child(Tab::new().label(t("ds-tab-options")))
                        .child(Tab::new().label(t("ds-tab-schemas")))
                        .on_click(cx.listener(|this, ix: &usize, _, cx| {
                            this.tab = *ix;
                            cx.notify();
                        })),
                )
                .child(
                    div()
                        .id("ds-form")
                        .flex_1()
                        .overflow_y_scroll()
                        .child(self.render_tab(cx)),
                )
                .when_some(test, |d, (msg, color)| {
                    d.child(
                        div()
                            .id("ds-test-result")
                            .test_support()
                            .text_sm()
                            .text_color(color)
                            .child(msg),
                    )
                })
                .into_any_element()
        } else {
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_color(cx.theme().muted_foreground)
                .child(t("ds-choose-driver"))
                .into_any_element()
        };
        h_flex()
            .id("data-source-form")
            .test_support()
            .h(px(480.))
            .gap_2()
            .child(self.render_driver_list(cx))
            .child(body)
    }
}

fn open(form: Entity<DataSourceForm>, title: String, window: &mut Window, cx: &mut App) {
    cx.set_global(LastForm(Some(form.clone())));
    window.open_dialog(cx, move |dialog, _, cx| {
        let ready = form.read(cx).driver.is_some();
        let testing = form.read(cx).test == TestState::Running;
        let (f1, f2) = (form.clone(), form.clone());
        dialog
            .title(title.clone())
            .w(px(860.))
            .child(form.clone())
            .footer(
                DialogFooter::new()
                    .child(
                        Button::new("ds-test")
                            .label(t("ds-test"))
                            .loading(testing)
                            .disabled(!ready)
                            .on_click(move |_, _, cx| f1.update(cx, |f, cx| f.test_connection(cx))),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("ds-cancel")
                            .label(t("button-cancel"))
                            .on_click(|_, w, cx| w.close_dialog(cx)),
                    )
                    .child(
                        Button::new("ds-ok")
                            .primary()
                            .label(t("button-ok"))
                            .disabled(!ready)
                            .on_click(move |_, w, cx| f2.update(cx, |f, cx| f.save(w, cx))),
                    ),
            )
    });
}

/// The form of the most recently opened dialog (UI tests).
#[derive(Default)]
pub struct LastForm(pub Option<Entity<DataSourceForm>>);
impl Global for LastForm {}

pub fn open_new(window: &mut Window, cx: &mut App) {
    let form = cx.new(|cx| DataSourceForm::new(None, window, cx));
    open(form, t("action-new-data-source"), window, cx);
}

pub fn open_edit(id: DataSourceId, window: &mut Window, cx: &mut App) {
    let Some(info) = AppState::backend(cx).data_sources().get(&id) else {
        return;
    };
    let form = cx.new(|cx| DataSourceForm::new(Some(info.config.clone()), window, cx));
    open(
        form,
        t_args("ds-edit-title", &[("name", info.config.name.into())]),
        window,
        cx,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn statuses_have_text() {
        assert!(!status_text(&DriverStatus::NotDownloaded).is_empty());
        assert!(
            status_text(&DriverStatus::Downloading {
                progress_permille: 500
            })
            .contains("50")
        );
    }
}

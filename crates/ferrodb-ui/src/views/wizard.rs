//! Export and import wizards (plan 2.8, phase 6): step-by-step dialogs on
//! [`ferrodb_api::TransferService`].
//!
//! - Export: the shown result of the last console (its statement runs again
//!   on its own connection and streams to the file) or the table /
//!   collection selected in the explorer → format → file and options →
//!   summary.
//! - Import: file → format and target table (create / truncate) → preview
//!   of the first rows and the column matching by name.

use std::path::PathBuf;

use ferrodb_api::{
    DataFormat, DataSourceId, ExtScriptInfo, ExtScriptKind, FilePreview, FormatOptions, NodeRef,
    ObjectKind, TransferEvent, TransferRequest, TransferSource, TransferTarget,
};
use ferrodb_ui_kit::{
    gpui::{prelude::*, *},
    kit::{
        ActiveTheme, Disableable as _, Selectable as _, Sizable as _, WindowExt as _,
        button::{Button, ButtonGroup, ButtonVariants as _},
        checkbox::Checkbox,
        dialog::DialogFooter,
        h_flex,
        input::{Input, InputState},
        notification::Notification,
        v_flex,
    },
};

use crate::{
    AppState,
    i18n::{t, t_args},
    output, workspace,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WizardKind {
    Export,
    Import,
}

/// What an export reads.
#[derive(Clone, Debug, PartialEq)]
pub enum ExportSource {
    /// A console result: its statement runs again.
    Result {
        data_source: DataSourceId,
        statement: String,
        title: String,
    },
    /// A whole table / collection.
    Object(NodeRef),
}

/// Where an import writes.
#[derive(Clone, Debug, PartialEq)]
pub struct ImportTarget {
    pub data_source: DataSourceId,
    pub container: Option<String>,
    pub table: Option<String>,
}

/// Formats offered by a wizard.
pub fn formats(kind: WizardKind) -> Vec<DataFormat> {
    DataFormat::ALL
        .into_iter()
        .filter(|f| kind == WizardKind::Export || f.importable())
        .collect()
}

fn home() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// `~/x` → the home directory.
pub fn expand_home(path: &str) -> PathBuf {
    match path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\")) {
        Some(rest) => home().join(rest),
        None => PathBuf::from(path),
    }
}

fn is_data_object(kind: Option<ObjectKind>) -> bool {
    matches!(
        kind,
        Some(
            ObjectKind::Table
                | ObjectKind::View
                | ObjectKind::MaterializedView
                | ObjectKind::Collection
        )
    )
}

/// The export source from the context: the last console's shown result,
/// else the explorer's selected table / collection.
pub fn export_source(cx: &App) -> Option<ExportSource> {
    if let Some(m) = workspace::active_result(cx) {
        let m = m.read(cx);
        if !m.statement.trim().is_empty() {
            return Some(ExportSource::Result {
                data_source: m.data_source.clone(),
                statement: m.statement.clone(),
                title: m.title.clone(),
            });
        }
    }
    workspace::explorer_selection(cx)
        .filter(|n| is_data_object(n.kind()))
        .map(ExportSource::Object)
}

/// The import target from the context: the explorer's selection, else the
/// last console's data source.
pub fn import_target(cx: &App) -> Option<ImportTarget> {
    if let Some(n) = workspace::explorer_selection(cx) {
        return Some(ImportTarget {
            data_source: n.data_source.clone(),
            container: n.path.first().map(|(_, c)| c.clone()),
            table: is_data_object(n.kind()).then(|| n.name().unwrap_or_default().to_owned()),
        });
    }
    workspace::active_console_source(cx).map(|ds| ImportTarget {
        data_source: ds,
        container: None,
        table: None,
    })
}

pub struct Wizard {
    pub kind: WizardKind,
    pub step: usize,
    pub format: DataFormat,
    pub header: bool,
    pub create: bool,
    pub truncate: bool,
    pub source: Option<ExportSource>,
    pub target_to: Option<ImportTarget>,
    /// Import: the file's first rows (or why they could not be read).
    pub preview: Option<Result<FilePreview, String>>,
    /// Import: columns of the existing target table.
    pub target_columns: Vec<String>,
    /// Export: extractor scripts offered after the built-in formats.
    pub scripts: Vec<ExtScriptInfo>,
    /// Export: the chosen extractor (`format == DataFormat::Script`).
    pub script: Option<String>,
    path: Entity<InputState>,
    target: Entity<InputState>,
}

/// The most recently opened wizard (UI tests).
#[derive(Default)]
pub struct LastWizard(pub Option<Entity<Wizard>>);
impl Global for LastWizard {}

impl Wizard {
    pub fn new(
        kind: WizardKind,
        source: Option<ExportSource>,
        target_to: Option<ImportTarget>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let format = DataFormat::Csv;
        let stem = match &source {
            Some(ExportSource::Object(n)) => n.name().unwrap_or("data").to_owned(),
            Some(ExportSource::Result { title, .. }) => {
                let s: String = title
                    .chars()
                    .map(|c| if c.is_alphanumeric() { c } else { '_' })
                    .collect();
                if s.trim_matches('_').is_empty() {
                    "result".into()
                } else {
                    s
                }
            }
            None => "data".into(),
        };
        let default_path = match kind {
            WizardKind::Export => home()
                .join("export")
                .join(format!("{stem}.{}", format.extension())),
            WizardKind::Import => home().join("data").join("import.csv"),
        };
        let path = cx.new(|cx| {
            InputState::new(window, cx).default_value(default_path.to_string_lossy().into_owned())
        });
        let table = target_to
            .as_ref()
            .and_then(|t| t.table.clone())
            .unwrap_or_default();
        let target = cx.new(|cx| InputState::new(window, cx).default_value(table));
        let scripts = match kind {
            WizardKind::Export => crate::scripts::list(ExtScriptKind::Extractor, cx),
            WizardKind::Import => Vec::new(),
        };
        Self {
            kind,
            step: 0,
            format,
            header: true,
            create: true,
            truncate: false,
            source,
            target_to,
            preview: None,
            target_columns: Vec::new(),
            scripts,
            script: None,
            path,
            target,
        }
    }

    pub fn steps(&self) -> [String; 3] {
        match self.kind {
            WizardKind::Export => [t("wizard-format"), t("wizard-options"), t("wizard-preview")],
            WizardKind::Import => [
                t("wizard-source-file"),
                t("wizard-mapping"),
                t("wizard-preview"),
            ],
        }
    }

    pub fn path(&self, cx: &App) -> PathBuf {
        expand_home(self.path.read(cx).value().trim())
    }

    fn options(&self) -> FormatOptions {
        FormatOptions {
            header: self.header,
            script: self
                .script
                .clone()
                .filter(|_| self.format == DataFormat::Script),
            ..Default::default()
        }
    }

    /// Chooses an extractor script as the export format; the path takes the
    /// script's extension.
    pub fn set_script(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(info) = self.scripts.iter().find(|s| s.id == id).cloned() else {
            return;
        };
        self.format = DataFormat::Script;
        self.script = Some(info.id);
        let ext = if info.extension.trim().is_empty() {
            DataFormat::Script.extension().to_owned()
        } else {
            info.extension.trim().trim_start_matches('.').to_owned()
        };
        let p = self.path(cx).with_extension(ext);
        self.path.update(cx, |i, cx| {
            i.set_value(p.to_string_lossy().into_owned(), window, cx)
        });
        cx.notify();
    }

    /// Label of the chosen format (the script's name for an extractor).
    pub fn format_label(&self) -> String {
        match (&self.script, self.format) {
            (Some(id), DataFormat::Script) => self
                .scripts
                .iter()
                .find(|s| &s.id == id)
                .map(|s| s.name.clone())
                .unwrap_or_else(|| id.clone()),
            _ => self.format.label().to_owned(),
        }
    }

    /// Chooses a format; an export path follows its extension, an import
    /// format follows the file name.
    pub fn set_format(&mut self, f: DataFormat, window: &mut Window, cx: &mut Context<Self>) {
        self.format = f;
        self.script = None;
        if self.kind == WizardKind::Export {
            let p = self.path(cx).with_extension(f.extension());
            self.path.update(cx, |i, cx| {
                i.set_value(p.to_string_lossy().into_owned(), window, cx)
            });
        }
        cx.notify();
    }

    /// Moves to the next step; entering the import preview reads the file.
    pub fn next(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.kind == WizardKind::Import && self.step == 0 {
            if let Some(f) = DataFormat::from_path(&self.path(cx)).filter(|f| f.importable()) {
                self.format = f;
            }
            if self.target.read(cx).value().trim().is_empty() {
                let stem = self
                    .path(cx)
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                self.target
                    .update(cx, |i, cx| i.set_value(stem, window, cx));
            }
        }
        self.step = (self.step + 1).min(2);
        if self.kind == WizardKind::Import && self.step == 2 {
            self.load_preview(cx);
        }
        cx.notify();
    }

    fn load_preview(&mut self, cx: &mut Context<Self>) {
        self.preview = None;
        let backend = AppState::backend(cx);
        let (path, format, opts) = (self.path(cx), self.format, self.options());
        let target = self.target_to.clone();
        let name = self.target.read(cx).value().trim().to_owned();
        let task = cx.background_executor().spawn(async move {
            let preview = backend
                .transfers()
                .preview_file(path, format, opts, 5)
                .await
                .map_err(|e| e.to_string());
            let cols = match target {
                Some(t) => backend
                    .transfers()
                    .target_columns(t.data_source, t.container, name)
                    .await
                    .unwrap_or_default(),
                None => Vec::new(),
            };
            (preview, cols)
        });
        cx.spawn(async move |this, cx| {
            let (preview, cols) = task.await;
            let _ = this.update(cx, |w, cx| {
                w.preview = Some(preview);
                w.target_columns = cols;
                cx.notify();
            });
        })
        .detach();
    }

    /// Text of the last step.
    pub fn summary(&self, cx: &App) -> String {
        match self.kind {
            WizardKind::Export => {
                let src = match &self.source {
                    Some(ExportSource::Result { statement, .. }) => statement.trim().to_owned(),
                    Some(ExportSource::Object(n)) => n
                        .path
                        .iter()
                        .map(|(_, p)| p.as_str())
                        .collect::<Vec<_>>()
                        .join("."),
                    None => t("wizard-no-source"),
                };
                format!(
                    "{}: {src}\n{}: {} ({})",
                    t("wizard-source"),
                    t("wizard-file"),
                    self.path(cx).display(),
                    self.format_label()
                )
            }
            WizardKind::Import => match &self.preview {
                None => t("wizard-running"),
                Some(Err(e)) => e.clone(),
                Some(Ok(p)) => {
                    let mut s = String::new();
                    let head: Vec<String> = p
                        .columns
                        .iter()
                        .zip(&p.types)
                        .map(|(c, ty)| format!("{c} ({ty})"))
                        .collect();
                    s.push_str(&head.join(" | "));
                    for r in &p.rows {
                        s.push('\n');
                        let cells: Vec<String> = r
                            .iter()
                            .map(|c| c.clone().unwrap_or_else(|| "NULL".into()))
                            .collect();
                        s.push_str(&cells.join(" | "));
                    }
                    s.push_str("\n\n");
                    s.push_str(&t("wizard-mapping-auto"));
                    for c in &p.columns {
                        let to = if self.target_columns.is_empty() {
                            t("wizard-mapping-new")
                        } else {
                            self.target_columns
                                .iter()
                                .find(|x| x.eq_ignore_ascii_case(c))
                                .cloned()
                                .unwrap_or_else(|| t("wizard-mapping-skipped"))
                        };
                        s.push_str(&format!("\n  {c} → {to}"));
                    }
                    s
                }
            },
        }
    }

    /// The transfer the wizard describes, if complete.
    pub fn request(&self, cx: &App) -> Option<TransferRequest> {
        let path = self.path(cx);
        match self.kind {
            WizardKind::Export => {
                let source = match self.source.clone()? {
                    ExportSource::Result {
                        data_source,
                        statement,
                        ..
                    } => TransferSource::Query {
                        data_source,
                        text: statement,
                        schema: None,
                    },
                    ExportSource::Object(n) => TransferSource::Object(n),
                };
                Some(TransferRequest {
                    source,
                    target: TransferTarget::File {
                        path,
                        format: self.format,
                        options: self.options(),
                    },
                })
            }
            WizardKind::Import => {
                let t = self.target_to.clone()?;
                let name = self.target.read(cx).value().trim().to_owned();
                if name.is_empty() {
                    return None;
                }
                Some(TransferRequest {
                    source: TransferSource::File {
                        path,
                        format: self.format,
                        options: self.options(),
                    },
                    target: TransferTarget::Object {
                        data_source: t.data_source,
                        container: t.container,
                        name,
                        create: self.create,
                        truncate: self.truncate,
                        mapping: Vec::new(),
                    },
                })
            }
        }
    }

    /// Starts the transfer; the outcome is reported as a notification and
    /// in the Output panel.
    pub fn finish(&self, window: &mut Window, cx: &mut App) {
        let Some(req) = self.request(cx) else {
            let msg = match self.kind {
                WizardKind::Export => t("wizard-no-source"),
                WizardKind::Import => t("wizard-no-target"),
            };
            window.push_notification(Notification::warning(msg), cx);
            return;
        };
        let (kind, path) = (self.kind, self.path(cx));
        let target_name = self.target.read(cx).value().trim().to_owned();
        let backend = AppState::backend(cx);
        let handle = backend.transfers().start(req);
        output::info(t("wizard-running"), cx);
        window
            .spawn(cx, async move |cx| {
                while let Ok(ev) = handle.events.recv().await {
                    let done = matches!(ev, TransferEvent::Finished(_) | TransferEvent::Failed(_));
                    let _ = cx.update(|window, cx| match ev {
                        TransferEvent::Progress { .. } => {}
                        TransferEvent::Warning(w) => output::log(output::Level::Warning, w, cx),
                        TransferEvent::Finished(r) => {
                            for s in &r.statements {
                                output::info(s.clone(), cx);
                            }
                            for w in &r.warnings {
                                output::log(output::Level::Warning, w.clone(), cx);
                            }
                            let elapsed = format!("{:.1} s", r.elapsed.as_secs_f64());
                            let msg = match kind {
                                WizardKind::Export => t_args(
                                    "wizard-export-done",
                                    &[
                                        ("items", r.items.into()),
                                        ("path", path.display().to_string().into()),
                                        ("method", r.method.clone().into()),
                                        ("elapsed", elapsed.into()),
                                    ],
                                ),
                                WizardKind::Import => t_args(
                                    "wizard-import-done",
                                    &[
                                        ("items", r.items.into()),
                                        ("target", target_name.clone().into()),
                                        ("method", r.method.clone().into()),
                                        ("elapsed", elapsed.into()),
                                    ],
                                ),
                            };
                            output::info(msg.clone(), cx);
                            window.push_notification(Notification::success(msg), cx);
                        }
                        TransferEvent::Failed(e) => {
                            let msg = t_args("wizard-failed", &[("error", e.into())]);
                            output::log(output::Level::Error, msg.clone(), cx);
                            window.push_notification(Notification::error(msg), cx);
                        }
                    });
                    if done {
                        break;
                    }
                }
            })
            .detach();
    }
}

impl Render for Wizard {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let steps = self.steps();
        let indicator = h_flex()
            .gap_2()
            .children(steps.iter().enumerate().map(|(i, s)| {
                let active = i == self.step;
                h_flex()
                    .gap_1()
                    .child(
                        div()
                            .size(px(20.))
                            .rounded_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_xs()
                            .bg(if i <= self.step {
                                theme.primary
                            } else {
                                theme.muted
                            })
                            .text_color(if i <= self.step {
                                theme.primary_foreground
                            } else {
                                theme.muted_foreground
                            })
                            .child((i + 1).to_string()),
                    )
                    .child(
                        div()
                            .text_sm()
                            .when(active, |d| d.font_weight(FontWeight::SEMIBOLD))
                            .child(s.clone()),
                    )
            }));
        let fmts = formats(self.kind);
        let builtin = fmts.len();
        let format_buttons = ButtonGroup::new("wizard-format")
            .small()
            .children(fmts.iter().enumerate().map(|(i, f)| {
                Button::new(("wizard-f", i))
                    .label(f.label())
                    .selected(*f == self.format)
            }))
            .children(self.scripts.iter().enumerate().map(|(i, s)| {
                Button::new(("wizard-f", builtin + i))
                    .label(s.name.clone())
                    .tooltip(match &s.error {
                        Some(e) => e.clone(),
                        None => s.description.clone(),
                    })
                    .disabled(s.error.is_some())
                    .selected(self.script.as_deref() == Some(s.id.as_str()))
            }))
            .on_click(cx.listener(move |this, sel: &Vec<usize>, window, cx| {
                let Some(&i) = sel.first() else { return };
                if let Some(f) = formats(this.kind).get(i).copied() {
                    this.set_format(f, window, cx);
                } else if let Some(id) = this.scripts.get(i - builtin).map(|s| s.id.clone()) {
                    this.set_script(&id, window, cx);
                }
            }));
        let format_buttons = h_flex().flex_wrap().child(format_buttons);
        let file_and_header = v_flex()
            .gap_2()
            .child(
                h_flex()
                    .gap_2()
                    .child(t("wizard-file"))
                    .child(div().w(px(420.)).child(Input::new(&self.path).small())),
            )
            .child(
                Checkbox::new("wizard-header")
                    .label(t("wizard-header-row"))
                    .checked(self.header)
                    .on_click(cx.listener(|this, v: &bool, _, cx| {
                        this.header = *v;
                        cx.notify();
                    })),
            );
        let body = match (self.kind, self.step) {
            (WizardKind::Export, 0) => v_flex().gap_2().child(format_buttons).into_any_element(),
            (WizardKind::Export, 1) | (WizardKind::Import, 0) => file_and_header.into_any_element(),
            (WizardKind::Import, 1) => v_flex()
                .gap_2()
                .child(format_buttons)
                .child(
                    h_flex()
                        .gap_2()
                        .child(t("wizard-target"))
                        .child(div().w(px(260.)).child(Input::new(&self.target).small())),
                )
                .child(
                    Checkbox::new("wizard-create")
                        .label(t("wizard-create-table"))
                        .checked(self.create)
                        .on_click(cx.listener(|this, v: &bool, _, cx| {
                            this.create = *v;
                            cx.notify();
                        })),
                )
                .child(
                    Checkbox::new("wizard-truncate")
                        .label(t("wizard-truncate"))
                        .checked(self.truncate)
                        .on_click(cx.listener(|this, v: &bool, _, cx| {
                            this.truncate = *v;
                            cx.notify();
                        })),
                )
                .into_any_element(),
            _ => div()
                .id("wizard-preview")
                .test_support()
                .p_2()
                .rounded_md()
                .bg(theme.muted)
                .font_family(theme.mono_font_family.clone())
                .text_sm()
                .max_h(px(320.))
                .overflow_y_scroll()
                .child(self.summary(cx))
                .into_any_element(),
        };
        v_flex()
            .id("wizard")
            .test_support()
            .gap_3()
            .min_h(px(220.))
            .child(indicator)
            .child(body)
    }
}

/// Opens a wizard for the current context (last console result, explorer
/// selection).
pub fn open(kind: WizardKind, window: &mut Window, cx: &mut App) {
    let source = (kind == WizardKind::Export)
        .then(|| export_source(cx))
        .flatten();
    let target = (kind == WizardKind::Import)
        .then(|| import_target(cx))
        .flatten();
    open_with(kind, source, target, window, cx);
}

/// Opens a wizard for an explicit source / target.
pub fn open_with(
    kind: WizardKind,
    source: Option<ExportSource>,
    target: Option<ImportTarget>,
    window: &mut Window,
    cx: &mut App,
) {
    let wizard = cx.new(|cx| Wizard::new(kind, source, target, window, cx));
    cx.set_global(LastWizard(Some(wizard.clone())));
    let title = match kind {
        WizardKind::Export => t("action-export-data"),
        WizardKind::Import => t("action-import-data"),
    };
    window.open_dialog(cx, move |dialog, _, cx| {
        let step = wizard.read(cx).step;
        let last = step == 2;
        let (w1, w2) = (wizard.clone(), wizard.clone());
        dialog
            .title(title.clone())
            .w(px(720.))
            .child(wizard.clone())
            .footer(
                DialogFooter::new()
                    .child(
                        Button::new("wizard-back")
                            .label(t("wizard-back"))
                            .disabled(step == 0)
                            .on_click(move |_, _, cx| {
                                w1.update(cx, |w, cx| {
                                    w.step = w.step.saturating_sub(1);
                                    cx.notify();
                                })
                            }),
                    )
                    .child(
                        Button::new("wizard-next")
                            .primary()
                            .label(if last {
                                t("wizard-finish")
                            } else {
                                t("wizard-next")
                            })
                            .on_click(move |_, window, cx| {
                                if last {
                                    window.close_dialog(cx);
                                    w2.update(cx, |w, cx| w.finish(window, cx));
                                } else {
                                    w2.update(cx, |w, cx| w.next(window, cx));
                                }
                            }),
                    ),
            )
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn formats_are_listed() {
        let ex = formats(WizardKind::Export);
        assert!(ex.contains(&DataFormat::Csv) && ex.contains(&DataFormat::Parquet));
        assert!(ex.contains(&DataFormat::Markdown));
        assert!(!formats(WizardKind::Import).contains(&DataFormat::Markdown));
    }

    #[::core::prelude::v1::test]
    fn home_paths() {
        assert_eq!(expand_home("/tmp/x.csv"), PathBuf::from("/tmp/x.csv"));
        assert!(expand_home("~/x.csv").ends_with("x.csv"));
    }
}

//! Export and import wizards (plan 2.8, mockups): step-by-step dialogs with
//! format, options and a preview. Data transfer itself arrives in phase 6.

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
    i18n::{t, t_args},
    output,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WizardKind {
    Export,
    Import,
}

pub const FORMATS: [&str; 7] = [
    "CSV",
    "TSV",
    "JSON",
    "SQL INSERT",
    "Markdown",
    "Excel (xlsx)",
    "Parquet",
];

pub struct Wizard {
    pub kind: WizardKind,
    pub step: usize,
    pub format: usize,
    pub header: bool,
    path: Entity<InputState>,
    target: Entity<InputState>,
}

impl Wizard {
    pub fn new(kind: WizardKind, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let path = cx.new(|cx| {
            InputState::new(window, cx).default_value(match kind {
                WizardKind::Export => "~/export/result.csv",
                WizardKind::Import => "~/data/customers.csv",
            })
        });
        let target = cx.new(|cx| InputState::new(window, cx).default_value("public.customers"));
        Self {
            kind,
            step: 0,
            format: 0,
            header: true,
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

    /// Sample of the output (export) or of the parsed input (import).
    pub fn preview(&self) -> String {
        let rows = [("1", "Anna 1", "Warszawa"), ("2", "Jan 2", "Kraków")];
        match (self.kind, FORMATS[self.format]) {
            (WizardKind::Export, "JSON") => rows
                .iter()
                .map(|(i, n, c)| format!("{{\"id\": {i}, \"name\": \"{n}\", \"city\": \"{c}\"}}"))
                .collect::<Vec<_>>()
                .join(",\n"),
            (WizardKind::Export, "SQL INSERT") => rows
                .iter()
                .map(|(i, n, c)| {
                    format!("INSERT INTO customers (id, name, city) VALUES ({i}, '{n}', '{c}');")
                })
                .collect::<Vec<_>>()
                .join("\n"),
            (WizardKind::Export, "Markdown") => {
                let mut s = String::from("| id | name | city |\n|---|---|---|");
                for (i, n, c) in rows {
                    s.push_str(&format!("\n| {i} | {n} | {c} |"));
                }
                s
            }
            (_, f) => {
                let sep = if f == "TSV" { "\t" } else { "," };
                let mut lines = Vec::new();
                if self.header {
                    lines.push(["id", "name", "city"].join(sep));
                }
                lines.extend(rows.iter().map(|(i, n, c)| [*i, *n, *c].join(sep)));
                lines.join("\n")
            }
        }
    }

    pub fn finish(&self, window: &mut Window, cx: &mut App) {
        let path = self.path.read(cx).value().to_string();
        let msg = match self.kind {
            WizardKind::Export => t_args(
                "wizard-export-done",
                &[
                    ("format", FORMATS[self.format].into()),
                    ("path", path.into()),
                ],
            ),
            WizardKind::Import => t_args(
                "wizard-import-done",
                &[
                    ("path", path.into()),
                    ("target", self.target.read(cx).value().to_string().into()),
                ],
            ),
        };
        output::info(msg.clone(), cx);
        window.push_notification(Notification::info(msg), cx);
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
        let body = match (self.kind, self.step) {
            (WizardKind::Export, 0) | (WizardKind::Import, 1) => v_flex()
                .gap_2()
                .child(
                    ButtonGroup::new("wizard-format")
                        .small()
                        .children(FORMATS.iter().enumerate().map(|(i, f)| {
                            Button::new(("wizard-f", i))
                                .label(*f)
                                .selected(i == self.format)
                        }))
                        .on_click(cx.listener(|this, sel: &Vec<usize>, _, cx| {
                            if let Some(i) = sel.first() {
                                this.format = *i;
                                cx.notify();
                            }
                        })),
                )
                .when(self.kind == WizardKind::Import, |d| {
                    d.child(
                        h_flex()
                            .gap_2()
                            .child(t("wizard-target"))
                            .child(div().w(px(260.)).child(Input::new(&self.target).small())),
                    )
                })
                .into_any_element(),
            (_, 0) | (WizardKind::Export, 1) => v_flex()
                .gap_2()
                .child(
                    h_flex()
                        .gap_2()
                        .child(t("wizard-file"))
                        .child(div().w(px(360.)).child(Input::new(&self.path).small())),
                )
                .child(
                    Checkbox::new("wizard-header")
                        .label(t("wizard-header-row"))
                        .checked(self.header)
                        .on_click(cx.listener(|this, v: &bool, _, cx| {
                            this.header = *v;
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
                .child(self.preview())
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

pub fn open(kind: WizardKind, window: &mut Window, cx: &mut App) {
    let wizard = cx.new(|cx| Wizard::new(kind, window, cx));
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
            .w(px(680.))
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
                                    w2.update(cx, |w, cx| {
                                        w.step += 1;
                                        cx.notify();
                                    });
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
        assert!(FORMATS.contains(&"CSV") && FORMATS.contains(&"Parquet"));
    }
}

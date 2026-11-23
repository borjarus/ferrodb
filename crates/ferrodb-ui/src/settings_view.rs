//! Settings dialog: appearance, language, editor, AI assistant, agents and
//! MCP (phase 9), updates and privacy (phase 10), developer.

use ferrodb_ui_kit::{
    gpui::{prelude::*, *},
    icons,
    kit::{
        WindowExt as _,
        setting::{
            NumberFieldOptions, SelectIndex, SettingField, SettingGroup, SettingItem, SettingPage,
            Settings,
        },
    },
};

use crate::{
    ai_settings::{AgentsSettingsView, AiSettingsView, LastSettingsViews},
    apply_look,
    i18n::{self, Locale, t},
    release::ReleaseSettingsView,
    settings::{AppSettings, SettingsStore},
};

/// Pages of the settings dialog, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsPageId {
    General,
    Editor,
    Ai,
    Agents,
    Updates,
    Developer,
}

impl SettingsPageId {
    fn index(self) -> usize {
        match self {
            Self::General => 0,
            Self::Editor => 1,
            Self::Ai => 2,
            Self::Agents => 3,
            Self::Updates => 4,
            Self::Developer => 5,
        }
    }
}

/// Updates settings and applies their visible effects in every window.
pub fn change(cx: &mut App, f: impl FnOnce(&mut AppSettings)) {
    SettingsStore::update(cx, f);
    let s = SettingsStore::get(cx).clone();
    i18n::set_locale(s.locale());
    apply_look(&s, cx);
    cx.refresh_windows();
}

fn appearance_options() -> Vec<(SharedString, SharedString)> {
    ["system", "light", "dark"]
        .into_iter()
        .map(|id| {
            (
                SharedString::from(id),
                SharedString::from(t(&format!("appearance-{id}"))),
            )
        })
        .collect()
}

fn locale_options() -> Vec<(SharedString, SharedString)> {
    let mut v = vec![(
        SharedString::from(""),
        SharedString::from(t("language-system")),
    )];
    v.extend(Locale::ALL.iter().map(|l| {
        (
            SharedString::from(l.id()),
            SharedString::from(l.native_name()),
        )
    }));
    v
}

pub fn settings_element(
    ai: Entity<AiSettingsView>,
    agents: Entity<AgentsSettingsView>,
    release: Entity<ReleaseSettingsView>,
    page: SettingsPageId,
) -> Settings {
    let general = SettingPage::new(t("settings-general"))
        .icon(icons::Lucide::Settings)
        .default_open(page == SettingsPageId::General)
        .group(
            SettingGroup::new()
                .title(t("settings-appearance"))
                .item(SettingItem::new(
                    t("settings-theme"),
                    SettingField::dropdown(
                        appearance_options(),
                        |cx| SettingsStore::get(cx).appearance.clone().into(),
                        |v, cx| change(cx, |s| s.appearance = v.to_string()),
                    ),
                ))
                .item(SettingItem::new(
                    t("settings-ui-font-size"),
                    SettingField::number_input(
                        NumberFieldOptions {
                            min: 10.,
                            max: 24.,
                            step: 1.,
                        },
                        |cx| SettingsStore::get(cx).ui_font_size as f64,
                        |v, cx| change(cx, |s| s.ui_font_size = v as f32),
                    ),
                )),
        )
        .group(
            SettingGroup::new().title(t("settings-language")).item(
                SettingItem::new(
                    t("settings-language"),
                    SettingField::dropdown(
                        locale_options(),
                        |cx| SettingsStore::get(cx).locale.clone().into(),
                        |v, cx| change(cx, |s| s.locale = v.to_string()),
                    ),
                )
                .description(t("settings-language-description")),
            ),
        );
    let editor = SettingPage::new(t("settings-editor"))
        .icon(icons::Lucide::SquareTerminal)
        .group(
            SettingGroup::new()
                .item(SettingItem::new(
                    t("settings-editor-font-size"),
                    SettingField::number_input(
                        NumberFieldOptions {
                            min: 8.,
                            max: 32.,
                            step: 1.,
                        },
                        |cx| SettingsStore::get(cx).editor_font_size as f64,
                        |v, cx| change(cx, |s| s.editor_font_size = v as f32),
                    ),
                ))
                .item(SettingItem::new(
                    t("settings-editor-font-family"),
                    SettingField::input(
                        |cx| SettingsStore::get(cx).editor_font_family.clone().into(),
                        |v, cx| change(cx, |s| s.editor_font_family = v.to_string()),
                    ),
                ))
                .item(SettingItem::new(
                    t("settings-line-numbers"),
                    SettingField::switch(
                        |cx| SettingsStore::get(cx).show_line_numbers,
                        |v, cx| change(cx, |s| s.show_line_numbers = v),
                    ),
                )),
        );
    let ai_page = SettingPage::new(t("settings-ai"))
        .icon(icons::Lucide::Sparkles)
        .default_open(page == SettingsPageId::Ai)
        .group(
            SettingGroup::new().item(SettingItem::render(move |_, _, _| ai.clone()).keywords([
                "ai",
                "llm",
                "anthropic",
                "openai",
                "ollama",
                "model",
                "api key",
            ])),
        );
    let agents_page = SettingPage::new(t("settings-agents"))
        .icon(icons::Lucide::Bot)
        .default_open(page == SettingsPageId::Agents)
        .group(
            SettingGroup::new().item(
                SettingItem::render(move |_, _, _| agents.clone())
                    .keywords(["mcp", "agents", "consent", "audit", "token", "policy"]),
            ),
        );
    let updates_page = SettingPage::new(t("settings-updates"))
        .icon(icons::Lucide::RefreshCw)
        .default_open(page == SettingsPageId::Updates)
        .group(SettingGroup::new().item(
            SettingItem::render(move |_, _, _| release.clone()).keywords([
                "update",
                "beta",
                "channel",
                "crash",
                "privacy",
                "telemetry",
                "consent",
            ]),
        ));
    let developer = SettingPage::new(t("settings-developer"))
        .icon(icons::Lucide::Bug)
        .group(
            SettingGroup::new().item(
                SettingItem::new(
                    t("settings-developer-mode"),
                    SettingField::switch(
                        |cx| SettingsStore::get(cx).developer_mode,
                        |v, cx| change(cx, |s| s.developer_mode = v),
                    ),
                )
                .description(t("settings-developer-mode-description")),
            ),
        );
    // A new element id per opened page: the selection is element state.
    Settings::new(("settings", page.index()))
        .default_selected_index(SelectIndex {
            page_ix: page.index(),
            group_ix: None,
        })
        .pages([
            general,
            editor,
            ai_page,
            agents_page,
            updates_page,
            developer,
        ])
}

pub fn open(window: &mut Window, cx: &mut App) {
    open_page(SettingsPageId::General, window, cx)
}

/// Opens the settings dialog on `page`.
pub fn open_page(page: SettingsPageId, window: &mut Window, cx: &mut App) {
    let ai = cx.new(|cx| AiSettingsView::new(window, cx));
    let agents = cx.new(|cx| AgentsSettingsView::new(window, cx));
    let release = cx.new(|cx| ReleaseSettingsView::new(window, cx));
    cx.set_global(LastSettingsViews {
        ai: Some(ai.clone()),
        agents: Some(agents.clone()),
        release: Some(release.clone()),
    });
    window.open_dialog(cx, move |dialog, _, _| {
        dialog
            .title(t("action-open-settings"))
            .w(px(860.))
            .h(px(600.))
            .child(div().size_full().child(settings_element(
                ai.clone(),
                agents.clone(),
                release.clone(),
                page,
            )))
    });
}

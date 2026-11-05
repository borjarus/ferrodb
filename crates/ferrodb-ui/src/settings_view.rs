//! Settings dialog (skeleton: appearance, language, editor, developer).

use ferrodb_ui_kit::{
    gpui::{prelude::*, *},
    icons,
    kit::{
        WindowExt as _,
        setting::{
            NumberFieldOptions, SettingField, SettingGroup, SettingItem, SettingPage, Settings,
        },
    },
};

use crate::{
    apply_look,
    i18n::{self, Locale, t},
    settings::{AppSettings, SettingsStore},
};

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

pub fn settings_element() -> Settings {
    let general = SettingPage::new(t("settings-general"))
        .icon(icons::Lucide::Settings)
        .default_open(true)
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
    Settings::new("settings").pages([general, editor, developer])
}

pub fn open(window: &mut Window, cx: &mut App) {
    window.open_dialog(cx, |dialog, _, _| {
        dialog
            .title(t("action-open-settings"))
            .w(px(780.))
            .h(px(540.))
            .child(div().size_full().child(settings_element()))
    });
}

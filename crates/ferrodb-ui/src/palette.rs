//! "Find Action" and "Search Everywhere" palettes.
//!
//! Both are a GPUI Kit `Command` hosted in a dialog. Filtering is ours
//! (`filterable(false)`): a fuzzy matcher (nucleo) ranks actions and backend
//! objects consistently. Confirming closes the dialog, returns focus to the
//! workspace and runs the target there, so workspace-level handlers apply.

use std::{cell::RefCell, rc::Rc};

use ferrodb_api::{ObjectKind, SearchEntry, SearchTarget};
use ferrodb_ui_kit::{
    gpui::{prelude::*, *},
    icons,
    kit::{
        ActiveTheme, Icon, Sizable as _, WindowExt as _,
        command::{Command, CommandItem, CommandState},
        h_flex,
        kbd::Kbd,
    },
};
use nucleo_matcher::{
    Config, Matcher, Utf32Str,
    pattern::{CaseMatching, Normalization, Pattern},
};

use crate::{
    AppState,
    actions::{ACTIONS, ActionSpec},
    i18n::t,
    workspace::Workspace,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaletteKind {
    Actions,
    Everywhere,
}

#[derive(Clone)]
pub enum Target {
    Action(&'static ActionSpec),
    Search(SearchEntry),
}

#[derive(Clone)]
pub struct Entry {
    pub label: String,
    pub detail: String,
    /// Extra text matched but not shown.
    pub keywords: String,
    pub target: Target,
}

/// Maximum rows rendered; the matcher still scores everything.
const MAX_SHOWN: usize = 200;

pub struct PaletteModel {
    pub entries: Vec<Entry>,
    pub matches: Vec<usize>,
    matcher: Matcher,
}

impl PaletteModel {
    pub fn new(entries: Vec<Entry>) -> Self {
        let matches = (0..entries.len().min(MAX_SHOWN)).collect();
        Self {
            entries,
            matches,
            matcher: Matcher::new(Config::DEFAULT),
        }
    }

    /// Re-ranks entries for `query` (empty query = original order).
    pub fn set_query(&mut self, query: &str) {
        if query.trim().is_empty() {
            self.matches = (0..self.entries.len().min(MAX_SHOWN)).collect();
            return;
        }
        let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
        let mut buf = Vec::new();
        let mut scored: Vec<(u32, usize)> = self
            .entries
            .iter()
            .enumerate()
            .filter_map(|(i, e)| {
                let label = pattern.score(Utf32Str::new(&e.label, &mut buf), &mut self.matcher);
                let extra = pattern
                    .score(Utf32Str::new(&e.keywords, &mut buf), &mut self.matcher)
                    .map(|s| s / 2);
                label.max(extra).map(|s| (s, i))
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        self.matches = scored.into_iter().take(MAX_SHOWN).map(|(_, i)| i).collect();
    }

    pub fn matched(&self, row: usize) -> Option<&Entry> {
        self.matches.get(row).and_then(|&i| self.entries.get(i))
    }
}

pub fn action_entries() -> Vec<Entry> {
    ACTIONS
        .iter()
        .map(|spec| Entry {
            label: t(spec.label_id),
            detail: t(spec.category.label_id()),
            keywords: spec.keywords.join(" "),
            target: Target::Action(spec),
        })
        .collect()
}

fn kind_label(kind: Option<ObjectKind>) -> String {
    match kind {
        None => t("search-kind-data-source"),
        Some(k) => t(&format!("object-{}", format!("{k:?}").to_ascii_lowercase())),
    }
}

pub fn search_entries(cx: &App) -> Vec<Entry> {
    let mut v: Vec<Entry> = AppState::backend(cx)
        .search()
        .entries()
        .into_iter()
        .map(|e| Entry {
            detail: format!("{} · {}", kind_label(e.kind), e.location),
            label: e.label.clone(),
            keywords: e.location.clone(),
            target: Target::Search(e),
        })
        .collect();
    v.extend(action_entries());
    v
}

fn entry_icon(e: &Entry) -> Icon {
    match &e.target {
        Target::Action(_) => Icon::from(icons::Lucide::Zap),
        Target::Search(s) => match (&s.target, s.kind) {
            (SearchTarget::DataSource(_), _) => Icon::from(icons::Lucide::Database),
            (SearchTarget::File(_), _) => Icon::from(icons::Lucide::File),
            (_, Some(k)) => icons::object_icon(k),
            _ => Icon::from(icons::Lucide::Search),
        },
    }
}

fn item(e: &Entry) -> CommandItem {
    let (label, detail, icon) = (e.label.clone(), e.detail.clone(), entry_icon(e));
    let spec = match &e.target {
        Target::Action(s) => Some(*s),
        Target::Search(_) => None,
    };
    CommandItem::new()
        .label(label.clone())
        .child(move |window, cx| {
            let keys =
                spec.and_then(|s| Kbd::binding_for_action((s.build)().as_ref(), s.context, window));
            h_flex()
                .w_full()
                .gap_2()
                .child(icon.clone().small().text_color(cx.theme().muted_foreground))
                .child(label.clone())
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(detail.clone()),
                )
                .child(div().flex_1())
                .children(keys)
        })
}

/// Opens a palette in `window`. Returns the shared model (used by UI tests).
pub fn open(
    kind: PaletteKind,
    workspace: WeakEntity<Workspace>,
    window: &mut Window,
    cx: &mut App,
) -> Rc<RefCell<PaletteModel>> {
    let entries = match kind {
        PaletteKind::Actions => action_entries(),
        PaletteKind::Everywhere => search_entries(cx),
    };
    let model = Rc::new(RefCell::new(PaletteModel::new(entries)));
    let state = cx.new(|cx| CommandState::new(window, cx));
    let placeholder = t(match kind {
        PaletteKind::Actions => "palette-actions-placeholder",
        PaletteKind::Everywhere => "palette-search-placeholder",
    });
    let m = model.clone();
    let st = state.clone();
    window.open_dialog(cx, move |dialog, _, _| {
        let items: Vec<CommandItem> = {
            let m = m.borrow();
            m.matches.iter().map(|&i| item(&m.entries[i])).collect()
        };
        let on_query = m.clone();
        let on_confirm = m.clone();
        let ws = workspace.clone();
        dialog.close_button(false).w(px(640.)).p_0().child(
            Command::new(&st)
                .placeholder(placeholder.clone())
                .filterable(false)
                .max_h(px(420.))
                .items(items)
                .empty(|_, _, _| div().p_3().child(t("palette-no-results")))
                .on_query(move |q, window, _| {
                    on_query.borrow_mut().set_query(q);
                    window.refresh();
                })
                .on_confirm(move |ix, window, cx| {
                    let target = on_confirm
                        .borrow()
                        .matched(ix.row)
                        .map(|e| e.target.clone());
                    window.close_dialog(cx);
                    if let Some(target) = target {
                        let _ = ws.update(cx, |ws, cx| ws.run_target(target, window, cx));
                    }
                }),
        )
    });
    state.update(cx, |s, cx| s.focus(window, cx));
    model
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn fuzzy_ranks_best_match_first() {
        let mut m = PaletteModel::new(action_entries());
        m.set_query("nwcons");
        let first = m.matched(0).expect("a match");
        assert_eq!(first.label, t("action-new-console"));
        m.set_query("zzzzqqq");
        assert!(m.matches.is_empty());
        m.set_query("");
        assert_eq!(m.matches.len(), ACTIONS.len());
    }
}

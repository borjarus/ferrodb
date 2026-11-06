//! Key browser (plan 2.6): keys grouped into a tree by a separator, with
//! type / TTL / size columns, pattern filter and a value pane per type with
//! editing through the operation preview.

use std::collections::BTreeMap;

use ferrodb_api::{Change, ChangeSet, KeyEntry, KeyValue};
use ferrodb_ui_kit::{
    gpui::{prelude::*, *},
    icons,
    kit::{
        ActiveTheme, Disableable as _, Sizable as _, WindowExt as _,
        button::{Button, ButtonVariants as _},
        dialog::DialogFooter,
        h_flex,
        input::{Editor, EditorState, Input, InputEvent, InputState},
        list::ListItem,
        notification::Notification,
        tree::{Tree, TreeItem, TreeState},
        v_flex,
    },
};

use super::{ResultData, ResultModel, ResultViewEvent, changes, format};
use crate::{
    AppState,
    i18n::{t, t_args},
    inspector::{self, Inspection},
};

const SEPARATOR: char = ':';
const SEP: char = '\u{1f}';

#[derive(Default)]
struct Node {
    children: BTreeMap<String, Node>,
    /// Index into the loaded keys when this node is a key.
    key: Option<usize>,
    count: usize,
}

/// Groups keys by `separator` into tree items (prefixes first, then keys).
pub fn key_tree(keys: &[KeyEntry], separator: char) -> Vec<TreeItem> {
    let mut root = Node::default();
    for (i, k) in keys.iter().enumerate() {
        let mut node = &mut root;
        let parts: Vec<&str> = k.key.split(separator).collect();
        for p in &parts[..parts.len() - 1] {
            node.count += 1;
            node = node.children.entry(format!("{p}{separator}")).or_default();
        }
        node.count += 1;
        let leaf = node
            .children
            .entry(parts[parts.len() - 1].to_owned())
            .or_default();
        leaf.key = Some(i);
    }
    fn items(node: &Node, prefix: &str, keys: &[KeyEntry]) -> Vec<TreeItem> {
        let mut prefixes = Vec::new();
        let mut leaves = Vec::new();
        for (name, child) in &node.children {
            let full = format!("{prefix}{name}");
            if let Some(ix) = child.key
                && child.children.is_empty()
            {
                let k = &keys[ix];
                leaves.push(TreeItem::new(
                    format!("k:{ix}"),
                    format!(
                        "{name}{SEP}{}{SEP}{}{SEP}{}",
                        format::key_type_label(k.kind),
                        format::ttl(k.ttl),
                        k.size.map(format::bytes).unwrap_or_default()
                    ),
                ));
            } else {
                prefixes.push(
                    TreeItem::new(
                        format!("p:{full}"),
                        format!("{name}{SEP}{SEP}{SEP}{}", child.count),
                    )
                    .children(items(child, &full, keys)),
                );
            }
        }
        prefixes.extend(leaves);
        prefixes
    }
    let mut out = items(&root, "", keys);
    // A single top-level prefix (typical: `app:`) is expanded, recursively.
    let mut level = &mut out;
    while level.len() == 1 && !level[0].children.is_empty() {
        let only = level.remove(0).expanded(true);
        level.push(only);
        level = &mut level[0].children;
    }
    out
}

pub struct KeyView {
    model: Entity<ResultModel>,
    tree: Entity<TreeState>,
    pattern: Entity<InputState>,
    built: usize,
    selected: Option<KeyEntry>,
    pub value: Option<KeyValue>,
    loading: bool,
    /// Text to load into the editor on the next render (needs a window).
    pending_text: Option<String>,
    /// Editor for string / JSON values.
    text: Entity<EditorState>,
    ttl: Entity<InputState>,
    focus: FocusHandle,
    _subs: Vec<Subscription>,
}

impl EventEmitter<ResultViewEvent> for KeyView {}

impl KeyView {
    pub fn new(model: Entity<ResultModel>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let tree = cx.new(|cx| TreeState::new(cx));
        let initial = match &model.read(cx).data {
            ResultData::Keys { pattern, .. } => pattern.clone(),
            _ => "*".into(),
        };
        let pattern = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(t("keys-pattern"))
                .default_value(initial)
        });
        let text = cx.new(|cx| {
            EditorState::new(window, cx)
                .language("javascript")
                .line_number(false)
        });
        let ttl = cx.new(|cx| InputState::new(window, cx).placeholder(t("keys-ttl-seconds")));
        let subs = vec![
            cx.observe(&model, |this, _, cx| this.sync(cx)),
            cx.observe(&tree, |this, tree, cx| {
                let ix = tree.read(cx).selected_item().and_then(|i| {
                    i.id.strip_prefix("k:")
                        .and_then(|s| s.parse::<usize>().ok())
                });
                let key = ix.and_then(|ix| match &this.model.read(cx).data {
                    ResultData::Keys { keys, .. } => keys.get(ix).cloned(),
                    _ => None,
                });
                if let Some(key) = key
                    && this.selected.as_ref() != Some(&key)
                {
                    this.select_key(key, cx);
                }
            }),
            cx.subscribe(&pattern, |this, input, ev: &InputEvent, cx| {
                if let InputEvent::PressEnter { .. } = ev {
                    let p = input.read(cx).value().trim().to_string();
                    let page = this.model.read(cx).page_size;
                    let p = if p.is_empty() { "*".to_owned() } else { p };
                    cx.emit(ResultViewEvent::Rerun(format!(
                        "SCAN 0 MATCH {p} COUNT {page}"
                    )));
                }
            }),
        ];
        let mut this = Self {
            model,
            tree,
            pattern,
            built: usize::MAX,
            selected: None,
            value: None,
            loading: false,
            pending_text: None,
            text,
            ttl,
            focus: cx.focus_handle(),
            _subs: subs,
        };
        this.sync(cx);
        this
    }

    pub fn model(&self) -> &Entity<ResultModel> {
        &self.model
    }

    pub fn tree(&self) -> &Entity<TreeState> {
        &self.tree
    }

    pub fn selected(&self) -> Option<&KeyEntry> {
        self.selected.as_ref()
    }

    fn sync(&mut self, cx: &mut Context<Self>) {
        let ResultData::Keys { keys, .. } = &self.model.read(cx).data else {
            return;
        };
        if keys.len() != self.built {
            self.built = keys.len();
            let items = key_tree(keys, SEPARATOR);
            self.tree.update(cx, |t, cx| t.set_items(items, cx));
        }
        cx.notify();
    }

    /// Selects a key and loads its value.
    pub fn select_key(&mut self, key: KeyEntry, cx: &mut Context<Self>) {
        self.selected = Some(key.clone());
        self.value = None;
        self.loading = true;
        cx.notify();
        let (session, backend) = (self.model.read(cx).session, AppState::backend(cx));
        let name = key.key.clone();
        let task = cx
            .background_executor()
            .spawn(async move { backend.queries().key_value(session, name).await });
        cx.spawn(async move |this, cx| {
            let v = task.await;
            let _ = this.update(cx, |this, cx| {
                this.loading = false;
                if this.selected.as_ref().map(|k| &k.key) != Some(&key.key) {
                    return; // a newer selection won
                }
                match v {
                    Ok(v) => this.show_value(v, cx),
                    Err(e) => crate::output::log(crate::output::Level::Error, e.to_string(), cx),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn show_value(&mut self, v: KeyValue, cx: &mut Context<Self>) {
        let Some(k) = &self.selected else { return };
        let fields: Vec<(String, String)> = match &v {
            KeyValue::Hash(h) => h.clone(),
            KeyValue::List(l) | KeyValue::Set(l) => l
                .iter()
                .enumerate()
                .map(|(i, x)| (i.to_string(), x.clone()))
                .collect(),
            KeyValue::ZSet(z) => z.iter().map(|(m, s)| (m.clone(), s.to_string())).collect(),
            KeyValue::Stream(s) => s
                .iter()
                .map(|e| {
                    let f: Vec<String> = e.fields.iter().map(|(k, v)| format!("{k}={v}")).collect();
                    (e.id.clone(), f.join(" "))
                })
                .collect(),
            KeyValue::String(_) | KeyValue::Json(_) => Vec::new(),
        };
        let text = match &v {
            KeyValue::String(s) | KeyValue::Json(s) => Some(s.clone()),
            _ => None,
        };
        inspector::show(
            Inspection {
                title: k.key.clone(),
                subtitle: format!(
                    "{} · TTL {} · {}",
                    format::key_type_label(k.kind),
                    format::ttl(k.ttl),
                    t_args("keys-elements", &[("count", v.len().into())])
                ),
                value: text.clone(),
                fields,
            },
            cx,
        );
        self.value = Some(v);
        if let Some(text) = text {
            self.pending_text = Some(text);
        }
    }

    fn check_editable(&self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.model.read(cx).editable() {
            return true;
        }
        window.push_notification(Notification::warning(t("grid-read-only")), cx);
        false
    }

    /// Sends one change through the operation preview, then reloads the value.
    pub fn apply(&mut self, change: Change, window: &mut Window, cx: &mut Context<Self>) {
        if !self.check_editable(window, cx) {
            return;
        }
        let view = cx.entity().downgrade();
        changes::review_and_apply(
            &self.model,
            ChangeSet {
                changes: vec![change],
            },
            move |_, cx| {
                let _ = view.update(cx, |this, cx| {
                    if let Some(k) = this.selected.clone() {
                        this.select_key(k, cx);
                    }
                    cx.emit(ResultViewEvent::Applied);
                });
            },
            window,
            cx,
        );
    }

    fn save_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(k) = &self.selected else { return };
        let value = self.text.read(cx).value().to_string();
        let change = Change::SetKey {
            key: k.key.clone(),
            value,
        };
        self.apply(change, window, cx);
    }

    fn save_ttl(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(k) = &self.selected else { return };
        let text = self.ttl.read(cx).value().trim().to_string();
        let ttl_secs = if text.is_empty() {
            None
        } else {
            match text.parse::<u64>() {
                Ok(s) => Some(s),
                Err(_) => {
                    window.push_notification(Notification::error(t("keys-ttl-invalid")), cx);
                    return;
                }
            }
        };
        let change = Change::ExpireKey {
            key: k.key.clone(),
            ttl_secs,
        };
        self.apply(change, window, cx);
    }

    fn delete_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(k) = &self.selected {
            let change = Change::DeleteKey { key: k.key.clone() };
            self.apply(change, window, cx);
        }
    }

    /// Edits one member (hash field, list index, set member, zset score).
    fn edit_member(
        &mut self,
        member: String,
        current: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(k) = self.selected.clone() else {
            return;
        };
        if !self.check_editable(window, cx) {
            return;
        }
        let input = cx.new(|cx| InputState::new(window, cx).default_value(current));
        let view = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, _, _| {
            let ok = {
                let (view, input, key, member) =
                    (view.clone(), input.clone(), k.key.clone(), member.clone());
                move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                    let value = input.read(cx).value().to_string();
                    window.close_dialog(cx);
                    let change = Change::SetKeyMember {
                        key: key.clone(),
                        member: member.clone(),
                        value,
                    };
                    let _ = view.update(cx, |this, cx| this.apply(change, window, cx));
                }
            };
            dialog
                .title(t_args(
                    "keys-edit-member",
                    &[("member", member.clone().into())],
                ))
                .w(px(420.))
                .child(Input::new(&input))
                .footer(
                    DialogFooter::new()
                        .child(
                            Button::new("key-member-cancel")
                                .label(t("button-cancel"))
                                .on_click(|_, w, cx| w.close_dialog(cx)),
                        )
                        .child(
                            Button::new("key-member-ok")
                                .primary()
                                .label(t("button-ok"))
                                .on_click(ok),
                        ),
                )
        });
    }

    fn render_tree(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        Tree::new(&self.tree, move |ix, entry, selected, _, _| {
            let label = entry.item().label.clone();
            let p: Vec<&str> = label.split(SEP).collect();
            let is_key = entry.item().id.starts_with("k:");
            let icon = if is_key {
                icons::Lucide::Key
            } else {
                icons::Lucide::Folder
            };
            let col = |w: f32, s: &str| {
                div()
                    .w(px(w))
                    .flex_shrink_0()
                    .text_xs()
                    .text_color(muted)
                    .child(s.to_owned())
            };
            ListItem::new(ix)
                .selected(selected)
                .pl(px(8. + 14. * entry.depth() as f32))
                .child(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .child(icons::Icon::from(icon).xsmall().text_color(muted))
                        .child(
                            div()
                                .flex_1()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .child(p.first().copied().unwrap_or_default().to_owned()),
                        )
                        .when(is_key, |d| {
                            d.child(col(56., p.get(1).copied().unwrap_or_default()))
                                .child(col(70., p.get(2).copied().unwrap_or_default()))
                                .child(col(64., p.get(3).copied().unwrap_or_default()))
                        })
                        .when(!is_key, |d| {
                            d.child(col(64., p.get(3).copied().unwrap_or_default()))
                        }),
                )
        })
        .size_full()
    }

    fn render_value(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if let Some(text) = self.pending_text.take() {
            self.text.update(cx, |e, cx| e.set_value(text, window, cx));
        }
        let theme = cx.theme().clone();
        let Some(k) = self.selected.clone() else {
            return div()
                .p_2()
                .text_color(theme.muted_foreground)
                .child(t("keys-select"))
                .into_any_element();
        };
        let editable = self.model.read(cx).editable();
        let header = v_flex()
            .gap_1()
            .child(div().font_weight(FontWeight::SEMIBOLD).child(k.key.clone()))
            .child(
                h_flex()
                    .gap_2()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(format::key_type_label(k.kind))
                    .child(format!("TTL {}", format::ttl(k.ttl)))
                    .children(k.size.map(format::bytes)),
            )
            .child(
                h_flex()
                    .gap_1()
                    .child(div().w(px(120.)).child(Input::new(&self.ttl).xsmall()))
                    .child(
                        Button::new("key-ttl")
                            .xsmall()
                            .label(t("keys-set-ttl"))
                            .disabled(!editable)
                            .on_click(cx.listener(|this, _, w, cx| this.save_ttl(w, cx))),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("key-delete")
                            .xsmall()
                            .danger()
                            .icon(icons::Lucide::Trash)
                            .label(t("keys-delete"))
                            .disabled(!editable)
                            .on_click(cx.listener(|this, _, w, cx| this.delete_key(w, cx))),
                    ),
            );
        let body = match &self.value {
            None => div().child(t("loading")).into_any_element(),
            Some(KeyValue::String(_) | KeyValue::Json(_)) => v_flex()
                .flex_1()
                .gap_1()
                .child(
                    div()
                        .flex_1()
                        .min_h(px(120.))
                        .child(Editor::new(&self.text).size_full()),
                )
                .child(
                    Button::new("key-save")
                        .xsmall()
                        .primary()
                        .label(t("keys-save"))
                        .disabled(!editable)
                        .on_click(cx.listener(|this, _, w, cx| this.save_text(w, cx))),
                )
                .into_any_element(),
            Some(v) => {
                let (h1, h2, rows): (String, String, Vec<(String, String)>) = match v {
                    KeyValue::Hash(h) => (t("keys-field"), t("keys-value"), h.clone()),
                    KeyValue::List(l) => (
                        t("keys-index"),
                        t("keys-value"),
                        l.iter()
                            .enumerate()
                            .map(|(i, x)| (i.to_string(), x.clone()))
                            .collect(),
                    ),
                    KeyValue::Set(s) => (
                        t("keys-member"),
                        String::new(),
                        s.iter().map(|x| (x.clone(), String::new())).collect(),
                    ),
                    KeyValue::ZSet(z) => (
                        t("keys-member"),
                        t("keys-score"),
                        z.iter().map(|(m, s)| (m.clone(), s.to_string())).collect(),
                    ),
                    KeyValue::Stream(s) => (
                        "ID".into(),
                        t("keys-fields"),
                        s.iter()
                            .map(|e| {
                                let f: Vec<String> =
                                    e.fields.iter().map(|(k, v)| format!("{k}={v}")).collect();
                                (e.id.clone(), f.join("  "))
                            })
                            .collect(),
                    ),
                    KeyValue::String(_) | KeyValue::Json(_) => unreachable!("handled above"),
                };
                let is_set = matches!(v, KeyValue::Set(_));
                let is_stream = matches!(v, KeyValue::Stream(_));
                v_flex()
                    .id("key-members")
                    .test_support()
                    .flex_1()
                    .overflow_y_scroll()
                    .text_sm()
                    .child(
                        h_flex()
                            .gap_2()
                            .font_weight(FontWeight::SEMIBOLD)
                            .border_b_1()
                            .border_color(theme.border)
                            .child(div().w(px(160.)).child(h1))
                            .child(h2),
                    )
                    .children(rows.into_iter().enumerate().map(|(i, (a, b))| {
                        let (member, current) = if is_set {
                            (a.clone(), a.clone())
                        } else {
                            (a.clone(), b.clone())
                        };
                        h_flex()
                            .id(("key-member", i))
                            .gap_2()
                            .px_1()
                            .hover(|s| s.bg(theme.muted))
                            .when(editable && !is_stream, |d| {
                                d.on_click(cx.listener(move |this, ev: &ClickEvent, w, cx| {
                                    if ev.click_count() == 2 {
                                        this.edit_member(member.clone(), current.clone(), w, cx)
                                    }
                                }))
                            })
                            .child(div().w(px(160.)).overflow_hidden().child(a))
                            .child(div().flex_1().overflow_hidden().child(b))
                    }))
                    .into_any_element()
            }
        };
        v_flex()
            .id("key-value")
            .test_support()
            .size_full()
            .p_2()
            .gap_2()
            .child(header)
            .child(body)
            .into_any_element()
    }
}

impl Focusable for KeyView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for KeyView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let m = self.model.read(cx);
        let (loaded, has_more, fetching) = (m.data.len(), m.has_more, m.fetching);
        let value = self.render_value(window, cx);
        v_flex()
            .id("key-view")
            .test_support()
            .track_focus(&self.focus)
            .size_full()
            .child(
                h_flex()
                    .gap_1()
                    .px_1()
                    .py_0p5()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        div().w(px(260.)).child(
                            Input::new(&self.pattern)
                                .xsmall()
                                .prefix(icons::Icon::from(icons::Lucide::Search).xsmall()),
                        ),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(t_args(
                                "keys-count",
                                &[
                                    ("count", loaded.into()),
                                    ("more", if has_more { "yes" } else { "no" }.into()),
                                ],
                            )),
                    )
                    .child(
                        Button::new("keys-load-more")
                            .ghost()
                            .xsmall()
                            .label(t("grid-load-more"))
                            .loading(fetching)
                            .disabled(!has_more)
                            .on_click(cx.listener(|this, _, _, cx| {
                                ResultModel::fetch_more(&this.model, false, cx)
                            })),
                    ),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .child(
                        div()
                            .flex_1()
                            .h_full()
                            .min_w_0()
                            .child(self.render_tree(cx)),
                    )
                    .child(
                        div()
                            .w(px(360.))
                            .h_full()
                            .border_l_1()
                            .border_color(cx.theme().border)
                            .child(value),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferrodb_api::KeyType;

    fn k(key: &str) -> KeyEntry {
        KeyEntry {
            key: key.into(),
            kind: KeyType::String,
            ttl: None,
            size: Some(10),
        }
    }

    #[::core::prelude::v1::test]
    fn keys_group_by_separator() {
        let items = key_tree(
            &[k("app:user:1"), k("app:user:2"), k("app:cfg"), k("solo")],
            ':',
        );
        assert_eq!(items.len(), 2, "prefix `app:` and key `solo`");
        assert_eq!(items[0].id.as_ref(), "p:app:");
        let app = &items[0].children;
        assert_eq!(app[0].id.as_ref(), "p:app:user:");
        assert_eq!(app[0].children.len(), 2);
        assert_eq!(app[1].id.as_ref(), "k:2");
        assert_eq!(items[1].id.as_ref(), "k:3");
    }
}

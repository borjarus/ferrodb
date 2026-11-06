//! Keymap: defaults from [`crate::actions::ACTIONS`] (JetBrains-compatible,
//! plan Appendix B) overridden by the user's `keymap.toml`:
//!
//! ```toml
//! [[binding]]
//! keys = "ctrl-shift-enter"
//! action = "ferrodb::ExecuteStatement"
//! context = "Console"          # optional
//!
//! [[binding]]
//! keys = "ctrl-alt-s"
//! action = ""                  # empty action = remove the default binding of these keys
//! ```
//!
//! `secondary-` means Cmd on macOS and Ctrl elsewhere.
//! "Search Everywhere" is additionally bound to a double Shift press, which is
//! detected by the workspace (GPUI bindings cannot express it).

use std::rc::Rc;

use ferrodb_ui_kit::gpui::{App, KeyBinding, KeyBindingContextPredicate};
use serde::Deserialize;

use crate::actions::ACTIONS;

#[derive(Debug, Default, Deserialize)]
pub struct KeymapFile {
    #[serde(default, rename = "binding")]
    pub bindings: Vec<UserBinding>,
}

#[derive(Debug, Deserialize)]
pub struct UserBinding {
    pub keys: String,
    pub action: String,
    pub context: Option<String>,
}

/// Effective binding: what is installed into GPUI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binding {
    pub keys: String,
    pub action: String,
    pub context: Option<String>,
}

/// Default bindings of all actions for the current platform.
pub fn default_bindings() -> Vec<Binding> {
    ACTIONS
        .iter()
        .filter_map(|a| {
            a.default_keys().map(|k| Binding {
                keys: k.to_owned(),
                action: a.action_name().to_owned(),
                context: a.context.map(str::to_owned),
            })
        })
        .collect()
}

/// Merges user bindings into defaults: a user binding replaces any default with
/// the same keys and context; an empty `action` only removes.
pub fn merge(defaults: Vec<Binding>, user: &KeymapFile) -> Vec<Binding> {
    let mut out = defaults;
    for u in &user.bindings {
        out.retain(|b| !(b.keys == u.keys && b.context == u.context));
        if !u.action.is_empty() {
            out.push(Binding {
                keys: u.keys.clone(),
                action: u.action.clone(),
                context: u.context.clone(),
            });
        }
    }
    out
}

pub fn parse(text: &str) -> Result<KeymapFile, String> {
    toml::from_str(text).map_err(|e| e.to_string())
}

/// Installs bindings into GPUI (once, at startup; keymap changes apply after
/// restart in v0). Returns human-readable problems (unknown actions, bad keystrokes).
pub fn install(bindings: &[Binding], cx: &mut App) -> Vec<String> {
    let mut problems = Vec::new();
    let mut loaded = Vec::new();
    for b in bindings {
        let action = match cx.build_action(&b.action, None) {
            Ok(a) => a,
            Err(e) => {
                problems.push(format!("{}: {e}", b.action));
                continue;
            }
        };
        let predicate = match &b.context {
            Some(c) => match KeyBindingContextPredicate::parse(c) {
                Ok(p) => Some(Rc::new(p)),
                Err(e) => {
                    problems.push(format!("{c}: {e}"));
                    continue;
                }
            },
            None => None,
        };
        match KeyBinding::load(&b.keys, action, predicate, false, None, cx) {
            Ok(kb) => loaded.push(kb),
            Err(e) => problems.push(format!("{}: {e}", b.keys)),
        }
    }
    cx.bind_keys(loaded);
    problems
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appendix_b_defaults_present() {
        let d = default_bindings();
        let has = |action: &str| d.iter().any(|b| b.action == action);
        for a in [
            "ferrodb::ExecuteStatement",
            "ferrodb::StopQuery",
            "ferrodb::FindAction",
            "ferrodb::FormatCode",
            "ferrodb::Rename",
            "ferrodb::GoToDeclaration",
            "ferrodb::LiveTemplates",
            "ferrodb::QuickDocumentation",
        ] {
            assert!(has(a), "{a}");
        }
    }

    #[test]
    fn user_bindings_override_and_remove() {
        let user = parse(
            r#"
            [[binding]]
            keys = "secondary-enter"
            action = "ferrodb::FormatCode"
            context = "Console"

            [[binding]]
            keys = "alt-1"
            action = ""
            "#,
        )
        .unwrap();
        let merged = merge(default_bindings(), &user);
        let enter: Vec<_> = merged
            .iter()
            .filter(|b| b.keys == "secondary-enter" && b.context.as_deref() == Some("Console"))
            .collect();
        assert_eq!(enter.len(), 1);
        assert_eq!(enter[0].action, "ferrodb::FormatCode");
        if !cfg!(target_os = "macos") {
            assert!(!merged.iter().any(|b| b.keys == "alt-1"));
        }
    }
}

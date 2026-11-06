//! Diff view (plan 2.8): compares two texts (DDL of an object in two data
//! sources, a console against a Local History snapshot) with GPUI Kit's diff.

use ferrodb_ui_kit::{
    gpui::{prelude::*, *},
    kit::{
        ActiveTheme,
        diff::{Diff, DiffFile, DiffState},
        h_flex, v_flex,
    },
};

use crate::i18n::t;

/// Unified diff (one hunk with full context) of two texts, by lines (LCS).
pub fn unified_diff(left: &str, right: &str, path: &str) -> String {
    let a: Vec<&str> = left.lines().collect();
    let b: Vec<&str> = right.lines().collect();
    // LCS table, bottom-up.
    let mut lcs = vec![vec![0u32; b.len() + 1]; a.len() + 1];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            lcs[i][j] = if a[i] == b[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let mut body = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < a.len() || j < b.len() {
        if i < a.len() && j < b.len() && a[i] == b[j] {
            body.push(format!(" {}", a[i]));
            i += 1;
            j += 1;
        } else if i < a.len() && (j == b.len() || lcs[i + 1][j] >= lcs[i][j + 1]) {
            body.push(format!("-{}", a[i]));
            i += 1;
        } else {
            body.push(format!("+{}", b[j]));
            j += 1;
        }
    }
    format!(
        "--- a/{path}\n+++ b/{path}\n@@ -1,{} +1,{} @@\n{}\n",
        a.len(),
        b.len(),
        body.join("\n")
    )
}

pub struct DiffView {
    pub left_title: String,
    pub right_title: String,
    pub changes: (usize, usize),
    state: Entity<DiffState>,
}

impl DiffView {
    pub fn new(
        left_title: String,
        left: &str,
        right_title: String,
        right: &str,
        language: &str,
        cx: &mut Context<Self>,
    ) -> Self {
        let patch = unified_diff(left, right, "object.sql");
        let files: Vec<DiffFile> = DiffFile::parse(&patch)
            .unwrap_or_default()
            .into_iter()
            .map(|f| f.with_language(language.to_owned()))
            .collect();
        let changes = files
            .iter()
            .fold((0, 0), |(a, d), f| (a + f.additions(), d + f.deletions()));
        let state = cx.new(|cx| DiffState::new(files, cx));
        Self {
            left_title,
            right_title,
            changes,
            state,
        }
    }
}

impl Render for DiffView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        v_flex()
            .id("diff-view")
            .test_support()
            .size_full()
            .child(
                h_flex()
                    .gap_4()
                    .px_2()
                    .py_1()
                    .text_sm()
                    .child(format!("{} ⟷ {}", self.left_title, self.right_title))
                    .child(
                        div()
                            .text_color(muted)
                            .child(format!("+{} −{}", self.changes.0, self.changes.1)),
                    )
                    .when(self.changes == (0, 0), |d| d.child(t("diff-identical"))),
            )
            .child(div().flex_1().min_h_0().child(Diff::new(&self.state)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn diff_marks_changes() {
        let d = unified_diff("a\nb\nc", "a\nx\nc\nd", "t.sql");
        assert!(d.contains("\n a\n-b\n+x\n c\n+d"), "{d}");
        assert!(d.contains("@@ -1,3 +1,4 @@"));
        let files = DiffFile::parse(&d).unwrap();
        assert_eq!((files[0].additions(), files[0].deletions()), (2, 1));
    }
}

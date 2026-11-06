//! Bridges the backend [`LanguageService`](ferrodb_api::LanguageService)
//! into the editor: completions (popup) and diagnostics (underlines).

use ferrodb_api::{CompletionKind, DataSourceId, DocSnapshot, QueryLanguage, Severity};
use ferrodb_ui_kit::{
    base::input::{Diagnostic, DiagnosticSeverity},
    gpui::{App, Task, Window},
    kit::input::{CompletionProvider, Rope, RopeExt as _},
    lsp_types::{
        self as lsp, CompletionContext, CompletionItemKind, CompletionResponse, CompletionTextEdit,
        TextEdit,
    },
};

use crate::AppState;

/// Completion provider of one console (source + language).
pub struct BackendCompletion {
    pub data_source: Option<DataSourceId>,
    pub language: QueryLanguage,
}

fn kind(k: CompletionKind) -> CompletionItemKind {
    match k {
        CompletionKind::Keyword => CompletionItemKind::KEYWORD,
        CompletionKind::Function => CompletionItemKind::FUNCTION,
        CompletionKind::Table | CompletionKind::Collection => CompletionItemKind::CLASS,
        CompletionKind::Column | CompletionKind::Field => CompletionItemKind::FIELD,
        CompletionKind::Method => CompletionItemKind::METHOD,
        CompletionKind::Operator => CompletionItemKind::OPERATOR,
        CompletionKind::Key => CompletionItemKind::VALUE,
        CompletionKind::Snippet => CompletionItemKind::SNIPPET,
    }
}

pub fn snapshot(text: &str, language: &QueryLanguage, ds: Option<&DataSourceId>) -> DocSnapshot {
    DocSnapshot {
        text: text.into(),
        language: language.clone(),
        data_source: ds.cloned(),
        version: 0,
    }
}

impl CompletionProvider for BackendCompletion {
    fn completions(
        &self,
        text: &Rope,
        offset: usize,
        _trigger: CompletionContext,
        _window: &mut Window,
        cx: &mut App,
    ) -> Task<anyhow::Result<CompletionResponse>> {
        let doc = snapshot(&text.to_string(), &self.language, self.data_source.as_ref());
        let rope = text.clone();
        let backend = AppState::backend(cx);
        cx.background_executor().spawn(async move {
            let items = backend.language().complete(doc, offset).await;
            Ok(CompletionResponse::Array(
                items
                    .into_iter()
                    .map(|i| lsp::CompletionItem {
                        label: i.label,
                        kind: Some(kind(i.kind)),
                        detail: i.detail,
                        text_edit: Some(CompletionTextEdit::Edit(TextEdit {
                            range: lsp::Range {
                                start: rope.offset_to_position(i.replace.start),
                                end: rope.offset_to_position(i.replace.end),
                            },
                            new_text: i.insert_text,
                        })),
                        ..Default::default()
                    })
                    .collect(),
            ))
        })
    }

    fn is_completion_trigger(&self, _offset: usize, new_text: &str, _cx: &mut App) -> bool {
        new_text
            .chars()
            .last()
            .is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '.')
    }
}

/// Converts backend diagnostics (byte ranges) to editor diagnostics.
pub fn editor_diagnostics(rope: &Rope, diags: &[ferrodb_api::Diagnostic]) -> Vec<Diagnostic> {
    diags
        .iter()
        .map(|d| {
            let start = rope.offset_to_position(d.range.start.min(rope.len()));
            let end = rope.offset_to_position(d.range.end.min(rope.len()));
            Diagnostic::new(start..end, d.message.clone()).with_severity(match d.severity {
                Severity::Error => DiagnosticSeverity::Error,
                Severity::Warning => DiagnosticSeverity::Warning,
                Severity::Info => DiagnosticSeverity::Info,
            })
        })
        .collect()
}

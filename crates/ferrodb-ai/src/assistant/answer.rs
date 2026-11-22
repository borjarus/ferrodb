//! Queries found in an answer: fenced code blocks in the data source's
//! language, checked by the language service (syntax errors, risks).

use std::{sync::Arc, time::Duration};

use ferrodb_api::{Backend, DocSnapshot, QueryLanguage, Severity, SuggestedQuery};

use super::{context::SourceContext, prompt::language_profile};

/// A fenced code block: info string (first word, lower case) and body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CodeBlock {
    pub tag: String,
    pub text: String,
}

fn fence_start(line: &str) -> Option<(char, usize)> {
    let ch = line.chars().next().filter(|c| *c == '`' || *c == '~')?;
    let n = line.chars().take_while(|c| *c == ch).count();
    (n >= 3).then_some((ch, n))
}

/// Closed fenced code blocks of a Markdown text, in order. A block left
/// open (an answer cut off at the token limit) is not returned.
pub(crate) fn code_blocks(text: &str) -> Vec<CodeBlock> {
    let mut out = Vec::new();
    let mut open: Option<(char, usize, String, Vec<&str>)> = None;
    for line in text.lines() {
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();
        match &mut open {
            None => {
                if indent > 3 {
                    continue;
                }
                if let Some((ch, n)) = fence_start(trimmed) {
                    let info = trimmed[n..].trim();
                    if ch == '`' && info.contains('`') {
                        continue; // inline code, not a fence
                    }
                    let tag = info
                        .split(|c: char| c.is_whitespace() || c == '{')
                        .next()
                        .unwrap_or_default()
                        .to_lowercase();
                    open = Some((ch, n, tag, Vec::new()));
                }
            }
            Some((ch, n, tag, lines)) => {
                let t = trimmed.trim_end();
                let closes = indent <= 3 && t.len() >= *n && t.chars().all(|c| c == *ch);
                if closes {
                    out.push(CodeBlock {
                        tag: std::mem::take(tag),
                        text: lines.join("\n"),
                    });
                    open = None;
                } else {
                    lines.push(line);
                }
            }
        }
    }
    out
}

/// Blocks that hold queries in `lang` (untagged blocks count).
pub(crate) fn query_blocks(text: &str, lang: &QueryLanguage) -> Vec<String> {
    let profile = language_profile(lang);
    code_blocks(text)
        .into_iter()
        .filter(|b| {
            b.tag.is_empty() || b.tag == profile.fence || profile.aliases.contains(&b.tag.as_str())
        })
        .map(|b| b.text.trim().to_owned())
        .filter(|t| !t.is_empty())
        .collect()
}

/// The queries of an answer, checked: `problems` are the language service's
/// errors, `risks` what the console would ask to confirm when the source
/// is in production (every modification), `modifies` = any risk.
pub(crate) async fn queries(
    backend: &dyn Backend,
    text: &str,
    ctx: &SourceContext,
) -> Vec<SuggestedQuery> {
    let lang = &ctx.language;
    let mut out = Vec::new();
    for query in query_blocks(text, lang) {
        let doc = DocSnapshot {
            text: Arc::from(query.as_str()),
            language: lang.clone(),
            data_source: ctx.data_source.clone(),
            schema: ctx.console_schema.clone(),
            version: 0,
        };
        let problems =
            tokio::time::timeout(Duration::from_secs(3), backend.language().diagnostics(doc))
                .await
                .unwrap_or_default()
                .into_iter()
                .filter(|d| d.severity == Severity::Error)
                .map(|d| d.message)
                .collect();
        let mut risks = Vec::new();
        for r in backend.language().risks(&query, lang, true) {
            if !risks.contains(&r.kind) {
                risks.push(r.kind);
            }
        }
        out.push(SuggestedQuery {
            modifies: !risks.is_empty(),
            text: query,
            language: lang.clone(),
            risks,
            problems,
        });
    }
    out
}

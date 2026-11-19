//! Analysis of a whole console: tokens, statements, their structure and
//! resolution. Tables created by a statement (`CREATE TABLE`, `CREATE VIEW`)
//! are known to the statements after it.

use std::{collections::HashMap, ops::Range};

use super::{
    dialect::Dialect,
    lexer::{Token, TokenKind, tokenize},
    resolve::{Env, LocalTable, Resolution, SourceColumn, TableTarget, resolve, source_columns},
    schema::SchemaModel,
    split::{Statement, split},
    syntax::{Parsed, parse},
};

pub struct StmtAnalysis {
    pub span: Statement,
    /// Significant tokens (absolute ranges).
    pub toks: Vec<Token>,
    pub parsed: Parsed,
    pub refs: Vec<Resolution>,
    pub sources: HashMap<(usize, usize), Option<TableTarget>>,
}

pub struct Document<'a> {
    pub text: &'a str,
    pub env: Env<'a>,
    /// All tokens, trivia included.
    pub tokens: Vec<Token>,
    pub stmts: Vec<StmtAnalysis>,
}

impl<'a> Document<'a> {
    pub fn analyze(
        text: &'a str,
        dialect: &str,
        model: &'a SchemaModel,
        current: Option<&str>,
    ) -> Self {
        let d = Dialect::get(dialect);
        let mut env = Env::new(d, model, current);
        let tokens = tokenize(text, &d.flavor);
        let spans = split(text, &d.flavor);
        let mut stmts = Vec::with_capacity(spans.len());
        let mut ti = 0;
        for span in spans {
            while ti < tokens.len() && tokens[ti].range.end <= span.body.start {
                ti += 1;
            }
            let mut toks = Vec::new();
            let mut j = ti;
            while j < tokens.len() && tokens[j].range.start < span.body.end {
                if !tokens[j].is_trivia() {
                    toks.push(tokens[j].clone());
                }
                j += 1;
            }
            let parsed = parse(text, &toks, d);
            let res = resolve(&parsed, &env);
            let (refs, sources) = (res.refs, res.sources);
            if let Some(c) = &parsed.created
                && let Some(name) = c.name.last()
            {
                let mut columns: Vec<String> = c.columns.iter().map(|x| x.name(d)).collect();
                if columns.is_empty()
                    && let Some(q) = c.query
                {
                    columns = outputs_of(&parsed, &env, &sources, q)
                        .into_iter()
                        .map(|c| c.name)
                        .collect();
                }
                env.locals.push(LocalTable {
                    schema: (c.name.len() > 1).then(|| c.name[c.name.len() - 2].name(d)),
                    name: name.name(d),
                    columns,
                    view: c.view,
                    decl: name.range.clone(),
                });
            }
            stmts.push(StmtAnalysis {
                span,
                toks,
                parsed,
                refs,
                sources,
            });
        }
        Self {
            text,
            env,
            tokens,
            stmts,
        }
    }

    pub fn dialect(&self) -> &'static Dialect {
        self.env.d
    }

    /// Statement containing `offset`, or the one just before it when only
    /// trivia separates them (typing at its end).
    pub fn statement_at(&self, offset: usize) -> Option<usize> {
        self.stmts.iter().rposition(|s| {
            s.span.body.start <= offset
                && (offset <= s.span.body.end
                    || self.text[s.span.body.end..offset]
                        .chars()
                        .all(char::is_whitespace))
        })
    }

    /// Token (any kind) whose range contains `offset` or ends at it.
    pub fn token_at(&self, offset: usize) -> Option<&Token> {
        let i = self.tokens.partition_point(|t| t.range.end < offset);
        let t = self.tokens.get(i)?;
        (t.range.start <= offset).then_some(t)
    }

    /// Identifier-like token touching `offset` (cursor inside or right after).
    pub fn word_at(&self, offset: usize) -> Option<&Token> {
        let i = self.tokens.partition_point(|t| t.range.end < offset);
        [i, i + 1]
            .into_iter()
            .filter_map(|k| self.tokens.get(k))
            .find(|t| {
                t.range.start <= offset
                    && offset <= t.range.end
                    && matches!(t.kind, TokenKind::Word | TokenKind::QuotedIdent)
            })
    }

    /// Columns of a source.
    pub fn columns(&self, stmt: usize, scope: usize, src: usize) -> Option<Vec<SourceColumn>> {
        let s = &self.stmts[stmt];
        source_columns(&s.parsed, &self.env, &s.sources, scope, src, 0)
    }

    /// Reference whose part covers `offset`: (statement, ref, part).
    pub fn ref_at(&self, offset: usize) -> Option<(usize, usize, usize)> {
        let si = self.statement_at(offset)?;
        let s = &self.stmts[si];
        for (ri, r) in s.parsed.refs.iter().enumerate() {
            for (pi, p) in r.parts.iter().enumerate() {
                if p.range.start <= offset && offset <= p.range.end && !p.range.is_empty() {
                    return Some((si, ri, pi));
                }
            }
        }
        None
    }

    pub fn range_text(&self, r: Range<usize>) -> &str {
        &self.text[r]
    }
}

fn outputs_of(
    p: &Parsed,
    env: &Env<'_>,
    sources: &HashMap<(usize, usize), Option<TableTarget>>,
    scope: usize,
) -> Vec<SourceColumn> {
    // Reuse the subquery logic through a synthetic lookup.
    let s = &p.scopes[scope];
    let mut out = Vec::new();
    for (idx, o) in s.outputs.iter().enumerate() {
        if o.star.is_some() {
            for i in 0..s.sources.len() {
                out.extend(source_columns(p, env, sources, scope, i, 0).unwrap_or_default());
            }
        } else if let Some(n) = o.name(&p.refs) {
            out.push(SourceColumn {
                name: n.name(env.d),
                symbol: super::resolve::Symbol::Output { scope, idx },
                data_type: None,
            });
        }
    }
    out
}

//! The sandboxed engine: limits, disabled symbols, helper functions and
//! value conversions.

use std::sync::Arc;

use ferrodb_api::{Error, ScriptValue};
use rhai::{Array, Dynamic, Engine, EvalAltResult, ImmutableString, Map};

/// Operations allowed in one call (one row of an extractor, one aggregate).
pub const MAX_OPERATIONS: u64 = 5_000_000;
/// Longest string a script can build (bytes).
pub const MAX_STRING: usize = 16 * 1024 * 1024;
/// Largest array a script can build.
pub const MAX_ARRAY: usize = 5_000_000;
const MAX_MAP: usize = 100_000;
const MAX_CALL_LEVELS: usize = 64;
/// Token returned by the progress hook when the run is cancelled.
const CANCELLED: &str = "cancelled";

/// A configured engine. `options` is what `options()` returns; `cancel`
/// is polled every 1024 operations.
pub(crate) fn engine(options: Map, cancel: Option<Arc<dyn Fn() -> bool + Send + Sync>>) -> Engine {
    let mut e = Engine::new();
    e.set_module_resolver(rhai::module_resolvers::DummyModuleResolver::new());
    for sym in ["eval", "import", "export"] {
        e.disable_symbol(sym);
    }
    e.set_max_operations(MAX_OPERATIONS)
        .set_max_string_size(MAX_STRING)
        .set_max_array_size(MAX_ARRAY)
        .set_max_map_size(MAX_MAP)
        .set_max_call_levels(MAX_CALL_LEVELS)
        .set_max_expr_depths(128, 64)
        .set_strict_variables(true)
        .set_fail_on_invalid_map_property(true);
    e.on_print(|_| {});
    e.on_debug(|_, _, _| {});
    if let Some(cancel) = cancel {
        e.on_progress(move |ops| {
            (ops % 1024 == 0 && cancel()).then(|| Dynamic::from(CANCELLED.to_string()))
        });
    }
    register_helpers(&mut e);
    e.register_fn("options", move || options.clone());
    e
}

/// The script error as an API error, naming the script.
pub(crate) fn error(script: &str, e: Box<EvalAltResult>) -> Error {
    let mut root = &*e;
    while let EvalAltResult::ErrorInFunctionCall(_, _, inner, _)
    | EvalAltResult::ErrorInModule(_, inner, _) = root
    {
        root = &**inner;
    }
    match root {
        EvalAltResult::ErrorTerminated(..) => Error::Cancelled,
        EvalAltResult::ErrorTooManyOperations(_) => Error::Other(format!(
            "script `{script}` stopped: more than {MAX_OPERATIONS} operations in one call \
             (endless loop?)"
        )),
        EvalAltResult::ErrorDataTooLarge(what, pos) => Error::Other(format!(
            "script `{script}` stopped: {what} over the sandbox limit ({pos})"
        )),
        _ => Error::Other(format!("script `{script}`: {e}")),
    }
}

// ------------------------------------------------------------------ values

pub(crate) fn to_dynamic(v: &ScriptValue) -> Dynamic {
    match v {
        ScriptValue::Null => Dynamic::UNIT,
        ScriptValue::Bool(b) => Dynamic::from_bool(*b),
        ScriptValue::Int(i) => Dynamic::from_int(*i),
        ScriptValue::Float(f) => Dynamic::from_float(*f),
        ScriptValue::Text(s) => Dynamic::from(s.clone()),
    }
}

pub(crate) fn row_array(row: &[ScriptValue]) -> Array {
    row.iter().map(to_dynamic).collect()
}

/// Shortest text that reads back as the same float (as in `ferrodb-data`).
pub(crate) fn float_text(f: f64) -> String {
    if f.is_finite() && f.fract() == 0.0 && f.abs() < 1e15 {
        format!("{f:.1}").trim_end_matches(".0").to_owned()
    } else {
        f.to_string()
    }
}

/// Display text: `()` → empty, floats in their shortest form, arrays and
/// maps as JSON.
pub(crate) fn text(v: &Dynamic) -> String {
    if v.is_unit() {
        String::new()
    } else if let Ok(f) = v.as_float() {
        float_text(f)
    } else if v.is_string() {
        v.clone().into_string().unwrap_or_default()
    } else if v.is_array() || v.is_map() {
        json(v)
    } else {
        v.to_string()
    }
}

fn json_into(v: &Dynamic, out: &mut String) {
    if v.is_unit() {
        out.push_str("null");
    } else if let Ok(b) = v.as_bool() {
        out.push_str(if b { "true" } else { "false" });
    } else if let Ok(i) = v.as_int() {
        out.push_str(&i.to_string());
    } else if let Ok(f) = v.as_float() {
        if f.is_finite() {
            out.push_str(&float_text(f));
        } else {
            out.push_str("null");
        }
    } else if let Some(a) = v.read_lock::<Array>() {
        out.push('[');
        for (i, x) in a.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            json_into(x, out);
        }
        out.push(']');
    } else if let Some(m) = v.read_lock::<Map>() {
        out.push('{');
        for (i, (k, x)) in m.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str(&serde_json::Value::String(k.to_string()).to_string());
            out.push(':');
            json_into(x, out);
        }
        out.push('}');
    } else {
        out.push_str(&serde_json::Value::String(text(v)).to_string());
    }
}

pub(crate) fn json(v: &Dynamic) -> String {
    let mut s = String::new();
    json_into(v, &mut s);
    s
}

/// Aggregator results and extractor chunks as text.
pub(crate) fn result_text(v: Dynamic) -> String {
    text(&v)
}

fn sql_literal(v: &Dynamic) -> String {
    if v.is_unit() {
        "NULL".into()
    } else if let Ok(b) = v.as_bool() {
        if b { "TRUE" } else { "FALSE" }.into()
    } else if let Ok(i) = v.as_int() {
        i.to_string()
    } else if let Ok(f) = v.as_float() {
        if f.is_finite() {
            float_text(f)
        } else {
            "NULL".into()
        }
    } else {
        format!("'{}'", text(v).replace('\'', "''"))
    }
}

fn sql_ident(s: &str) -> String {
    let simple = s
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if simple {
        s.to_owned()
    } else {
        format!("\"{}\"", s.replace('"', "\"\""))
    }
}

fn csv_field(v: &Dynamic, delimiter: &str) -> String {
    let s = text(v);
    if s.contains(delimiter) || s.contains(['"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s
    }
}

fn html_escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&#39;"),
            c => o.push(c),
        }
    }
    o
}

fn md_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('|', "\\|")
        .replace("\r\n", "<br>")
        .replace('\n', "<br>")
}

/// Numbers and numeric text as a number, else `()`.
fn to_number(v: &Dynamic) -> Dynamic {
    if v.is_int() || v.is_float() {
        return v.clone();
    }
    if v.is_string() {
        let s = text(v);
        let t = s.trim();
        if let Ok(i) = t.parse::<i64>() {
            return Dynamic::from_int(i);
        }
        if let Ok(f) = t.parse::<f64>()
            && f.is_finite()
        {
            return Dynamic::from_float(f);
        }
    }
    Dynamic::UNIT
}

fn as_f64(v: &Dynamic) -> Option<f64> {
    let n = to_number(v);
    n.as_int()
        .map(|i| i as f64)
        .ok()
        .or_else(|| n.as_float().ok())
}

fn pad(v: &Dynamic, width: i64, left: bool) -> String {
    let s = text(v);
    let n = s.chars().count();
    let width = usize::try_from(width).unwrap_or(0).min(MAX_STRING);
    if n >= width {
        return s;
    }
    let fill = " ".repeat(width - n);
    if left {
        fill + s.as_str()
    } else {
        s + fill.as_str()
    }
}

fn format_number(x: f64, decimals: i64) -> String {
    let d = usize::try_from(decimals.clamp(0, 20)).unwrap_or(0);
    format!("{x:.d$}")
}

fn register_helpers(e: &mut Engine) {
    e.register_fn("text", |v: Dynamic| text(&v));
    e.register_fn("text", |v: Dynamic, null: ImmutableString| {
        if v.is_unit() {
            null.to_string()
        } else {
            text(&v)
        }
    });
    e.register_fn("sql_literal", |v: Dynamic| sql_literal(&v));
    e.register_fn("sql_ident", |v: Dynamic| sql_ident(&text(&v)));
    e.register_fn("json", |v: Dynamic| json(&v));
    e.register_fn("csv_field", |v: Dynamic| csv_field(&v, ","));
    e.register_fn("csv_field", |v: Dynamic, d: ImmutableString| {
        csv_field(&v, if d.is_empty() { "," } else { d.as_str() })
    });
    e.register_fn("html_escape", |v: Dynamic| html_escape(&text(&v)));
    e.register_fn("md_escape", |v: Dynamic| md_escape(&text(&v)));
    e.register_fn("is_null", |v: Dynamic| v.is_unit());
    e.register_fn("is_number", |v: Dynamic| v.is_int() || v.is_float());
    e.register_fn("to_number", |v: Dynamic| to_number(&v));
    e.register_fn("numbers", |values: Array| -> Array {
        values
            .iter()
            .filter_map(as_f64)
            .map(Dynamic::from_float)
            .collect()
    });
    e.register_fn("join", |values: Array, sep: ImmutableString| {
        let parts: Vec<String> = values.iter().map(text).collect();
        parts.join(sep.as_str())
    });
    e.register_fn("pad", |v: Dynamic, w: i64| pad(&v, w, false));
    e.register_fn("pad_left", |v: Dynamic, w: i64| pad(&v, w, true));
    e.register_fn("format_number", |x: f64, d: i64| format_number(x, d));
    e.register_fn("format_number", |x: i64, d: i64| format_number(x as f64, d));
    e.register_fn("round_to", |x: f64, d: i64| {
        let m = 10f64.powi(d.clamp(0, 15) as i32);
        (x * m).round() / m
    });
    e.register_fn("round_to", |x: i64, _d: i64| x);
    e.register_fn("num_text", float_text);
    e.register_fn("num_text", |x: i64| x.to_string());
}

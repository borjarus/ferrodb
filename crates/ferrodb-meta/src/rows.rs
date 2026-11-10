//! Reading catalog query results: every value as text (or `None` for NULL),
//! columns looked up by lower-case name. Catalog queries only return text,
//! numbers and booleans, so this covers what drivers produce for them.

use arrow_array::{
    Array, ArrayRef, BinaryArray, BooleanArray, Decimal128Array, Float32Array, Float64Array,
    Int8Array, Int16Array, Int32Array, Int64Array, LargeBinaryArray, LargeStringArray, StringArray,
    UInt8Array, UInt16Array, UInt32Array, UInt64Array,
};
use arrow_schema::DataType;
use ferrodb_api::{Error, Result};
use ferrodb_drivers::{
    CancellationToken, Connection,
    sink::{Collected, collect},
};

/// Separator used by catalog queries that aggregate names into one value.
pub const SEP: char = '\u{1f}';

/// One row of a catalog query.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Row {
    names: std::sync::Arc<Vec<String>>,
    values: Vec<Option<String>>,
}

impl Row {
    pub fn get(&self, name: &str) -> Option<&str> {
        let i = self.names.iter().position(|n| n == name)?;
        self.values[i].as_deref()
    }

    /// Text value, `None` for NULL or empty.
    pub fn text(&self, name: &str) -> Option<String> {
        self.get(name).filter(|s| !s.is_empty()).map(str::to_owned)
    }

    pub fn string(&self, name: &str) -> String {
        self.get(name).unwrap_or_default().to_owned()
    }

    pub fn int(&self, name: &str) -> Option<i64> {
        let v = self.get(name)?.trim();
        v.parse::<i64>()
            .ok()
            .or_else(|| v.parse::<f64>().ok().map(|f| f as i64))
    }

    pub fn bool(&self, name: &str) -> bool {
        matches!(
            self.get(name).map(|s| s.trim().to_ascii_lowercase()),
            Some(ref s) if s == "true" || s == "t" || s == "1" || s == "yes" || s == "y"
        )
    }

    /// A value aggregated with [`SEP`] (empty = no items).
    pub fn list(&self, name: &str) -> Vec<String> {
        match self.get(name) {
            Some(s) if !s.is_empty() => s.split(SEP).map(str::to_owned).collect(),
            _ => Vec::new(),
        }
    }
}

fn cell(a: &ArrayRef, i: usize) -> Option<String> {
    if a.is_null(i) {
        return None;
    }
    let any = a.as_any();
    macro_rules! try_as {
        ($($t:ty),*) => {$(
            if let Some(x) = any.downcast_ref::<$t>() {
                return Some(x.value(i).to_string());
            }
        )*};
    }
    try_as!(
        StringArray,
        LargeStringArray,
        Int8Array,
        Int16Array,
        Int32Array,
        Int64Array,
        UInt8Array,
        UInt16Array,
        UInt32Array,
        UInt64Array,
        Float32Array,
        Float64Array,
        BooleanArray
    );
    if let Some(x) = any.downcast_ref::<BinaryArray>() {
        return Some(String::from_utf8_lossy(x.value(i)).into_owned());
    }
    if let Some(x) = any.downcast_ref::<LargeBinaryArray>() {
        return Some(String::from_utf8_lossy(x.value(i)).into_owned());
    }
    if let (Some(x), DataType::Decimal128(_, scale)) =
        (any.downcast_ref::<Decimal128Array>(), a.data_type())
    {
        let v = x.value(i);
        let scale = *scale as u32;
        if scale == 0 {
            return Some(v.to_string());
        }
        let p = 10i128.pow(scale);
        let sign = if v < 0 { "-" } else { "" };
        let v = v.unsigned_abs();
        return Some(format!(
            "{sign}{}.{:0w$}",
            v / p as u128,
            v % p as u128,
            w = scale as usize
        ));
    }
    None
}

/// Rows of every result set (catalog queries return one).
pub fn rows(results: &[Collected]) -> Vec<Row> {
    let mut out = Vec::new();
    for r in results {
        let names = std::sync::Arc::new(
            r.schema
                .fields()
                .iter()
                .map(|f| f.name().to_ascii_lowercase())
                .collect::<Vec<_>>(),
        );
        for b in &r.batches {
            for i in 0..b.num_rows() {
                out.push(Row {
                    names: names.clone(),
                    values: b.columns().iter().map(|c| cell(c, i)).collect(),
                });
            }
        }
    }
    out
}

/// Runs a catalog query and returns its rows.
pub async fn query(conn: &mut dyn Connection, sql: impl Into<String>) -> Result<Vec<Row>> {
    let sql = sql.into();
    let out = collect(conn, sql.clone(), CancellationToken::new())
        .await
        .map_err(|e| match e {
            Error::Query(mut q) => {
                q.message = format!("{} (introspection)", q.message);
                Error::Query(q)
            }
            other => other,
        })?;
    Ok(rows(&out.results))
}

#[cfg(test)]
pub fn row(pairs: &[(&str, Option<&str>)]) -> Row {
    Row {
        names: std::sync::Arc::new(pairs.iter().map(|(k, _)| k.to_string()).collect()),
        values: pairs.iter().map(|(_, v)| v.map(str::to_owned)).collect(),
    }
}

#![allow(dead_code)]
use rayon::prelude::*;
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

pub const FLOAT_EPSILON: f64 = 1e-6;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JsonlReadReport {
    pub path: String,
    pub rows_seen: usize,
    pub rows_ok: usize,
    pub rows_skipped_empty: usize,
    pub rows_skipped_parse_error: usize,
}

pub fn f64v(v: Option<&Value>, default: f64) -> f64 {
    match v {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(default),
        Some(Value::String(s)) if !s.is_empty() => s.parse().unwrap_or(default),
        _ => default,
    }
}

pub fn i64v(v: Option<&Value>, default: i64) -> i64 {
    match v {
        Some(Value::Number(n)) => n
            .as_i64()
            .or_else(|| n.as_f64().map(|x| x as i64))
            .unwrap_or(default),
        Some(Value::String(s)) if !s.is_empty() => {
            s.parse::<f64>().map(|x| x as i64).unwrap_or(default)
        }
        _ => default,
    }
}

pub fn boolv(v: Option<&Value>) -> bool {
    match v {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => s == "true" || s == "1",
        Some(Value::Number(n)) => n.as_i64().unwrap_or(0) != 0,
        _ => false,
    }
}

pub fn strv<'a>(row: &'a Value, key: &str) -> &'a str {
    row.get(key).and_then(|v| v.as_str()).unwrap_or("")
}

pub fn approx_eq(a: f64, b: f64) -> bool {
    (a - b).abs() <= FLOAT_EPSILON
}

pub fn read_jsonl(path: &str) -> Vec<Value> {
    read_jsonl_with_report(path).0
}

pub fn read_jsonl_with_report(path: &str) -> (Vec<Value>, JsonlReadReport) {
    let mut report = JsonlReadReport {
        path: path.to_string(),
        ..Default::default()
    };
    let p = Path::new(path);
    if !p.exists() {
        return (vec![], report);
    }
    let Ok(file) = File::open(p) else {
        return (vec![], report);
    };
    let mut out = Vec::new();
    for (idx, line) in BufReader::new(file).lines().enumerate() {
        report.rows_seen += 1;
        let Ok(line) = line else {
            report.rows_skipped_parse_error += 1;
            eprintln!(
                "[analytics] jsonl read failed path={} row={}",
                path,
                idx + 1
            );
            continue;
        };
        let line = line.trim();
        if line.is_empty() {
            report.rows_skipped_empty += 1;
            continue;
        }
        match serde_json::from_str::<Value>(line) {
            Ok(v) => {
                report.rows_ok += 1;
                out.push(v);
            }
            Err(e) => {
                report.rows_skipped_parse_error += 1;
                eprintln!(
                    "[analytics] jsonl parse failed path={} row={} err={}; skipping",
                    path,
                    idx + 1,
                    e
                );
            }
        }
    }
    (out, report)
}

pub fn process_jsonl_stream<T, F>(path: &str, mut handler: F) -> JsonlReadReport
where
    T: DeserializeOwned,
    F: FnMut(T),
{
    let mut report = JsonlReadReport {
        path: path.to_string(),
        ..Default::default()
    };
    let p = Path::new(path);
    if !p.exists() {
        return report;
    }
    let Ok(file) = File::open(p) else {
        return report;
    };
    for (idx, line) in BufReader::new(file).lines().enumerate() {
        report.rows_seen += 1;
        let Ok(line) = line else {
            report.rows_skipped_parse_error += 1;
            eprintln!(
                "[analytics] jsonl read failed path={} row={}",
                path,
                idx + 1
            );
            continue;
        };
        let line = line.trim();
        if line.is_empty() {
            report.rows_skipped_empty += 1;
            continue;
        }
        match serde_json::from_str::<T>(line) {
            Ok(record) => {
                report.rows_ok += 1;
                handler(record);
            }
            Err(e) => {
                report.rows_skipped_parse_error += 1;
                eprintln!(
                    "[analytics] jsonl parse failed path={} row={} err={}; skipping",
                    path,
                    idx + 1,
                    e
                );
            }
        }
    }
    report
}

pub fn process_jsonl_parallel<T>(raw_file_content: &str) -> Vec<T>
where
    T: DeserializeOwned + Send,
{
    raw_file_content
        .par_lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() {
                None
            } else {
                serde_json::from_str::<T>(line).ok()
            }
        })
        .collect()
}

pub fn write_jsonl(path: &str, rows: &[Value]) -> anyhow::Result<()> {
    if let Some(parent) = Path::new(path).parent() {
        fs::create_dir_all(parent)?;
    }
    let mut f = File::create(path)?;
    for row in rows {
        writeln!(f, "{}", serde_json::to_string(row)?)?;
    }
    Ok(())
}

pub fn write_json(path: &str, row: &Value) -> anyhow::Result<()> {
    if let Some(parent) = Path::new(path).parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, serde_json::to_string_pretty(row)? + "\n")?;
    Ok(())
}

pub fn latest_by_mint(rows: &[Value]) -> HashMap<String, Value> {
    let mut out = HashMap::new();
    for r in rows {
        if let Some(mint) = r.get("mint").and_then(|v| v.as_str()) {
            if !mint.is_empty() {
                out.insert(mint.to_string(), r.clone());
            }
        }
    }
    out
}

pub fn group_by_mint(rows: &[Value]) -> HashMap<String, Vec<Value>> {
    let mut out: HashMap<String, Vec<Value>> = HashMap::new();
    for r in rows {
        if let Some(mint) = r.get("mint").and_then(|v| v.as_str()) {
            if !mint.is_empty() {
                out.entry(mint.to_string()).or_default().push(r.clone());
            }
        }
    }
    out
}

pub fn risk_bucket(v: f64) -> &'static str {
    if v < 20.0 {
        "00-20"
    } else if v < 40.0 {
        "20-40"
    } else if v < 60.0 {
        "40-60"
    } else if v < 80.0 {
        "60-80"
    } else {
        "80-100"
    }
}

pub fn counter_json<I>(items: I) -> Value
where
    I: IntoIterator<Item = String>,
{
    let mut counts: HashMap<String, usize> = HashMap::new();
    for item in items {
        *counts.entry(item).or_insert(0) += 1;
    }
    json!(counts)
}

pub fn arg_value(args: &[String], name: &str, default: &str) -> String {
    args.windows(2)
        .find_map(|w| {
            if w[0] == name {
                Some(w[1].clone())
            } else {
                None
            }
        })
        .unwrap_or_else(|| default.to_string())
}

pub fn has_flag(args: &[String], name: &str) -> bool {
    args.iter().any(|a| a == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approx_float_uses_epsilon() {
        assert!(approx_eq(0.3, 0.3000004));
        assert!(!approx_eq(0.3, 0.30001));
    }

    #[test]
    fn parallel_jsonl_skips_bad_rows() {
        let rows: Vec<Value> = process_jsonl_parallel("{\"a\":1}\nBAD\n{\"a\":2}\n");
        assert_eq!(rows.len(), 2);
    }
}

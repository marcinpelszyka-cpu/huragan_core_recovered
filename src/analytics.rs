#![allow(dead_code)]
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

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

pub fn read_jsonl(path: &str) -> Vec<Value> {
    let p = Path::new(path);
    if !p.exists() {
        return vec![];
    }
    let Ok(file) = File::open(p) else {
        return vec![];
    };
    BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() {
                None
            } else {
                serde_json::from_str::<Value>(line).ok()
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

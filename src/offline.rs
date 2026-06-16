use crate::record::{normalize, Record};
use serde_json::{Map, Value};
use std::fs;
use std::path::{Path, PathBuf};

fn downloads() -> PathBuf {
    dirs::home_dir().unwrap_or_default().join("Downloads")
}

fn newest(paths: Vec<PathBuf>) -> Option<PathBuf> {
    paths
        .into_iter()
        .filter_map(|p| {
            let m = fs::metadata(&p).ok()?.modified().ok()?;
            Some((m, p))
        })
        .max_by_key(|(m, _)| *m)
        .map(|(_, p)| p)
}

pub fn resolve(arg: Option<&str>) -> PathBuf {
    if let Some(a) = arg {
        if Path::new(a).is_file() {
            return PathBuf::from(a);
        }
        if a.contains('*') || a.contains('?') {
            if let Ok(g) = glob::glob(a) {
                let hits: Vec<PathBuf> = g.filter_map(Result::ok).collect();
                if let Some(p) = newest(hits) {
                    return p;
                }
            }
        }
        let in_dl = downloads().join(a);
        if in_dl.is_file() {
            return in_dl;
        }
    }
    let mut hits = Vec::new();
    for pat in ["Logs-*.json", "Explore-logs-*.json"] {
        let full = downloads().join(pat);
        if let Ok(g) = glob::glob(&full.to_string_lossy()) {
            hits.extend(g.filter_map(Result::ok));
        }
    }
    match newest(hits) {
        Some(p) => p,
        None => crate::die("no log file found (pass a path or put a Grafana JSON export in ~/Downloads)"),
    }
}

pub fn load_stdin() -> Vec<Record> {
    use std::io::Read;
    let mut text = String::new();
    if let Err(e) = std::io::stdin().read_to_string(&mut text) {
        crate::die(&format!("cannot read stdin: {e}"));
    }
    load_text(&text, "stdin")
}

pub fn load(path: &Path) -> Vec<Record> {
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => crate::die(&format!("cannot read {}: {e}", path.display())),
    };
    load_text(&text, &path.display().to_string())
}

fn load_text(text: &str, src: &str) -> Vec<Record> {
    let data: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(e) => crate::die(&format!("invalid JSON in {src}: {e}")),
    };
    let list: Vec<Value> = match data {
        Value::Array(a) => a,
        Value::Object(ref o) => o
            .get("data")
            .or_else(|| o.get("logs"))
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_else(|| vec![data.clone()]),
        other => vec![other],
    };
    list.iter().map(rec_from_value).collect()
}

fn rec_from_value(v: &Value) -> Record {
    let empty = Map::new();
    let obj = v.as_object().unwrap_or(&empty);
    let line = obj.get("line").and_then(Value::as_str).unwrap_or("");
    let date = obj.get("date").and_then(Value::as_str).unwrap_or("");
    let fields = obj.get("fields").and_then(Value::as_object).unwrap_or(&empty);
    normalize(line, date, fields)
}

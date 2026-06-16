use chrono::NaiveDateTime;
use regex::Regex;
use serde::Serialize;
use serde_json::{Map, Value};
use std::sync::OnceLock;

#[derive(Clone, Serialize)]
pub struct Record {
    pub ts: String,
    pub level: String,
    pub service: String,
    pub pod: String,
    pub namespace: String,
    pub logger: String,
    pub thread: String,
    pub trace: String,
    pub msg: String,
    pub stack: String,
}

fn get<'a>(m: &'a Map<String, Value>, k: &str) -> Option<&'a str> {
    m.get(k).and_then(Value::as_str).filter(|s| !s.is_empty())
}

fn first(opts: &[Option<&str>]) -> String {
    opts.iter().find_map(|o| *o).unwrap_or("").to_string()
}

pub fn normalize(line: &str, date: &str, fields: &Map<String, Value>) -> Record {
    let o: Map<String, Value> = if line.trim_start().starts_with('{') {
        serde_json::from_str(line).unwrap_or_default()
    } else {
        Map::new()
    };
    let o = &o;

    let msg = get(o, "message").map(str::to_string).unwrap_or_else(|| line.to_string());
    let level = first(&[get(o, "level"), get(fields, "level"), get(fields, "detected_level")]);
    let level = if level.is_empty() { "?".into() } else { level.to_uppercase() };
    let service = first(&[get(fields, "service_name"), get(fields, "app"), get(o, "application_name")]);
    let service = if service.is_empty() { "?".into() } else { service };

    Record {
        ts: first(&[(!date.is_empty()).then_some(date), get(o, "@timestamp")]),
        level,
        service,
        pod: get(fields, "pod").unwrap_or("").to_string(),
        namespace: get(fields, "namespace").unwrap_or("").to_string(),
        logger: get(o, "logger_name").unwrap_or("").to_string(),
        thread: get(o, "thread_name").unwrap_or("").to_string(),
        trace: first(&[get(o, "traceId"), get(o, "trace_id"), get(fields, "TraceID")]),
        msg,
        stack: get(o, "stack_trace").unwrap_or("").to_string(),
    }
}

pub fn short_ts(ts: &str) -> String {
    if ts.is_empty() {
        return "?".to_string();
    }
    let s: String = ts.replace('T', " ").replace('Z', "");
    s.chars().take(23).collect()
}

pub fn oneline(s: &str, n: usize) -> String {
    static WS: OnceLock<Regex> = OnceLock::new();
    let ws = WS.get_or_init(|| Regex::new(r"\s+").unwrap());
    let s = ws.replace_all(s.trim(), " ");
    if s.chars().count() <= n {
        s.into_owned()
    } else {
        let mut out: String = s.chars().take(n - 1).collect();
        out.push('…');
        out
    }
}

pub fn sig(msg: &str) -> String {
    static HEX: OnceLock<Regex> = OnceLock::new();
    static NUM: OnceLock<Regex> = OnceLock::new();
    static WS: OnceLock<Regex> = OnceLock::new();
    let hex = HEX.get_or_init(|| Regex::new(r"[0-9a-f]{8,}").unwrap());
    let num = NUM.get_or_init(|| Regex::new(r"\d+").unwrap());
    let ws = WS.get_or_init(|| Regex::new(r"\s+").unwrap());
    let s = msg.to_lowercase();
    let s = hex.replace_all(&s, "<hex>");
    let s = num.replace_all(&s, "<n>");
    let s = ws.replace_all(s.trim(), " ");
    s.chars().take(120).collect()
}

pub fn last_segment(s: &str) -> &str {
    s.rsplit('.').next().unwrap_or(s)
}

pub fn ts_epoch_ms(ts: &str) -> Option<i64> {
    let t = ts.trim().trim_end_matches('Z');
    NaiveDateTime::parse_from_str(t, "%Y-%m-%dT%H:%M:%S%.f")
        .or_else(|_| NaiveDateTime::parse_from_str(t, "%Y-%m-%dT%H:%M:%S"))
        .ok()
        .map(|d| d.and_utc().timestamp_millis())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields(pairs: &[(&str, &str)]) -> Map<String, Value> {
        pairs.iter().map(|(k, v)| (k.to_string(), Value::String(v.to_string()))).collect()
    }

    #[test]
    fn normalize_json_line() {
        let line = r#"{"message":"boom","level":"error","logger_name":"a.b.C","traceId":"t1","stack_trace":"x"}"#;
        let f = fields(&[("service_name", "svc"), ("pod", "p1"), ("namespace", "prod")]);
        let r = normalize(line, "2026-06-04T09:00:00.000Z", &f);
        assert_eq!(r.msg, "boom");
        assert_eq!(r.level, "ERROR");
        assert_eq!(r.service, "svc");
        assert_eq!(r.logger, "a.b.C");
        assert_eq!(r.trace, "t1");
        assert_eq!(r.stack, "x");
        assert_eq!(r.ts, "2026-06-04T09:00:00.000Z");
    }

    #[test]
    fn normalize_plain_line_uses_field_level_and_app() {
        let f = fields(&[("app", "legacy"), ("detected_level", "info")]);
        let r = normalize("plain text", "2026-01-01T00:00:00Z", &f);
        assert_eq!(r.msg, "plain text");
        assert_eq!(r.level, "INFO");
        assert_eq!(r.service, "legacy");
    }

    #[test]
    fn normalize_missing_everything_is_question_marks() {
        let r = normalize("hi", "", &Map::new());
        assert_eq!(r.level, "?");
        assert_eq!(r.service, "?");
        assert_eq!(r.ts, "");
    }

    #[test]
    fn normalize_label_trace_and_timestamp_fallback() {
        let line = r#"{"@timestamp":"2026-02-02T02:02:02Z","message":"m"}"#;
        let f = fields(&[("TraceID", "abc"), ("service_name", "s")]);
        let r = normalize(line, "", &f);
        assert_eq!(r.trace, "abc");
        assert_eq!(r.ts, "2026-02-02T02:02:02Z");
    }

    #[test]
    fn sig_normalizes_numbers_and_hex() {
        assert_eq!(sig("user 12345 at deadbeefcafe"), "user <n> at <hex>");
        assert_eq!(sig("Fixture[id=99] seq=1"), "fixture[id=<n>] seq=<n>");
    }

    #[test]
    fn oneline_collapses_and_truncates() {
        assert_eq!(oneline("  a\n\t b  ", 80), "a b");
        let out = oneline("abcdefghij", 5);
        assert_eq!(out.chars().count(), 5);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn ts_epoch_ms_ordering_fraction_and_invalid() {
        let a = ts_epoch_ms("2026-06-04T09:00:01.000Z").unwrap();
        let b = ts_epoch_ms("2026-06-04T09:00:02.000Z").unwrap();
        assert_eq!(b - a, 1000);
        assert_eq!(ts_epoch_ms("2026-06-04T09:00:01.123Z").unwrap(), a + 123);
        assert_eq!(ts_epoch_ms("2026-06-04T09:00:05").unwrap(), a + 4000);
        assert_eq!(ts_epoch_ms("not-a-time"), None);
    }

    #[test]
    fn short_ts_and_last_segment() {
        assert_eq!(short_ts("2026-06-04T09:00:01.123Z"), "2026-06-04 09:00:01.123");
        assert_eq!(short_ts(""), "?");
        assert_eq!(last_segment("com.bp.Foo"), "Foo");
        assert_eq!(last_segment("Foo"), "Foo");
    }
}

use crate::cli::{Filter, View};
use crate::record::{Record, last_segment, oneline, short_ts, sig, ts_epoch_ms};
use regex::RegexBuilder;
use serde_json::{Value, json};
use std::collections::HashMap;

pub fn print_json(v: &Value) {
    println!(
        "{}",
        serde_json::to_string_pretty(v).unwrap_or_else(|_| "null".into())
    );
}

pub fn apply_filter(mut recs: Vec<Record>, f: &Filter) -> Vec<Record> {
    if let Some(s) = &f.service {
        let s = s.to_lowercase();
        recs.retain(|r| r.service.to_lowercase().contains(&s));
    }
    if let Some(s) = &f.logger {
        let s = s.to_lowercase();
        recs.retain(|r| r.logger.to_lowercase().contains(&s));
    }
    if let Some(s) = &f.match_ {
        let s = s.to_lowercase();
        recs.retain(|r| r.msg.to_lowercase().contains(&s) || r.stack.to_lowercase().contains(&s));
    }
    if let Some(a) = &f.after {
        recs.retain(|r| !r.ts.is_empty() && r.ts.as_str() >= a.as_str());
    }
    if let Some(b) = &f.before {
        recs.retain(|r| !r.ts.is_empty() && r.ts.as_str() <= b.as_str());
    }
    recs
}

fn count_by<'a>(recs: &'a [Record], key: impl Fn(&'a Record) -> &'a str) -> Vec<(&'a str, usize)> {
    let mut idx: HashMap<&str, usize> = HashMap::new();
    let mut out: Vec<(&str, usize)> = Vec::new();
    for r in recs {
        let k = key(r);
        match idx.get(k) {
            Some(&i) => out[i].1 += 1,
            None => {
                idx.insert(k, out.len());
                out.push((k, 1));
            }
        }
    }
    out.sort_by_key(|b| std::cmp::Reverse(b.1));
    out
}

pub fn dispatch(recs: &[Record], view: &View, json_out: bool, full: bool) {
    match view {
        View::Summary => summary(recs, json_out, full),
        View::Errors { warn, level } => errors(recs, *warn, level.as_deref(), json_out, full),
        View::Grep {
            pattern,
            ignore_case,
            limit,
        } => grep(recs, pattern, *ignore_case, *limit, json_out, full),
        View::Trace { trace_id } => trace(recs, trace_id, json_out, full),
        View::Show { index } => show(recs, *index, json_out),
        View::Patterns { limit, level } => patterns(recs, *limit, level.as_deref(), json_out, full),
        View::Timeline { buckets } => timeline(recs, *buckets, json_out),
    }
}

fn body(msg: &str, full: bool, width: usize) -> String {
    if full {
        msg.trim_end().to_string()
    } else {
        oneline(msg, width)
    }
}

fn record_json(index: Option<usize>, r: &Record) -> Value {
    let mut o = json!({
        "ts": r.ts, "level": r.level, "service": r.service, "pod": r.pod,
        "namespace": r.namespace, "logger": r.logger, "thread": r.thread,
        "trace": r.trace, "msg": r.msg, "stack": r.stack,
    });
    if let Some(i) = index
        && let Some(m) = o.as_object_mut()
    {
        m.insert("index".into(), json!(i));
    }
    o
}

fn cluster_errors(recs: &[Record]) -> (usize, Vec<(String, Vec<&Record>)>) {
    let errs: Vec<&Record> = recs
        .iter()
        .filter(|r| matches!(r.level.as_str(), "ERROR" | "WARN" | "FATAL"))
        .collect();
    let total = errs.len();
    let mut idx: HashMap<String, usize> = HashMap::new();
    let mut clusters: Vec<(String, Vec<&Record>)> = Vec::new();
    for r in errs {
        let s = sig(&r.msg);
        match idx.get(&s) {
            Some(&i) => clusters[i].1.push(r),
            None => {
                idx.insert(s.clone(), clusters.len());
                clusters.push((s, vec![r]));
            }
        }
    }
    clusters.sort_by_key(|b| std::cmp::Reverse(b.1.len()));
    (total, clusters)
}

fn summary(recs: &[Record], json_out: bool, full: bool) {
    let ts: Vec<&str> = recs
        .iter()
        .map(|r| r.ts.as_str())
        .filter(|s| !s.is_empty())
        .collect();
    let range = (!ts.is_empty()).then(|| (*ts.iter().min().unwrap(), *ts.iter().max().unwrap()));
    let (errtotal, clusters) = cluster_errors(recs);

    if json_out {
        let pack = |v: Vec<(&str, usize)>, n: usize| {
            v.into_iter()
                .take(n)
                .map(|(k, c)| json!({"name": k, "count": c}))
                .collect::<Vec<_>>()
        };
        let patterns: Vec<Value> = clusters
            .iter()
            .take(15)
            .map(|(s, rs)| json!({"count": rs.len(), "level": rs[0].level, "sig": s, "sample": rs[0].msg}))
            .collect();
        print_json(&json!({
            "records": recs.len(),
            "range": range.map(|(a, b)| json!({"start": short_ts(a), "end": short_ts(b)})),
            "levels": pack(count_by(recs, |r| &r.level), usize::MAX),
            "services": pack(count_by(recs, |r| &r.service), 15),
            "loggers": pack(count_by(recs, |r| &r.logger).into_iter().filter(|(v, _)| !v.is_empty()).collect(), 10),
            "error_warn_total": errtotal,
            "patterns": patterns,
        }));
        return;
    }

    if recs.is_empty() {
        println!("empty");
        return;
    }
    println!("records: {}", recs.len());
    if let Some((lo, hi)) = range {
        println!("range:   {}  ->  {}", short_ts(lo), short_ts(hi));
    }
    println!("\nlevels:");
    for (v, c) in count_by(recs, |r| &r.level) {
        println!("  {c:>6}  {v}");
    }
    println!("\nservices:");
    for (v, c) in count_by(recs, |r| &r.service).into_iter().take(15) {
        println!("  {c:>6}  {v}");
    }
    let loggers = count_by(recs, |r| &r.logger);
    if loggers.iter().any(|(v, _)| !v.is_empty()) {
        println!("\ntop loggers:");
        for (v, c) in loggers.into_iter().take(10) {
            if !v.is_empty() {
                println!("  {c:>6}  {v}");
            }
        }
    }
    if errtotal > 0 {
        println!("\ntop error/warn patterns ({errtotal} total):");
        for (_, rs) in clusters.into_iter().take(15) {
            println!(
                "  {:>5}x [{}] {}",
                rs.len(),
                rs[0].level,
                body(&rs[0].msg, full, 120)
            );
        }
    }
}

fn level_set(warn: bool, level: Option<&str>) -> Vec<String> {
    if let Some(l) = level {
        l.split(',').map(|s| s.trim().to_uppercase()).collect()
    } else if warn {
        vec!["ERROR".into(), "FATAL".into(), "WARN".into()]
    } else {
        vec!["ERROR".into(), "FATAL".into()]
    }
}

fn errors(recs: &[Record], warn: bool, level: Option<&str>, json_out: bool, full: bool) {
    let mut levels = level_set(warn, level);
    levels.sort();
    let matched: Vec<(usize, &Record)> = recs
        .iter()
        .enumerate()
        .filter(|(_, r)| levels.iter().any(|l| l == &r.level))
        .collect();

    if json_out {
        let arr: Vec<Value> = matched.iter().map(|(i, r)| record_json(Some(*i), r)).collect();
        print_json(&json!({"levels": levels, "count": arr.len(), "records": arr}));
        return;
    }
    let pretty = levels
        .iter()
        .map(|l| format!("'{l}'"))
        .collect::<Vec<_>>()
        .join(", ");
    println!("{} record(s) at [{pretty}]\n", matched.len());
    for (i, r) in matched {
        let mark = if r.stack.is_empty() { "" } else { " +stack" };
        println!(
            "[{i}] {} {} {} {}{mark}",
            short_ts(&r.ts),
            r.level,
            r.service,
            last_segment(&r.logger)
        );
        println!("     {}", body(&r.msg, full, 160));
        if full && !r.stack.is_empty() {
            println!("     STACK: {}", r.stack.trim_end());
        }
    }
}

fn grep(recs: &[Record], pattern: &str, ignore_case: bool, limit: usize, json_out: bool, full: bool) {
    let rx = match RegexBuilder::new(pattern)
        .case_insensitive(ignore_case)
        .build()
    {
        Ok(r) => r,
        Err(e) => crate::die(&format!("bad regex: {e}")),
    };
    let mut hits: Vec<(usize, &Record)> = Vec::new();
    let mut truncated = false;
    for (i, r) in recs.iter().enumerate() {
        if rx.is_match(&format!("{}\n{}", r.msg, r.stack)) {
            hits.push((i, r));
            if hits.len() >= limit {
                truncated = true;
                break;
            }
        }
    }
    if json_out {
        let arr: Vec<Value> = hits.iter().map(|(i, r)| record_json(Some(*i), r)).collect();
        print_json(
            &json!({"pattern": pattern, "count": arr.len(), "truncated": truncated, "records": arr}),
        );
        return;
    }
    if hits.is_empty() {
        println!("no match");
        return;
    }
    for (i, r) in &hits {
        let tr: String = r.trace.chars().take(16).collect();
        println!(
            "[{i}] {} {} {} trace={tr}",
            short_ts(&r.ts),
            r.level,
            r.service
        );
        println!("     {}", body(&r.msg, full, 160));
        if full && !r.stack.is_empty() {
            println!("     STACK: {}", r.stack.trim_end());
        }
    }
    if truncated {
        println!("... stopped at {limit} (use --limit)");
    }
}

fn trace(recs: &[Record], trace_id: &str, json_out: bool, full: bool) {
    let mut out: Vec<&Record> = recs
        .iter()
        .filter(|r| !r.trace.is_empty() && r.trace.starts_with(trace_id))
        .collect();
    out.sort_by(|a, b| a.ts.cmp(&b.ts));
    if json_out {
        let arr: Vec<Value> = out.iter().map(|r| record_json(None, r)).collect();
        print_json(&json!({"trace": trace_id, "count": arr.len(), "records": arr}));
        return;
    }
    println!("{} record(s) for trace {trace_id}\n", out.len());
    for r in out {
        println!(
            "{} {} {} {}",
            short_ts(&r.ts),
            r.level,
            r.service,
            last_segment(&r.logger)
        );
        println!("  {}", body(&r.msg, full, 160));
        if !r.stack.is_empty() {
            let stack = if full {
                r.stack.trim_end().to_string()
            } else {
                oneline(&r.stack, 200)
            };
            println!("  STACK: {stack}");
        }
    }
}

fn show(recs: &[Record], index: usize, json_out: bool) {
    let Some(r) = recs.get(index) else {
        crate::die(&format!("index {index} out of range (0..{})", recs.len()));
    };
    if json_out {
        print_json(&serde_json::to_value(r).unwrap_or(Value::Null));
        return;
    }
    for (k, v) in [
        ("ts", &r.ts),
        ("level", &r.level),
        ("service", &r.service),
        ("pod", &r.pod),
        ("namespace", &r.namespace),
        ("logger", &r.logger),
        ("thread", &r.thread),
        ("trace", &r.trace),
    ] {
        if !v.is_empty() {
            println!("{k:>10}: {v}");
        }
    }
    println!("\nmessage:\n{}", r.msg);
    if !r.stack.is_empty() {
        println!("\nstack_trace:\n{}", r.stack);
    }
}

fn patterns(recs: &[Record], limit: usize, level: Option<&str>, json_out: bool, full: bool) {
    let want: Option<Vec<String>> =
        level.map(|l| l.split(',').map(|s| s.trim().to_uppercase()).collect());
    let mut idx: HashMap<String, usize> = HashMap::new();
    let mut clusters: Vec<(String, usize, Vec<String>, String, usize)> = Vec::new();
    for (i, r) in recs.iter().enumerate() {
        if let Some(w) = &want
            && !w.iter().any(|l| l == &r.level)
        {
            continue;
        }
        let s = sig(&r.msg);
        match idx.get(&s) {
            Some(&j) => {
                let c = &mut clusters[j];
                c.1 += 1;
                if !c.2.contains(&r.level) {
                    c.2.push(r.level.clone());
                }
            }
            None => {
                idx.insert(s.clone(), clusters.len());
                clusters.push((s, 1, vec![r.level.clone()], r.msg.clone(), i));
            }
        }
    }
    clusters.sort_by_key(|b| std::cmp::Reverse(b.1));

    if json_out {
        let arr: Vec<Value> = clusters
            .iter()
            .take(limit)
            .map(|(s, count, levels, sample, first)| {
                json!({"count": count, "levels": levels, "sig": s, "sample": sample, "first_index": first})
            })
            .collect();
        print_json(&json!({"clusters": clusters.len(), "patterns": arr}));
        return;
    }
    println!("{} distinct pattern(s)\n", clusters.len());
    for (_, count, levels, sample, first) in clusters.into_iter().take(limit) {
        println!(
            "{count:>6}x [{}] #{first}  {}",
            levels.join("/"),
            body(&sample, full, 120)
        );
    }
}

fn fmt_ms(ms: i64) -> String {
    use chrono::{TimeZone, Utc};
    Utc.timestamp_millis_opt(ms)
        .single()
        .map(|d| d.format("%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| "?".into())
}

fn timeline(recs: &[Record], buckets: usize, json_out: bool) {
    let pts: Vec<(i64, bool)> = recs
        .iter()
        .filter_map(|r| ts_epoch_ms(&r.ts).map(|ms| (ms, r.level == "ERROR" || r.level == "FATAL")))
        .collect();
    if pts.is_empty() {
        if json_out {
            print_json(&json!({"buckets": [], "note": "no parseable timestamps"}));
        } else {
            println!("no parseable timestamps");
        }
        return;
    }
    let n = buckets.max(1);
    let min = pts.iter().map(|(t, _)| *t).min().unwrap();
    let max = pts.iter().map(|(t, _)| *t).max().unwrap();
    let span = (max - min).max(1);
    let width = (span / n as i64).max(1);
    let mut total = vec![0usize; n];
    let mut errs = vec![0usize; n];
    for (ms, is_err) in &pts {
        let b = (((ms - min) / width) as usize).min(n - 1);
        total[b] += 1;
        if *is_err {
            errs[b] += 1;
        }
    }
    if json_out {
        let arr: Vec<Value> = (0..n)
            .map(|i| json!({"start_ms": min + i as i64 * width, "count": total[i], "errors": errs[i]}))
            .collect();
        print_json(
            &json!({"bucket_secs": width / 1000, "start": fmt_ms(min), "end": fmt_ms(max), "buckets": arr}),
        );
        return;
    }
    let peak = *total.iter().max().unwrap_or(&1).max(&1);
    println!(
        "{} records over {} → {}  (bucket {}s)\n",
        pts.len(),
        fmt_ms(min),
        fmt_ms(max),
        (width / 1000).max(1)
    );
    for i in 0..n {
        let bar = "█".repeat(total[i] * 40 / peak);
        let err = if errs[i] > 0 {
            format!("  err {}", errs[i])
        } else {
            String::new()
        };
        println!(
            "{}  {:>6} {bar}{err}",
            fmt_ms(min + i as i64 * width),
            total[i]
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(service: &str, level: &str, logger: &str, msg: &str, ts: &str) -> Record {
        Record {
            ts: ts.into(),
            level: level.into(),
            service: service.into(),
            pod: String::new(),
            namespace: String::new(),
            logger: logger.into(),
            thread: String::new(),
            trace: String::new(),
            msg: msg.into(),
            stack: String::new(),
        }
    }

    fn empty_filter() -> Filter {
        Filter {
            service: None,
            logger: None,
            match_: None,
            after: None,
            before: None,
        }
    }

    fn sample() -> Vec<Record> {
        vec![
            rec(
                "auth-service",
                "ERROR",
                "a.b.Proc",
                "null pointer",
                "2026-06-04T09:00:01Z",
            ),
            rec(
                "billing-service",
                "WARN",
                "a.b.Pool",
                "timeout to db",
                "2026-06-04T09:00:02Z",
            ),
            rec(
                "auth-service",
                "INFO",
                "a.b.Boot",
                "started ok",
                "2026-06-04T09:05:00Z",
            ),
        ]
    }

    #[test]
    fn filter_by_service_substring_ci() {
        let f = Filter {
            service: Some("AUTH".into()),
            ..empty_filter()
        };
        let out = apply_filter(sample(), &f);
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|r| r.service == "auth-service"));
    }

    #[test]
    fn filter_by_match_on_message() {
        let f = Filter {
            match_: Some("timeout".into()),
            ..empty_filter()
        };
        let out = apply_filter(sample(), &f);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].service, "billing-service");
    }

    #[test]
    fn filter_by_logger_and_time_bounds() {
        let f = Filter {
            logger: Some("Boot".into()),
            ..empty_filter()
        };
        assert_eq!(apply_filter(sample(), &f).len(), 1);

        let f = Filter {
            after: Some("2026-06-04T09:00:02".into()),
            ..empty_filter()
        };
        let out = apply_filter(sample(), &f);
        assert_eq!(out.len(), 2);

        let f = Filter {
            before: Some("2026-06-04T09:00:01Z".into()),
            ..empty_filter()
        };
        assert_eq!(apply_filter(sample(), &f).len(), 1);
    }

    #[test]
    fn body_truncates_unless_full() {
        let long = "x".repeat(500);
        let truncated = body(&long, false, 120);
        assert_eq!(truncated.chars().count(), 120);
        assert!(truncated.ends_with('…'));
        let whole = body(&long, true, 120);
        assert_eq!(whole.chars().count(), 500);
        assert!(!whole.ends_with('…'));
    }

    #[test]
    fn record_json_carries_full_msg_and_stack() {
        let mut r = rec("svc", "ERROR", "a.b.C", &"m".repeat(400), "2026-06-04T09:00:01Z");
        r.stack = "s".repeat(800);
        r.trace = "t1".into();
        let with_index = record_json(Some(7), &r);
        assert_eq!(with_index["index"], 7);
        assert_eq!(with_index["msg"].as_str().unwrap().len(), 400);
        assert_eq!(with_index["stack"].as_str().unwrap().len(), 800);
        assert_eq!(with_index["trace"], "t1");
        // without an index the key is absent
        let no_index = record_json(None, &r);
        assert!(no_index.get("index").is_none());
    }

    #[test]
    fn filters_compose() {
        let f = Filter {
            service: Some("auth".into()),
            match_: Some("null".into()),
            ..empty_filter()
        };
        let out = apply_filter(sample(), &f);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].level, "ERROR");
    }
}

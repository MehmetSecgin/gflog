use crate::cli::{Conn, Filter, Ns, TimeRange, View};
use crate::record::normalize;
use chrono::{NaiveDateTime, TimeZone, Utc};
use reqwest::StatusCode;
use reqwest::blocking::{Client, RequestBuilder};
use reqwest::header::SET_COOKIE;
use serde_json::{Map, Value};
use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 \
                  (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36";

fn base() -> String {
    let raw = std::env::var("GRAFANA_URL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            fs::read_to_string(url_file())
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        });
    match raw {
        Some(u) => match normalize_url(&u) {
            Ok(v) => v,
            Err(e) => crate::die(&format!("invalid Grafana URL ({}): {e}", u.trim())),
        },
        None => crate::die(&format!(
            "no Grafana URL set. Set one:\n  gflog url https://grafana.example.com\nor export GRAFANA_URL (checked $GRAFANA_URL and {})",
            url_file().display()
        )),
    }
}

fn is_local_host(host: &str) -> bool {
    let bare = host.split(':').next().unwrap_or("");
    matches!(bare, "localhost" | "127.0.0.1" | "::1") || bare.ends_with(".localhost")
}

fn normalize_url(raw: &str) -> Result<String, String> {
    let u = raw.trim().trim_end_matches('/');
    if u.is_empty() {
        return Err("empty URL".into());
    }
    let host = if let Some(h) = u.strip_prefix("https://") {
        h.split('/').next().unwrap_or("")
    } else if let Some(h) = u.strip_prefix("http://") {
        let host = h.split('/').next().unwrap_or("");
        if is_local_host(host) {
            return Ok(u.to_string());
        }
        return Err(format!(
            "refusing http:// for non-local host {host:?} — your token/cookie would be sent in cleartext. Use https://"
        ));
    } else {
        return Err(format!("URL must start with https:// (got {u:?})"));
    };
    if host.is_empty() {
        return Err("URL has no host".into());
    }
    Ok(u.to_string())
}

fn cfg_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_default()
        .join(".config/grafana-logs")
}
fn url_file() -> PathBuf {
    cfg_dir().join("url")
}
fn cookie_file() -> PathBuf {
    cfg_dir().join("cookie")
}
fn ds_cache() -> PathBuf {
    cfg_dir().join("datasource.json")
}
fn token_file() -> PathBuf {
    cfg_dir().join("token")
}

fn token_value() -> Option<String> {
    if let Ok(t) = std::env::var("GRAFANA_TOKEN") {
        let t = t.trim();
        if !t.is_empty() {
            return Some(t.to_string());
        }
    }
    fs::read_to_string(token_file())
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

enum Auth {
    Bearer(String),
    Cookie(String),
}

fn resolve_auth() -> Auth {
    if let Some(t) = token_value() {
        return Auth::Bearer(t);
    }
    match read_cookie_raw() {
        Some(raw) if raw.contains('=') => Auth::Cookie(raw),
        Some(raw) => Auth::Cookie(format!("grafana_session={raw}")),
        None => crate::die(&format!(
            "no credentials. Set one:\n  gflog token    (service-account Bearer token — preferred, no browser collision)\n  gflog cookie   (copies the grafana_session value from your clipboard)\nlooked in $GRAFANA_TOKEN, {}, {}",
            token_file().display(),
            cookie_file().display()
        )),
    }
}

fn clip(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

fn unit_secs(u: char) -> Option<i64> {
    match u {
        's' => Some(1),
        'm' => Some(60),
        'h' => Some(3600),
        'd' => Some(86400),
        'w' => Some(604800),
        _ => None,
    }
}

fn read_cookie_raw() -> Option<String> {
    fs::read_to_string(cookie_file())
        .ok()
        .map(|s| s.trim().to_string())
}

fn cookie_header() -> String {
    match read_cookie_raw() {
        Some(raw) if raw.contains('=') => raw,
        Some(raw) => format!("grafana_session={raw}"),
        None => crate::die(&format!(
            "no cookie at {}\n  set it: gflog cookie   (copies the grafana_session value from your clipboard)",
            cookie_file().display()
        )),
    }
}

fn client() -> Client {
    Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap_or_else(|e| crate::die(&format!("http client init failed: {e}")))
}

fn with_headers(rb: RequestBuilder) -> RequestBuilder {
    let rb = match resolve_auth() {
        Auth::Bearer(t) => rb.header("Authorization", format!("Bearer {t}")),
        Auth::Cookie(c) => rb.header("Cookie", c),
    };
    rb.header("Accept", "application/json")
        .header("User-Agent", UA)
}

fn save_rotated(set_cookies: &[String]) {
    for c in set_cookies {
        let Some(rest) = c.trim_start().strip_prefix("grafana_session=") else {
            continue;
        };
        let tok = rest.split(';').next().unwrap_or("");
        if tok.is_empty() || tok == "deleted" {
            continue;
        }
        let cur = read_cookie_raw()
            .map(|r| r.rsplit('=').next().unwrap_or("").to_string())
            .unwrap_or_default();
        if tok != cur {
            let _ = fs::create_dir_all(cfg_dir());
            let _ = write_atomic(&cookie_file(), tok);
        }
    }
}

fn set_600(p: &PathBuf) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(p, fs::Permissions::from_mode(0o600));
    }
}

fn write_atomic(path: &PathBuf, content: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, content)?;
    set_600(&tmp);
    fs::rename(&tmp, path)
}

fn collect_set_cookies(resp: &reqwest::blocking::Response) -> Vec<String> {
    resp.headers()
        .get_all(SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok().map(str::to_string))
        .collect()
}

fn http_get(url: &str, params: &[(&str, &str)]) -> (StatusCode, Vec<String>, String) {
    let resp = match with_headers(client().get(url).query(params)).send() {
        Ok(r) => r,
        Err(e) => crate::die(&format!("cannot reach {}: {e}", base())),
    };
    let status = resp.status();
    let cookies = collect_set_cookies(&resp);
    let body = resp.text().unwrap_or_default();
    (status, cookies, body)
}

fn is_auth_err(status: StatusCode) -> bool {
    status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN
}

fn auth_die(status: StatusCode, body: &str) -> ! {
    let fix = if token_value().is_some() {
        "token rejected — check/replace it:\n  gflog token --test"
    } else {
        "session expired and the refresh attempt failed — re-copy the cookie:\n  gflog cookie\n(tip: a service-account Bearer token never rotates and avoids this — see `gflog token`)"
    };
    crate::die(&format!(
        "auth failed ({}). {fix}\n{}",
        status.as_u16(),
        clip(body, 300)
    ));
}

fn api(path: &str, params: &[(&str, &str)]) -> Value {
    let url = format!("{}{path}", base());
    let (status, cookies, body) = http_get(&url, params);
    if status.is_success() {
        save_rotated(&cookies);
        return serde_json::from_str(&body).unwrap_or(Value::Null);
    }
    if is_auth_err(status) {
        // A Bearer token never rotates, so a retry can't help — fail clearly now.
        // For a cookie, the prior rotation may have left a stale value: refresh once
        // and retry a single time before giving up.
        if token_value().is_none() && rotate(false) {
            let (status2, cookies2, body2) = http_get(&url, params);
            if status2.is_success() {
                save_rotated(&cookies2);
                return serde_json::from_str(&body2).unwrap_or(Value::Null);
            }
            auth_die(status2, &body2);
        }
        auth_die(status, &body);
    }
    crate::die(&format!(
        "HTTP {} from {path}\n{}",
        status.as_u16(),
        clip(&body, 300)
    ));
}

pub fn rotate(verbose: bool) -> bool {
    if token_value().is_some() {
        if verbose {
            println!("using a Bearer token — session rotation not applicable");
        }
        return true;
    }
    if read_cookie_raw().is_none() {
        return false;
    }
    let before = read_cookie_raw()
        .unwrap_or_default()
        .rsplit('=')
        .next()
        .unwrap_or("")
        .to_string();
    let url = format!("{}/api/user/auth-tokens/rotate", base());
    let rb = client()
        .post(&url)
        .body("{}")
        .header("Cookie", cookie_header())
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .header("User-Agent", UA);
    let resp = match rb.send() {
        Ok(r) => r,
        Err(e) => crate::die(&format!("cannot reach {}: {e}", base())),
    };
    if !resp.status().is_success() {
        if verbose {
            println!(
                "rotate failed ({}) — session likely fully expired, re-copy the cookie",
                resp.status().as_u16()
            );
        }
        return false;
    }
    let cookies = collect_set_cookies(&resp);
    let _ = resp.text();
    save_rotated(&cookies);
    let after = read_cookie_raw()
        .unwrap_or_default()
        .rsplit('=')
        .next()
        .unwrap_or("")
        .to_string();
    if verbose {
        if after != before {
            let tail: String = after
                .chars()
                .rev()
                .take(6)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            println!("session refreshed (new token …{tail})");
        } else {
            println!("session refreshed (unchanged)");
        }
    }
    true
}

fn sanitize_cookie(raw: &str) -> String {
    let raw = raw.replace('\u{feff}', "");
    let raw = raw.trim();
    let re = regex::Regex::new(r"grafana_session=([^;\s]+)").unwrap();
    if let Some(c) = re.captures(raw) {
        return c[1].to_string();
    }
    let tok = raw.split_whitespace().last().unwrap_or(raw);
    let tok = tok.trim_matches(|c| c == '\'' || c == '"');
    tok.split(';').next().unwrap_or("").to_string()
}

fn read_clipboard() -> String {
    let tools: &[(&str, &[&str])] = &[
        ("pbpaste", &[]),
        ("wl-paste", &["--no-newline"]),
        ("xclip", &["-selection", "clipboard", "-o"]),
        ("xsel", &["-b"]),
    ];
    for (bin, args) in tools {
        if let Ok(o) = Command::new(bin).args(*args).output()
            && o.status.success()
            && let Ok(s) = String::from_utf8(o.stdout)
            && !s.trim().is_empty()
        {
            return s;
        }
    }
    String::new()
}

fn report_auth() -> bool {
    let url = format!("{}/api/datasources", base());
    let resp = match with_headers(client().get(&url)).send() {
        Ok(r) => r,
        Err(e) => crate::die(&format!("cannot reach {}: {e}", base())),
    };
    let status = resp.status();
    let cookies = collect_set_cookies(&resp);
    let body = resp.text().unwrap_or_default();
    let mode = if token_value().is_some() {
        "Bearer token"
    } else {
        "session cookie"
    };
    if status.is_success() {
        save_rotated(&cookies);
        let v: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
        let names: Vec<String> = loki_list(&v).iter().map(|(_, n)| n.clone()).collect();
        let joined = if names.is_empty() {
            "(none with a URL)".to_string()
        } else {
            names.join(", ")
        };
        println!(
            "auth OK via {mode} — {} usable Loki datasource(s): {joined}",
            names.len()
        );
        true
    } else if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        let fix = if token_value().is_some() {
            "token rejected/expired — set a fresh one with `gflog token`.".to_string()
        } else {
            format!(
                "cookie expired/rotated. Copy a fresh grafana_session from the browser ({}) and run `cookie` again.",
                base()
            )
        };
        println!("auth FAILED ({}) via {mode} — {fix}", status.as_u16());
        false
    } else {
        println!("unexpected HTTP {}: {}", status.as_u16(), clip(&body, 300));
        false
    }
}

fn loki_list(v: &Value) -> Vec<(i64, String)> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter(|d| {
                    d.get("type").and_then(Value::as_str) == Some("loki")
                        && d.get("url")
                            .and_then(Value::as_str)
                            .map(|u| !u.is_empty())
                            .unwrap_or(false)
                })
                .filter_map(|d| {
                    Some((d.get("id")?.as_i64()?, d.get("name")?.as_str()?.to_string()))
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn cmd_cookie(value: Option<String>, stdin: bool, test: bool) {
    if test {
        std::process::exit(if report_auth() { 0 } else { 1 });
    }
    let (raw, src) = if let Some(v) = value {
        (v, "argument")
    } else if stdin {
        let mut s = String::new();
        let _ = std::io::stdin().read_to_string(&mut s);
        (s, "stdin")
    } else {
        let c = read_clipboard();
        if c.trim().is_empty() {
            crate::die("clipboard empty — pass the value as an argument or pipe it with --stdin");
        }
        (c, "clipboard")
    };
    let tok = sanitize_cookie(&raw);
    if tok.is_empty() {
        crate::die(&format!("could not parse a cookie token from {src}"));
    }
    let _ = fs::create_dir_all(cfg_dir());
    if let Err(e) = write_atomic(&cookie_file(), &tok) {
        crate::die(&format!("cannot write {}: {e}", cookie_file().display()));
    }
    let tail: String = tok
        .chars()
        .rev()
        .take(6)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    println!(
        "wrote {} from {src} (token …{tail}, {} chars)",
        cookie_file().display(),
        tok.chars().count()
    );
    report_auth();
}

fn sanitize_token(raw: &str) -> String {
    let raw = raw.replace('\u{feff}', "");
    let raw = raw.trim().trim_matches(|c| c == '\'' || c == '"').trim();
    raw.strip_prefix("Bearer ")
        .or_else(|| raw.strip_prefix("bearer "))
        .unwrap_or(raw)
        .trim()
        .to_string()
}

pub fn cmd_token(value: Option<String>, stdin: bool, test: bool, clear: bool) {
    if clear {
        match fs::remove_file(token_file()) {
            Ok(_) => println!(
                "removed {} — reverted to cookie auth",
                token_file().display()
            ),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                println!("no token file to remove")
            }
            Err(e) => crate::die(&format!("cannot remove {}: {e}", token_file().display())),
        }
        return;
    }
    if test {
        std::process::exit(if report_auth() { 0 } else { 1 });
    }
    let (raw, src) = if let Some(v) = value {
        (v, "argument")
    } else if stdin {
        let mut s = String::new();
        let _ = std::io::stdin().read_to_string(&mut s);
        (s, "stdin")
    } else {
        let c = read_clipboard();
        if c.trim().is_empty() {
            crate::die("clipboard empty — pass the token as an argument or pipe it with --stdin");
        }
        (c, "clipboard")
    };
    let tok = sanitize_token(&raw);
    if tok.is_empty() {
        crate::die(&format!("could not parse a token from {src}"));
    }
    let _ = fs::create_dir_all(cfg_dir());
    if let Err(e) = write_atomic(&token_file(), &tok) {
        crate::die(&format!("cannot write {}: {e}", token_file().display()));
    }
    let tail: String = tok
        .chars()
        .rev()
        .take(6)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    println!(
        "wrote {} from {src} (token …{tail}, {} chars)",
        token_file().display(),
        tok.chars().count()
    );
    report_auth();
}

pub fn cmd_url(value: Option<String>) {
    match value {
        Some(v) => {
            let v = match normalize_url(&v) {
                Ok(v) => v,
                Err(e) => crate::die(&format!("rejected URL: {e}")),
            };
            let _ = fs::create_dir_all(cfg_dir());
            if let Err(e) = fs::write(url_file(), &v) {
                crate::die(&format!("cannot write {}: {e}", url_file().display()));
            }
            set_600(&url_file());
            println!("wrote {} = {v}", url_file().display());
        }
        None => match fs::read_to_string(url_file()) {
            Ok(s) if !s.trim().is_empty() => println!("{}", s.trim()),
            _ => println!("no URL set — run `gflog url <https://...>` or export GRAFANA_URL"),
        },
    }
}

fn loki_datasource(want: Option<&str>, refresh: bool) -> (i64, String) {
    if !refresh
        && want.is_none()
        && let Ok(t) = fs::read_to_string(ds_cache())
        && let Ok(v) = serde_json::from_str::<Value>(&t)
        && let (Some(id), Some(name)) = (
            v.get("id").and_then(Value::as_i64),
            v.get("name").and_then(Value::as_str),
        )
    {
        return (id, name.to_string());
    }
    let ds = loki_list(&api("/api/datasources", &[]));
    if ds.is_empty() {
        crate::die("no usable Loki datasource found in this Grafana");
    }
    let chosen = if let Some(w) = want {
        match ds
            .iter()
            .find(|(_, n)| n.to_lowercase().contains(&w.to_lowercase()))
        {
            Some(c) => c.clone(),
            None => {
                let avail: Vec<String> = ds.iter().map(|(_, n)| n.clone()).collect();
                crate::die(&format!(
                    "no Loki datasource matching {w:?}. available: {}",
                    avail.join(", ")
                ));
            }
        }
    } else {
        ds[0].clone()
    };
    let _ = fs::create_dir_all(cfg_dir());
    let rec = serde_json::json!({"id": chosen.0, "name": chosen.1});
    let _ = fs::write(ds_cache(), rec.to_string());
    chosen
}

fn now_ns() -> i64 {
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    d.as_nanos() as i64
}

fn to_ns(s: &str, now: i64) -> i64 {
    let s = s.trim();
    if s == "now" || s.is_empty() {
        return now;
    }
    let last = s.chars().last().unwrap();
    let head = &s[..s.len() - last.len_utf8()];
    if let Some(secs) = unit_secs(last)
        && let Ok(n) = head.trim_start_matches('-').parse::<i64>()
    {
        return now - n * secs * 1_000_000_000;
    }
    if s.chars().all(|c| c.is_ascii_digit()) {
        let n: i64 = s.parse().unwrap_or(0);
        return n * if s.len() > 17 { 1 } else { 1_000_000_000 };
    }
    let head = s.split('.').next().unwrap_or(s);
    match NaiveDateTime::parse_from_str(head, "%Y-%m-%dT%H:%M:%S") {
        Ok(ndt) => chrono::Local
            .from_local_datetime(&ndt)
            .single()
            .map(|dt| dt.timestamp() * 1_000_000_000)
            .unwrap_or(now),
        Err(_) => crate::die(&format!("cannot parse time {s:?}")),
    }
}

fn iso_from_ns(ts_ns: i64) -> String {
    let secs = ts_ns / 1_000_000_000;
    let ms = (ts_ns % 1_000_000_000) / 1_000_000;
    let dt = Utc.timestamp_opt(secs, 0).single().unwrap_or_else(Utc::now);
    format!("{}.{:03}Z", dt.format("%Y-%m-%dT%H:%M:%S"), ms)
}

fn duration_secs(s: &str) -> i64 {
    let s = s.trim();
    let last = s.chars().last().unwrap_or('h');
    let secs = unit_secs(last).unwrap_or(3600);
    let n: i64 = s[..s.len() - last.len_utf8()].parse().unwrap_or(0);
    n * secs
}

fn window(t: &TimeRange) -> (i64, i64, String) {
    let now = now_ns();
    let end_ns = to_ns(&t.end, now);
    let start_ns = match &t.start {
        Some(s) => to_ns(s, now),
        None => end_ns - duration_secs(&t.since) * 1_000_000_000,
    };
    let win = format!("{}..{}", t.start.as_deref().unwrap_or(&t.since), t.end);
    (start_ns, end_ns, win)
}

fn prep(conn: &Conn) -> (i64, String) {
    if !conn.no_rotate {
        rotate(false);
    }
    loki_datasource(conn.datasource.as_deref(), conn.refresh_datasource)
}

fn proxy_get(id: i64, subpath: &str, params: &[(&str, &str)]) -> Value {
    api(&format!("/api/datasources/proxy/{id}{subpath}"), params)
}

fn selector_has_namespace(query: &str) -> bool {
    let inner = match (query.find('{'), query.find('}')) {
        (Some(a), Some(b)) if b > a => &query[a + 1..b],
        _ => return false,
    };
    regex::Regex::new(r#"namespace\s*(=~|!~|!=|=)"#)
        .unwrap()
        .is_match(inner)
}

fn ns_ok(ns: &str) -> bool {
    !ns.is_empty()
        && ns
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '*' | '-'))
}

fn ns_matcher(nss: &[String]) -> String {
    for ns in nss {
        if !ns_ok(ns) {
            crate::die(&format!(
                "invalid --namespace {ns:?}: only [A-Za-z0-9._*-] allowed. For a custom matcher, put it in -q directly."
            ));
        }
    }
    format!("namespace=~\"{}\"", nss.join("|"))
}

fn inject_ns(query: &str, matcher: &str) -> String {
    match query.find('{') {
        Some(pos) => {
            let after = pos + 1;
            if query[after..].trim_start().starts_with('}') {
                format!("{}{}{}", &query[..after], matcher, &query[after..])
            } else {
                format!("{}{}, {}", &query[..after], matcher, &query[after..])
            }
        }
        None => format!("{{{matcher}}}"),
    }
}

fn resolve_ns(query: &str, ns: &Ns) -> Option<Vec<String>> {
    if selector_has_namespace(query) {
        if !ns.namespace.is_empty() {
            eprintln!(
                "note: -q already sets a namespace matcher — keeping it, ignoring --namespace"
            );
        }
        return None;
    }
    (!ns.namespace.is_empty()).then(|| ns.namespace.clone())
}

fn apply_ns(query: &str, ns: &Ns) -> String {
    match resolve_ns(query, ns) {
        Some(nss) => inject_ns(query, &ns_matcher(&nss)),
        None => query.to_string(),
    }
}

fn apply_ns_opt(query: Option<&str>, ns: &Ns) -> Option<String> {
    let q = query.unwrap_or("");
    match resolve_ns(q, ns) {
        Some(nss) => Some(inject_ns(q, &ns_matcher(&nss))),
        None => query.map(str::to_string),
    }
}

pub fn run_live(
    query: &str,
    ns: &Ns,
    limit: usize,
    json: bool,
    time: &TimeRange,
    conn: &Conn,
    filter: &Filter,
    view: &View,
) {
    let (id, name) = prep(conn);
    let query = apply_ns(query, ns);
    let query = query.as_str();
    let (start_ns, end_ns, win) = window(time);
    let data = proxy_get(
        id,
        "/loki/api/v1/query_range",
        &[
            ("query", query),
            ("start", &start_ns.to_string()),
            ("end", &end_ns.to_string()),
            ("limit", &limit.to_string()),
            ("direction", "backward"),
        ],
    );
    let empty = Map::new();
    let mut recs = Vec::new();
    if let Some(streams) = data
        .get("data")
        .and_then(|d| d.get("result"))
        .and_then(Value::as_array)
    {
        for stream in streams {
            let labels = stream
                .get("stream")
                .and_then(Value::as_object)
                .unwrap_or(&empty);
            if let Some(values) = stream.get("values").and_then(Value::as_array) {
                for pair in values {
                    let Some(arr) = pair.as_array() else { continue };
                    let ts_ns: i64 = arr
                        .first()
                        .and_then(Value::as_str)
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(0);
                    let line = arr.get(1).and_then(Value::as_str).unwrap_or("");
                    recs.push(normalize(line, &iso_from_ns(ts_ns), labels));
                }
            }
        }
    }
    recs.sort_by(|a, b| b.ts.cmp(&a.ts));
    let total = recs.len();
    let recs = crate::views::apply_filter(recs, filter);
    if recs.len() == total {
        eprintln!("# loki[{name}] q='{query}' window={win}  ({total} lines)\n");
    } else {
        eprintln!(
            "# loki[{name}] q='{query}' window={win}  ({} of {total} lines after filter)\n",
            recs.len()
        );
    }
    crate::views::dispatch(&recs, view, json);
}

fn label_set(metric: &Map<String, Value>) -> String {
    if metric.is_empty() {
        return "{}".to_string();
    }
    let mut kv: Vec<String> = metric
        .iter()
        .map(|(k, v)| format!("{k}=\"{}\"", v.as_str().unwrap_or("")))
        .collect();
    kv.sort();
    format!("{{{}}}", kv.join(", "))
}

pub fn run_metric(query: &str, ns: &Ns, step: &str, json: bool, time: &TimeRange, conn: &Conn) {
    let (id, name) = prep(conn);
    let query = apply_ns(query, ns);
    let query = query.as_str();
    let (start_ns, end_ns, win) = window(time);
    let step_s = duration_secs(step).max(1);
    let data = proxy_get(
        id,
        "/loki/api/v1/query_range",
        &[
            ("query", query),
            ("start", &start_ns.to_string()),
            ("end", &end_ns.to_string()),
            ("step", &format!("{step_s}s")),
        ],
    );
    let rtype = data
        .get("data")
        .and_then(|d| d.get("resultType"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let empty = Map::new();
    let series = data
        .get("data")
        .and_then(|d| d.get("result"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let mut parsed: Vec<(String, Vec<(f64, f64)>, &Value)> = Vec::new();
    for s in &series {
        let metric = s.get("metric").and_then(Value::as_object).unwrap_or(&empty);
        let mut pts: Vec<(f64, f64)> = Vec::new();
        let raw = s
            .get("values")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let vector = s.get("value").cloned();
        let iter: Vec<Value> = if raw.is_empty() {
            vector.into_iter().collect()
        } else {
            raw
        };
        for p in &iter {
            if let Some(a) = p.as_array() {
                let t = a
                    .first()
                    .and_then(|x| {
                        x.as_f64()
                            .or_else(|| x.as_str().and_then(|s| s.parse().ok()))
                    })
                    .unwrap_or(0.0);
                let v = a
                    .get(1)
                    .and_then(|x| x.as_str())
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0.0);
                pts.push((t, v));
            }
        }
        parsed.push((
            label_set(metric),
            pts,
            s.get("metric").unwrap_or(&Value::Null),
        ));
    }

    if json {
        let arr: Vec<Value> = parsed
            .iter()
            .map(|(_, pts, metric)| {
                let sum: f64 = pts.iter().map(|(_, v)| v).sum();
                let last = pts.last().map(|(_, v)| *v);
                let max = pts.iter().map(|(_, v)| *v).fold(f64::MIN, f64::max);
                let min = pts.iter().map(|(_, v)| *v).fold(f64::MAX, f64::min);
                serde_json::json!({
                    "labels": *metric,
                    "points": pts.iter().map(|(t, v)| serde_json::json!([t, v])).collect::<Vec<_>>(),
                    "sum": sum, "last": last,
                    "max": (!pts.is_empty()).then_some(max),
                    "min": (!pts.is_empty()).then_some(min),
                })
            })
            .collect();
        crate::views::print_json(
            &serde_json::json!({"resultType": rtype, "step": format!("{step_s}s"), "series": arr}),
        );
        return;
    }

    eprintln!(
        "# loki[{name}] metric q='{query}' window={win} step={step_s}s  ({} series)\n",
        parsed.len()
    );
    if parsed.is_empty() {
        println!("no data");
        return;
    }
    for (labels, pts, _) in &parsed {
        let sum: f64 = pts.iter().map(|(_, v)| v).sum();
        let last = pts.last().map(|(_, v)| *v).unwrap_or(0.0);
        let max = pts.iter().map(|(_, v)| *v).fold(0.0_f64, f64::max);
        println!("{labels}");
        println!(
            "  pts={} sum={} last={} max={}",
            pts.len(),
            trim_num(sum),
            trim_num(last),
            trim_num(max)
        );
    }
}

fn trim_num(n: f64) -> String {
    if (n - n.round()).abs() < 1e-9 {
        format!("{}", n.round() as i64)
    } else {
        format!("{n:.3}")
    }
}

pub fn run_labels(ns: &Ns, json: bool, time: &TimeRange, conn: &Conn) {
    let (id, name) = prep(conn);
    let (start_ns, end_ns, win) = window(time);
    let start = start_ns.to_string();
    let end = end_ns.to_string();
    let mut params: Vec<(&str, &str)> = vec![("start", &start), ("end", &end)];
    let selector = resolve_ns("", ns).map(|nss| format!("{{{}}}", ns_matcher(&nss)));
    if let Some(q) = selector.as_deref() {
        params.push(("query", q));
    }
    let data = proxy_get(id, "/loki/api/v1/labels", &params);
    let labels: Vec<String> = data
        .get("data")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    if json {
        crate::views::print_json(&serde_json::json!({"datasource": name, "labels": labels}));
        return;
    }
    eprintln!(
        "# loki[{name}] labels window={win}  ({} labels)\n",
        labels.len()
    );
    for l in labels {
        println!("{l}");
    }
}

pub fn run_values(
    label: &str,
    query: Option<&str>,
    ns: &Ns,
    json: bool,
    time: &TimeRange,
    conn: &Conn,
) {
    let (id, name) = prep(conn);
    let query = apply_ns_opt(query, ns);
    let (start_ns, end_ns, win) = window(time);
    let start = start_ns.to_string();
    let end = end_ns.to_string();
    let mut params: Vec<(&str, &str)> = vec![("start", &start), ("end", &end)];
    if let Some(q) = query.as_deref() {
        params.push(("query", q));
    }
    let data = proxy_get(id, &format!("/loki/api/v1/label/{label}/values"), &params);
    let values: Vec<String> = data
        .get("data")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    if json {
        crate::views::print_json(
            &serde_json::json!({"datasource": name, "label": label, "values": values}),
        );
        return;
    }
    eprintln!(
        "# loki[{name}] values of '{label}' window={win}  ({} values)\n",
        values.len()
    );
    for v in values {
        println!("{v}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_750_000_000_000_000_000;

    #[test]
    fn to_ns_now_and_empty() {
        assert_eq!(to_ns("now", NOW), NOW);
        assert_eq!(to_ns("", NOW), NOW);
    }

    #[test]
    fn to_ns_relative_windows() {
        assert_eq!(to_ns("30m", NOW), NOW - 1_800 * 1_000_000_000);
        assert_eq!(to_ns("2h", NOW), NOW - 7_200 * 1_000_000_000);
        assert_eq!(to_ns("1d", NOW), NOW - 86_400 * 1_000_000_000);
    }

    #[test]
    fn to_ns_epoch_seconds_vs_nanos() {
        assert_eq!(to_ns("1700000000", NOW), 1_700_000_000 * 1_000_000_000);
        assert_eq!(to_ns("1700000000000000000", NOW), 1_700_000_000_000_000_000);
    }

    #[test]
    fn duration_secs_units() {
        assert_eq!(duration_secs("30m"), 1_800);
        assert_eq!(duration_secs("2h"), 7_200);
        assert_eq!(duration_secs("1d"), 86_400);
        assert_eq!(duration_secs("45s"), 45);
    }

    #[test]
    fn is_auth_err_matches_401_403_only() {
        assert!(is_auth_err(StatusCode::UNAUTHORIZED));
        assert!(is_auth_err(StatusCode::FORBIDDEN));
        assert!(!is_auth_err(StatusCode::OK));
        assert!(!is_auth_err(StatusCode::INTERNAL_SERVER_ERROR));
    }

    #[test]
    fn write_atomic_writes_overwrites_and_leaves_no_tmp() {
        let p = std::env::temp_dir().join("gflog_test_atomic_cred");
        let _ = fs::remove_file(&p);
        write_atomic(&p, "first").unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), "first");
        write_atomic(&p, "second").unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), "second");
        assert!(!p.with_extension("tmp").exists());
        let _ = fs::remove_file(&p);
    }

    #[test]
    fn sanitize_cookie_forms() {
        assert_eq!(sanitize_cookie("abc123"), "abc123");
        assert_eq!(sanitize_cookie("grafana_session=abc123; Path=/"), "abc123");
        assert_eq!(sanitize_cookie("  'tok' "), "tok");
        assert_eq!(sanitize_cookie("\u{feff}grafana_session=xyz"), "xyz");
    }

    #[test]
    fn sanitize_token_strips_bearer_and_quotes() {
        assert_eq!(sanitize_token("Bearer glsa_abc"), "glsa_abc");
        assert_eq!(sanitize_token("  \"glsa_xyz\"  "), "glsa_xyz");
        assert_eq!(sanitize_token("glsa_plain"), "glsa_plain");
    }

    #[test]
    fn normalize_url_accepts_https_and_local_http() {
        assert_eq!(
            normalize_url("https://g.example.com/").unwrap(),
            "https://g.example.com"
        );
        assert_eq!(
            normalize_url("  https://g.example.com  ").unwrap(),
            "https://g.example.com"
        );
        assert_eq!(
            normalize_url("http://localhost:3000").unwrap(),
            "http://localhost:3000"
        );
        assert_eq!(
            normalize_url("http://127.0.0.1").unwrap(),
            "http://127.0.0.1"
        );
    }

    #[test]
    fn normalize_url_rejects_cleartext_and_schemeless() {
        assert!(normalize_url("http://grafana.example.com").is_err());
        assert!(normalize_url("ftp://x").is_err());
        assert!(normalize_url("grafana.example.com").is_err());
        assert!(normalize_url("").is_err());
        assert!(normalize_url("https://").is_err());
    }

    #[test]
    fn ns_ok_allows_safe_rejects_injection() {
        assert!(ns_ok("prod"));
        assert!(ns_ok("prod-eu_1.x"));
        assert!(ns_ok("prod*"));
        assert!(!ns_ok(""));
        assert!(!ns_ok("a\"} |bar"));
        assert!(!ns_ok("a|b"));
        assert!(!ns_ok("a b"));
        assert!(!ns_ok("a\\b"));
    }

    #[test]
    fn ns_matcher_joins_with_alternation() {
        assert_eq!(ns_matcher(&["a".into(), "b".into()]), r#"namespace=~"a|b""#);
    }

    #[test]
    fn inject_ns_into_empty_and_nonempty_selectors() {
        assert_eq!(inject_ns("{}", r#"namespace=~"a""#), r#"{namespace=~"a"}"#);
        assert_eq!(
            inject_ns(r#"{service="x"}"#, r#"namespace=~"a""#),
            r#"{namespace=~"a", service="x"}"#
        );
        assert_eq!(
            inject_ns("no-selector", r#"namespace=~"a""#),
            r#"{namespace=~"a"}"#
        );
    }

    #[test]
    fn selector_has_namespace_detects_matchers() {
        assert!(selector_has_namespace(r#"{namespace="prod"}"#));
        assert!(selector_has_namespace(r#"{namespace=~"a|b"}"#));
        assert!(selector_has_namespace(r#"{namespace!="x"}"#));
        assert!(!selector_has_namespace(r#"{service="x"}"#));
        assert!(!selector_has_namespace("no braces"));
    }

    #[test]
    fn clip_is_char_safe() {
        assert_eq!(clip("hello", 3), "hel");
        assert_eq!(clip("hi", 10), "hi");
        let s = "héllo wörld ☃";
        assert_eq!(clip(s, 2), "hé");
        let _ = clip(s, 8);
    }

    #[test]
    fn label_set_sorted_and_empty() {
        let mut m = Map::new();
        m.insert("b".into(), Value::String("2".into()));
        m.insert("a".into(), Value::String("1".into()));
        assert_eq!(label_set(&m), r#"{a="1", b="2"}"#);
        assert_eq!(label_set(&Map::new()), "{}");
    }
}

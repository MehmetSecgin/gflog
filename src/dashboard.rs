use crate::cli::DashboardConn;
use chrono::{DateTime, NaiveDate, NaiveDateTime, TimeZone, Utc};
use reqwest::Url;
use serde::Serialize;
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_REDIRECTS: usize = 8;

#[derive(Clone, Debug)]
struct DashboardRef {
    uid: String,
    slug: String,
    org_id: Option<String>,
    panel_id: Option<i64>,
    variables: BTreeMap<String, String>,
    from: Option<String>,
    to: Option<String>,
}

#[derive(Clone, Debug)]
struct Target {
    ref_id: String,
    expression: String,
    legend_format: Option<String>,
}

#[derive(Clone, Debug)]
struct Panel {
    id: i64,
    title: String,
    kind: String,
    unit: Option<String>,
    datasource: Value,
    targets: Vec<Value>,
}

#[derive(Clone, Debug, Serialize)]
struct Summary {
    first: f64,
    last: f64,
    min: f64,
    average: f64,
    max: f64,
    point_count: usize,
}

#[derive(Clone, Debug, Serialize)]
struct Series {
    labels: BTreeMap<String, String>,
    summary: Summary,
}

#[derive(Clone, Debug, Serialize)]
struct TargetOutput {
    ref_id: String,
    expression: String,
    legend_format: Option<String>,
    series: Vec<Series>,
}

#[derive(Clone, Debug, Serialize)]
struct DashboardOutput {
    dashboard: DashboardInfo,
    panel: PanelInfo,
    datasource: DatasourceInfo,
    range: RangeInfo,
    variables: BTreeMap<String, String>,
    targets: Vec<TargetOutput>,
}

#[derive(Clone, Debug, Serialize)]
struct DashboardInfo {
    uid: String,
    title: String,
    url: String,
}

#[derive(Clone, Debug, Serialize)]
struct PanelInfo {
    id: i64,
    title: String,
    #[serde(rename = "type")]
    kind: String,
    unit: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
struct DatasourceInfo {
    uid: String,
    name: String,
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Clone, Debug, Serialize)]
struct RangeInfo {
    start: String,
    end: String,
    step_seconds: i64,
}

#[allow(clippy::too_many_arguments)]
pub fn run(
    raw_url: &str,
    panel_override: Option<&str>,
    since: Option<&str>,
    start: Option<&str>,
    end: Option<&str>,
    step: Option<&str>,
    cli_vars: &[String],
    query_only: bool,
    json: bool,
    conn: &DashboardConn,
) {
    if !conn.no_rotate {
        crate::live::rotate(false);
    }
    let base = parse_base();
    let reference =
        resolve_reference(raw_url, &base, conn.no_rotate).unwrap_or_else(|e| crate::die(&e));
    let dashboard_url = grafana_url(&base, &format!("/api/dashboards/uid/{}", reference.uid));
    let org_params = reference
        .org_id
        .as_deref()
        .map(|org| vec![("orgId", org)])
        .unwrap_or_default();
    let envelope = crate::live::authenticated_json_for_org(
        dashboard_url.as_str(),
        &org_params,
        reference.org_id.as_deref(),
    );
    let dashboard = envelope
        .get("dashboard")
        .unwrap_or_else(|| crate::die("Grafana dashboard response has no dashboard object"));
    let title = string_at(dashboard, "title")
        .unwrap_or("(untitled)")
        .to_string();

    let panels = collect_panels(dashboard);
    let panel = select_panel(&panels, panel_override, reference.panel_id)
        .unwrap_or_else(|e| crate::die(&e));
    let (start_s, end_s, step_s) = resolve_range(
        since,
        start,
        end,
        reference.from.as_deref(),
        reference.to.as_deref(),
        step,
    )
    .unwrap_or_else(|e| crate::die(&e));
    let variables = resolve_variables(
        dashboard,
        &reference.variables,
        cli_vars,
        step_s,
        end_s - start_s,
    )
    .unwrap_or_else(|e| crate::die(&e));
    let panel_ds =
        datasource_parts(&panel.datasource, &variables).unwrap_or_else(|e| crate::die(&e));
    let targets = resolve_targets(&panel, &panel_ds, &variables).unwrap_or_else(|e| crate::die(&e));

    let ds_url = grafana_url(&base, &format!("/api/datasources/uid/{}", panel_ds.0));
    let ds = crate::live::authenticated_json_for_org(
        ds_url.as_str(),
        &org_params,
        reference.org_id.as_deref(),
    );
    let ds_kind = string_at(&ds, "type").unwrap_or("").to_string();
    if !ds_kind.eq_ignore_ascii_case("prometheus") {
        crate::die(&format!(
            "unsupported datasource type {:?}; dashboard v1 supports Prometheus only",
            ds_kind
        ));
    }
    let datasource = DatasourceInfo {
        uid: panel_ds.0,
        name: string_at(&ds, "name").unwrap_or("(unnamed)").to_string(),
        kind: ds_kind,
    };
    let mut target_outputs = Vec::new();
    for target in targets {
        let series = if query_only {
            Vec::new()
        } else {
            query_prometheus(
                &base,
                &datasource.uid,
                &target.expression,
                start_s,
                end_s,
                step_s,
                reference.org_id.as_deref(),
            )
            .unwrap_or_else(|e| crate::die(&e))
        };
        target_outputs.push(TargetOutput {
            ref_id: target.ref_id,
            expression: target.expression,
            legend_format: target.legend_format,
            series,
        });
    }

    let canonical = format!(
        "{}://{}{}/d/{}/{}",
        base.scheme(),
        authority(&base),
        base.path().trim_end_matches('/'),
        reference.uid,
        reference.slug
    );
    let output = DashboardOutput {
        dashboard: DashboardInfo {
            uid: reference.uid,
            title,
            url: canonical,
        },
        panel: PanelInfo {
            id: panel.id,
            title: panel.title,
            kind: panel.kind,
            unit: panel.unit,
        },
        datasource,
        range: RangeInfo {
            start: iso_seconds(start_s),
            end: iso_seconds(end_s),
            step_seconds: step_s,
        },
        variables,
        targets: target_outputs,
    };
    if json {
        crate::views::print_json(&serde_json::to_value(&output).unwrap_or(Value::Null));
    } else {
        print_text(&output, query_only);
    }
}

fn parse_base() -> Url {
    Url::parse(&crate::live::base())
        .unwrap_or_else(|e| crate::die(&format!("invalid Grafana URL: {e}")))
}

fn grafana_url(base: &Url, path: &str) -> Url {
    Url::parse(&format!("{}{}", base.as_str().trim_end_matches('/'), path))
        .expect("Grafana API path must form a valid URL")
}

fn grafana_path<'a>(url: &'a Url, base: &Url) -> Option<&'a str> {
    let base_path = base.path().trim_end_matches('/');
    let path = url.path();
    if base_path.is_empty() {
        Some(path)
    } else {
        path.strip_prefix(base_path)
            .filter(|relative| relative.is_empty() || relative.starts_with('/'))
    }
}

fn authority(url: &Url) -> String {
    match url.port() {
        Some(port) => format!("{}:{port}", url.host_str().unwrap_or("")),
        None => url.host_str().unwrap_or("").to_string(),
    }
}

fn same_origin(a: &Url, b: &Url) -> bool {
    a.scheme() == b.scheme()
        && a.host_str().map(str::to_ascii_lowercase) == b.host_str().map(str::to_ascii_lowercase)
        && a.port_or_known_default() == b.port_or_known_default()
}

fn resolve_reference(raw: &str, base: &Url, no_rotate: bool) -> Result<DashboardRef, String> {
    resolve_reference_with(raw, base, no_rotate, |url| {
        crate::live::authenticated_get_no_redirect(url, &[])
    })
}

fn resolve_reference_with(
    raw: &str,
    base: &Url,
    no_rotate: bool,
    mut get: impl FnMut(&str) -> crate::live::GetResponse,
) -> Result<DashboardRef, String> {
    let mut url = match Url::parse(raw) {
        Ok(url) => url,
        Err(_) => base
            .join(raw)
            .map_err(|e| format!("invalid dashboard URL: {e}"))?,
    };
    if !same_origin(&url, base) {
        return Err("dashboard URL must use the configured Grafana origin".into());
    }
    if grafana_path(&url, base).is_some_and(|path| path.starts_with("/goto/")) {
        let mut seen = HashSet::new();
        for _ in 0..MAX_REDIRECTS {
            if !seen.insert(url.to_string()) {
                return Err("Grafana /goto redirect loop detected".into());
            }
            let mut response = get(url.as_str());
            if (response.status.as_u16() == 401 || response.status.as_u16() == 403)
                && !no_rotate
                && crate::live::rotate(false)
            {
                response = get(url.as_str());
            }
            if response.status.as_u16() == 401 || response.status.as_u16() == 403 {
                return Err(format!(
                    "Grafana authentication failed ({})",
                    response.status
                ));
            }
            if !response.status.is_redirection() {
                return Err(format!(
                    "Grafana /goto returned HTTP {} instead of a dashboard redirect",
                    response.status
                ));
            }
            let location = response
                .location
                .ok_or("Grafana redirect has no Location header")?;
            let next = url
                .join(&location)
                .map_err(|e| format!("invalid Grafana redirect: {e}"))?;
            if !same_origin(&next, base) {
                return Err(
                    "refusing cross-origin Grafana redirect; credentials were not forwarded".into(),
                );
            }
            url = next;
            if grafana_path(&url, base).is_some_and(|path| path.starts_with("/d/")) {
                break;
            }
            if grafana_path(&url, base).is_some_and(|path| path.starts_with("/login")) {
                return Err("Grafana redirect ended at /login; refresh credentials".into());
            }
        }
    }
    parse_dashboard_url(url, base)
}

fn parse_dashboard_url(url: Url, base: &Url) -> Result<DashboardRef, String> {
    if !same_origin(&url, base) {
        return Err("dashboard URL must use the configured Grafana origin".into());
    }
    let path = grafana_path(&url, base)
        .ok_or("dashboard URL is outside the configured Grafana subpath")?;
    if path.starts_with("/login") {
        return Err("Grafana URL resolved to /login; refresh credentials".into());
    }
    let parts: Vec<String> = path
        .trim_start_matches('/')
        .split('/')
        .map(str::to_string)
        .collect();
    if parts.len() < 3 || parts[0] != "d" || parts[1].is_empty() {
        return Err("URL did not resolve to /d/<uid>/<slug>".into());
    }
    let mut panel_id = None;
    let mut org_id = None;
    let mut variables = BTreeMap::new();
    let mut from = None;
    let mut to = None;
    for (key, value) in url.query_pairs() {
        if key == "viewPanel" {
            panel_id = Some(parse_panel_id(&value)?);
        } else if key == "orgId" {
            org_id = Some(value.into_owned());
        } else if let Some(name) = key.strip_prefix("var-") {
            if !name.is_empty() {
                variables.insert(name.to_string(), value.into_owned());
            }
        } else if key == "from" {
            from = Some(value.into_owned());
        } else if key == "to" {
            to = Some(value.into_owned());
        }
    }
    Ok(DashboardRef {
        uid: parts[1].clone(),
        slug: parts[2].clone(),
        org_id,
        panel_id,
        variables,
        from,
        to,
    })
}

fn parse_panel_id(raw: &str) -> Result<i64, String> {
    raw.strip_prefix("panel-")
        .unwrap_or(raw)
        .parse::<i64>()
        .map_err(|_| format!("invalid panel ID {raw:?}"))
}

fn collect_panels(dashboard: &Value) -> Vec<Panel> {
    fn walk(value: &Value, seen: &mut BTreeSet<i64>, out: &mut Vec<Panel>) {
        if let Some(items) = value.as_array() {
            for item in items {
                if let Some(id) = item.get("id").and_then(Value::as_i64)
                    && seen.insert(id)
                {
                    out.push(Panel {
                        id,
                        title: string_at(item, "title").unwrap_or("(untitled)").to_string(),
                        kind: string_at(item, "type").unwrap_or("unknown").to_string(),
                        unit: item
                            .pointer("/fieldConfig/defaults/unit")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        datasource: item.get("datasource").cloned().unwrap_or(Value::Null),
                        targets: item
                            .get("targets")
                            .and_then(Value::as_array)
                            .cloned()
                            .unwrap_or_default(),
                    });
                }
                if let Some(nested) = item.get("panels") {
                    walk(nested, seen, out);
                }
            }
        }
    }
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    walk(
        dashboard.get("panels").unwrap_or(&Value::Null),
        &mut seen,
        &mut out,
    );
    out
}

fn queryable(panel: &Panel) -> bool {
    panel.kind != "row"
        && !panel.datasource.is_null()
        && panel.targets.iter().any(|t| {
            !t.get("hide").and_then(Value::as_bool).unwrap_or(false)
                && string_at(t, "expr").is_some_and(|s| !s.trim().is_empty())
        })
}

fn select_panel(
    panels: &[Panel],
    explicit: Option<&str>,
    from_url: Option<i64>,
) -> Result<Panel, String> {
    let candidates: Vec<&Panel> = panels.iter().filter(|p| queryable(p)).collect();
    let wanted = explicit
        .map(str::to_string)
        .or_else(|| from_url.map(|id| id.to_string()));
    if let Some(wanted) = wanted {
        if let Ok(id) = parse_panel_id(&wanted) {
            let panel = panels
                .iter()
                .find(|p| p.id == id)
                .ok_or_else(|| panel_error(&candidates, &format!("panel {id} not found")))?;
            return queryable(panel)
                .then(|| panel.clone())
                .ok_or_else(|| format!("panel {id} is not queryable"));
        }
        let matches: Vec<&&Panel> = candidates.iter().filter(|p| p.title == wanted).collect();
        return match matches.as_slice() {
            [panel] => Ok((**panel).clone()),
            [] => Err(panel_error(
                &candidates,
                &format!("panel title {wanted:?} not found"),
            )),
            many => Err(format!(
                "panel title {wanted:?} is ambiguous; matching IDs: {}",
                many.iter()
                    .map(|p| p.id.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        };
    }
    match candidates.as_slice() {
        [panel] => Ok((*panel).clone()),
        _ => Err(panel_error(&candidates, "select a panel with --panel")),
    }
}

fn panel_error(panels: &[&Panel], message: &str) -> String {
    let mut list: Vec<String> = panels
        .iter()
        .map(|p| format!("{} — {}", p.id, p.title))
        .collect();
    list.sort();
    format!("{message}. Queryable panels:\n  {}", list.join("\n  "))
}

fn json_value(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => (!s.is_empty()).then(|| s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Array(a) => {
            let values: Vec<String> = a.iter().filter_map(json_value).collect();
            (!values.is_empty()).then(|| values.join("|"))
        }
        _ => None,
    }
}

fn resolve_variables(
    dashboard: &Value,
    url: &BTreeMap<String, String>,
    cli: &[String],
    step_seconds: i64,
    range_seconds: i64,
) -> Result<BTreeMap<String, String>, String> {
    let mut values = BTreeMap::new();
    if let Some(vars) = dashboard
        .pointer("/templating/list")
        .and_then(Value::as_array)
    {
        for var in vars {
            let Some(name) = string_at(var, "name") else {
                continue;
            };
            let value = var
                .pointer("/current/value")
                .and_then(json_value)
                .or_else(|| {
                    var.get("options")
                        .and_then(Value::as_array)
                        .and_then(|options| {
                            options
                                .iter()
                                .find(|o| o.get("selected").and_then(Value::as_bool) == Some(true))
                                .and_then(|o| o.get("value"))
                                .and_then(json_value)
                        })
                });
            if let Some(value) = value {
                values.insert(name.to_string(), value);
            }
        }
    }
    values.extend(url.clone());
    for raw in cli {
        let (name, value) = raw
            .split_once('=')
            .ok_or_else(|| format!("invalid --var {raw:?}; expected NAME=VALUE"))?;
        if name.is_empty() {
            return Err("--var name cannot be empty".into());
        }
        values.insert(name.to_string(), value.to_string());
    }
    values.extend([
        ("__interval".into(), format!("{step_seconds}s")),
        ("__rate_interval".into(), format!("{step_seconds}s")),
        ("__range".into(), format!("{range_seconds}s")),
        ("__all".into(), ".*".into()),
    ]);
    Ok(values)
}

fn interpolate(input: &str, values: &BTreeMap<String, String>) -> Result<String, String> {
    let unsupported = regex::Regex::new(r"\$\{([A-Za-z_][A-Za-z0-9_]*):[^}]+\}").unwrap();
    if let Some(m) = unsupported.find(input) {
        return Err(format!(
            "unsupported Grafana interpolation {:?}; v1 supports $Name and ${{Name}}",
            m.as_str()
        ));
    }
    let pattern =
        regex::Regex::new(r"\$\{([A-Za-z_][A-Za-z0-9_]*)\}|\$([A-Za-z_][A-Za-z0-9_]*)").unwrap();
    let mut result = input.to_string();
    for _ in 0..16 {
        let missing: BTreeSet<String> = pattern
            .captures_iter(&result)
            .filter_map(|c| {
                c.get(1)
                    .or_else(|| c.get(2))
                    .map(|m| m.as_str().to_string())
            })
            .filter(|name| !values.contains_key(name))
            .collect();
        if !missing.is_empty() {
            return Err(format!(
                "unresolved dashboard variables: {}. Supply {}",
                missing.iter().cloned().collect::<Vec<_>>().join(", "),
                missing
                    .iter()
                    .map(|n| format!("--var {n}=value"))
                    .collect::<Vec<_>>()
                    .join(" ")
            ));
        }
        let next = pattern
            .replace_all(&result, |caps: &regex::Captures<'_>| {
                let name = caps.get(1).or_else(|| caps.get(2)).unwrap().as_str();
                values.get(name).cloned().unwrap_or_default()
            })
            .into_owned();
        if next == result {
            return Ok(result);
        }
        result = next;
    }
    if pattern.is_match(&result) {
        Err("dashboard variable interpolation cycle or excessive nesting".into())
    } else {
        Ok(result)
    }
}

fn datasource_parts(
    value: &Value,
    variables: &BTreeMap<String, String>,
) -> Result<(String, String), String> {
    let (uid, kind) = match value {
        Value::String(uid) => (uid.as_str(), "prometheus"),
        Value::Object(o) => (
            o.get("uid").and_then(Value::as_str).unwrap_or(""),
            o.get("type")
                .and_then(Value::as_str)
                .unwrap_or("prometheus"),
        ),
        _ => return Err("selected panel has no concrete datasource".into()),
    };
    let uid = interpolate(uid, variables)?;
    let kind = interpolate(kind, variables)?;
    if uid.is_empty() {
        return Err("selected panel datasource UID is empty".into());
    }
    if kind == "mixed" || kind == "__expr__" {
        return Err(format!("unsupported panel datasource type {kind:?}"));
    }
    Ok((uid, kind))
}

fn resolve_targets(
    panel: &Panel,
    panel_ds: &(String, String),
    variables: &BTreeMap<String, String>,
) -> Result<Vec<Target>, String> {
    let mut out = Vec::new();
    for raw in &panel.targets {
        if raw.get("hide").and_then(Value::as_bool).unwrap_or(false) {
            continue;
        }
        let Some(expr) = string_at(raw, "expr").filter(|s| !s.trim().is_empty()) else {
            continue;
        };
        if let Some(target_ds) = raw.get("datasource") {
            let resolved = datasource_parts(target_ds, variables)?;
            if resolved.0 != panel_ds.0 || !resolved.1.eq_ignore_ascii_case(&panel_ds.1) {
                return Err(
                    "mixed or multiple target datasources are unsupported in dashboard v1".into(),
                );
            }
        }
        out.push(Target {
            ref_id: string_at(raw, "refId").unwrap_or("?").to_string(),
            expression: interpolate(expr, variables)?,
            legend_format: string_at(raw, "legendFormat").map(str::to_string),
        });
    }
    if out.is_empty() {
        Err("selected panel has no enabled PromQL targets".into())
    } else {
        Ok(out)
    }
}

fn now_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn parse_time(raw: &str, now: i64) -> Result<i64, String> {
    let raw = raw.trim();
    if raw == "now" || raw.is_empty() {
        return Ok(now);
    }
    if let Some(duration) = raw.strip_prefix("now-") {
        let secs = crate::live::parse_compound_secs(duration)
            .ok_or_else(|| format!("unsupported Grafana date math {raw:?}; use --start/--since"))?;
        return Ok(now - secs);
    }
    if raw.chars().all(|c| c.is_ascii_digit()) {
        let n = raw
            .parse::<i64>()
            .map_err(|_| format!("invalid timestamp {raw:?}"))?;
        return Ok(if raw.len() >= 13 { n / 1000 } else { n });
    }
    if let Ok(dt) = DateTime::parse_from_rfc3339(raw) {
        return Ok(dt.timestamp());
    }
    for fmt in [
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%dT%H:%M",
    ] {
        if let Ok(dt) = NaiveDateTime::parse_from_str(raw, fmt) {
            return Ok(dt.and_utc().timestamp());
        }
    }
    if let Ok(date) = NaiveDate::parse_from_str(raw, "%Y-%m-%d") {
        return Ok(date.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp());
    }
    Err(format!("cannot parse time {raw:?}"))
}

fn resolve_range(
    since: Option<&str>,
    start: Option<&str>,
    end: Option<&str>,
    url_from: Option<&str>,
    url_to: Option<&str>,
    step: Option<&str>,
) -> Result<(i64, i64, i64), String> {
    let now = now_seconds();
    let cli_time = since.is_some() || start.is_some() || end.is_some();
    let end_s = parse_time(
        if cli_time {
            end.unwrap_or("now")
        } else {
            url_to.unwrap_or("now")
        },
        now,
    )?;
    let start_s = if let Some(start) = start {
        parse_time(start, now)?
    } else if let Some(since) = since {
        end_s - positive_duration(since, "--since")?
    } else if !cli_time {
        match url_from {
            Some(from) => parse_time(from, now)?,
            None => end_s - 3600,
        }
    } else {
        end_s - 3600
    };
    if start_s >= end_s {
        return Err("range start must be before end".into());
    }
    let range = end_s - start_s;
    let step_s = match step {
        Some(step) => positive_duration(step, "--step")?,
        None => ((range + 499) / 500).clamp(1, 3600),
    };
    if step_s > range {
        return Err(format!("--step {step_s}s exceeds range {range}s"));
    }
    Ok((start_s, end_s, step_s))
}

fn positive_duration(raw: &str, flag: &str) -> Result<i64, String> {
    let value = crate::live::parse_compound_secs(raw)
        .ok_or_else(|| format!("invalid {flag} duration {raw:?}"))?;
    if value <= 0 {
        Err(format!("{flag} must be positive"))
    } else {
        Ok(value)
    }
}

fn query_prometheus(
    base: &Url,
    uid: &str,
    expression: &str,
    start: i64,
    end: i64,
    step: i64,
    org_id: Option<&str>,
) -> Result<Vec<Series>, String> {
    let url = grafana_url(
        base,
        &format!("/api/datasources/proxy/uid/{uid}/api/v1/query_range"),
    );
    let start = start.to_string();
    let end = end.to_string();
    let step = format!("{step}s");
    let mut params = vec![
        ("query", expression),
        ("start", start.as_str()),
        ("end", end.as_str()),
        ("step", step.as_str()),
    ];
    if let Some(org_id) = org_id {
        params.push(("orgId", org_id));
    }
    let value = crate::live::authenticated_json_for_org(url.as_str(), &params, org_id);
    parse_prometheus(&value)
}

fn parse_prometheus(value: &Value) -> Result<Vec<Series>, String> {
    if string_at(value, "status") != Some("success") {
        return Err(format!(
            "Prometheus query failed ({}): {}",
            string_at(value, "errorType").unwrap_or("unknown"),
            string_at(value, "error").unwrap_or("unknown error")
        ));
    }
    let result_type = value
        .pointer("/data/resultType")
        .and_then(Value::as_str)
        .unwrap_or("");
    if result_type != "matrix" {
        return Err(format!(
            "Prometheus range query returned unsupported result type {result_type:?}"
        ));
    }
    let results = value
        .pointer("/data/result")
        .and_then(Value::as_array)
        .ok_or("Prometheus response has no data.result array")?;
    let mut out = Vec::new();
    for result in results {
        let labels = result
            .get("metric")
            .and_then(Value::as_object)
            .map(ordered_labels)
            .unwrap_or_default();
        let values = result
            .get("values")
            .and_then(Value::as_array)
            .ok_or("Prometheus series has no values array")?;
        let mut samples = Vec::new();
        for pair in values {
            let pair = pair
                .as_array()
                .filter(|p| p.len() >= 2)
                .ok_or("malformed Prometheus sample")?;
            let raw = pair[1]
                .as_str()
                .ok_or("Prometheus sample value is not a string")?;
            let sample = raw
                .parse::<f64>()
                .map_err(|_| format!("invalid Prometheus sample value {raw:?}"))?;
            if !sample.is_finite() {
                return Err(format!("non-finite Prometheus sample value {raw:?}"));
            }
            samples.push(sample);
        }
        if !samples.is_empty() {
            let sum: f64 = samples.iter().sum();
            out.push(Series {
                labels,
                summary: Summary {
                    first: samples[0],
                    last: *samples.last().unwrap(),
                    min: samples.iter().copied().fold(f64::INFINITY, f64::min),
                    average: sum / samples.len() as f64,
                    max: samples.iter().copied().fold(f64::NEG_INFINITY, f64::max),
                    point_count: samples.len(),
                },
            });
        }
    }
    out.sort_by_key(|s| label_set(&s.labels));
    Ok(out)
}

fn ordered_labels(map: &Map<String, Value>) -> BTreeMap<String, String> {
    map.iter()
        .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
        .collect()
}

fn label_set(labels: &BTreeMap<String, String>) -> String {
    format!(
        "{{{}}}",
        labels
            .iter()
            .map(|(k, v)| format!("{k}=\"{v}\""))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn legend(template: Option<&str>, labels: &BTreeMap<String, String>) -> String {
    let Some(template) = template.filter(|s| !s.is_empty()) else {
        return label_set(labels);
    };
    let re = regex::Regex::new(r"\{\{\s*([^{} ]+)\s*\}\}").unwrap();
    re.replace_all(template, |caps: &regex::Captures<'_>| {
        labels
            .get(&caps[1])
            .cloned()
            .unwrap_or_else(|| caps[0].to_string())
    })
    .into_owned()
}

fn format_value(value: f64, unit: Option<&str>) -> String {
    if matches!(unit, Some("s" | "seconds")) {
        if value.abs() < 0.001 {
            format!("{:.3} µs", value * 1_000_000.0)
        } else if value.abs() < 1.0 {
            format!("{:.3} ms", value * 1000.0)
        } else {
            format!("{value:.3} s")
        }
    } else {
        format!("{value:.6}")
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_string()
    }
}

fn print_text(output: &DashboardOutput, query_only: bool) {
    println!("Dashboard:  {}", output.dashboard.title);
    println!("Panel:      {} — {}", output.panel.id, output.panel.title);
    println!("Datasource: {}", output.datasource.name);
    println!("Range:      {} .. {}", output.range.start, output.range.end);
    println!("Step:       {}s", output.range.step_seconds);
    for target in &output.targets {
        println!(
            "\nTarget {}:\n  {}",
            target.ref_id,
            target.expression.replace('\n', "\n  ")
        );
        if query_only {
            continue;
        }
        println!("\nSeries:");
        if target.series.is_empty() {
            println!("  no data");
        }
        for series in &target.series {
            let s = &series.summary;
            println!(
                "  {}  last={}  min={}  sample_avg={}  max={}  points={}",
                legend(target.legend_format.as_deref(), &series.labels),
                format_value(s.last, output.panel.unit.as_deref()),
                format_value(s.min, output.panel.unit.as_deref()),
                format_value(s.average, output.panel.unit.as_deref()),
                format_value(s.max, output.panel.unit.as_deref()),
                s.point_count
            );
        }
    }
}

fn iso_seconds(seconds: i64) -> String {
    Utc.timestamp_opt(seconds, 0)
        .single()
        .unwrap_or_else(Utc::now)
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn string_at<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn base() -> Url {
        Url::parse("https://grafana.example.com").unwrap()
    }

    #[test]
    fn parses_direct_dashboard_url_and_variables() {
        let r = parse_dashboard_url(Url::parse("https://grafana.example.com/d/uid/slug?orgId=1&viewPanel=panel-21&var-Namespace=production&from=now-2d&to=now").unwrap(), &base()).unwrap();
        assert_eq!(r.uid, "uid");
        assert_eq!(r.org_id.as_deref(), Some("1"));
        assert_eq!(r.panel_id, Some(21));
        assert_eq!(r.variables["Namespace"], "production");
        assert_eq!(r.from.as_deref(), Some("now-2d"));
    }

    #[test]
    fn rejects_other_origin() {
        assert!(
            parse_dashboard_url(Url::parse("https://evil.example/d/u/s").unwrap(), &base())
                .is_err()
        );
    }

    #[test]
    fn follows_same_origin_goto_redirect() {
        let mut requests = Vec::new();
        let reference = resolve_reference_with("/goto/key", &base(), true, |url| {
            requests.push(url.to_string());
            crate::live::GetResponse {
                status: reqwest::StatusCode::FOUND,
                location: Some("/d/uid/slug?viewPanel=panel-21&var-Namespace=production".into()),
                body: String::new(),
            }
        })
        .unwrap();
        assert_eq!(requests, ["https://grafana.example.com/goto/key"]);
        assert_eq!(reference.uid, "uid");
        assert_eq!(reference.panel_id, Some(21));
    }

    #[test]
    fn refuses_cross_origin_redirect_before_following_it() {
        let mut requests = Vec::new();
        let error = resolve_reference_with("/goto/key", &base(), true, |url| {
            requests.push(url.to_string());
            crate::live::GetResponse {
                status: reqwest::StatusCode::FOUND,
                location: Some("https://evil.example/d/uid/slug".into()),
                body: String::new(),
            }
        })
        .unwrap_err();
        assert_eq!(requests.len(), 1);
        assert!(error.contains("credentials were not forwarded"));
    }

    #[test]
    fn origin_includes_effective_port() {
        assert!(same_origin(
            &Url::parse("https://grafana.example.com/a").unwrap(),
            &Url::parse("https://grafana.example.com:443/b").unwrap()
        ));
        assert!(!same_origin(
            &Url::parse("https://grafana.example.com/a").unwrap(),
            &Url::parse("https://grafana.example.com:8443/b").unwrap()
        ));
    }

    #[test]
    fn recursively_collects_and_selects_panels() {
        let d = json!({"panels":[{"id":1,"type":"row","panels":[{"id":21,"title":"Latency","type":"timeseries","datasource":{"uid":"prom"},"targets":[{"refId":"B","expr":"up"}]}]}]});
        let panels = collect_panels(&d);
        assert_eq!(
            select_panel(&panels, Some("panel-21"), None).unwrap().title,
            "Latency"
        );
        assert_eq!(select_panel(&panels, Some("Latency"), None).unwrap().id, 21);
    }

    #[test]
    fn interpolates_without_changing_promql() {
        let values = BTreeMap::from([
            (
                "services".into(),
                "example-service|background-worker".into(),
            ),
            ("Namespace".into(), "production".into()),
        ]);
        let q = "max(metric{module=~\"$services\",quantile=\"1.0\",namespace=~\"${Namespace}\"}) by (method) > 0";
        assert_eq!(
            interpolate(q, &values).unwrap(),
            "max(metric{module=~\"example-service|background-worker\",quantile=\"1.0\",namespace=~\"production\"}) by (method) > 0"
        );
    }

    #[test]
    fn reports_unresolved_and_unsupported_variables() {
        assert!(
            interpolate("$Missing", &BTreeMap::new())
                .unwrap_err()
                .contains("--var Missing=value")
        );
        assert!(
            interpolate("${Name:regex}", &BTreeMap::new())
                .unwrap_err()
                .contains("unsupported")
        );
    }

    #[test]
    fn variable_precedence_is_cli_then_url_then_dashboard() {
        let dashboard = json!({"templating":{"list":[
            {"name":"Namespace","current":{"value":"qa"}},
            {"name":"services","options":[{"selected":true,"value":["a","b"]}]}
        ]}});
        let url = BTreeMap::from([("Namespace".into(), "staging".into())]);
        let vars = resolve_variables(&dashboard, &url, &["Namespace=production".into()], 30, 3600)
            .unwrap();
        assert_eq!(vars["Namespace"], "production");
        assert_eq!(vars["services"], "a|b");
    }

    #[test]
    fn resolves_grafana_builtins_and_all_values() {
        let dashboard = json!({"templating":{"list":[
            {"name":"services","current":{"value":"$__all"}}
        ]}});
        let vars = resolve_variables(&dashboard, &BTreeMap::new(), &[], 30, 3600).unwrap();
        let query =
            "rate(requests_total[$__rate_interval]) and $__interval and $__range and $services";
        assert_eq!(
            interpolate(query, &vars).unwrap(),
            "rate(requests_total[30s]) and 30s and 3600s and .*"
        );
    }

    #[test]
    fn preserves_grafana_subpath_in_api_and_dashboard_urls() {
        let base = Url::parse("https://grafana.example.com/grafana").unwrap();
        assert_eq!(
            grafana_url(&base, "/api/dashboards/uid/abc").as_str(),
            "https://grafana.example.com/grafana/api/dashboards/uid/abc"
        );
        let reference = parse_dashboard_url(
            Url::parse("https://grafana.example.com/grafana/d/abc/stats").unwrap(),
            &base,
        )
        .unwrap();
        assert_eq!(reference.uid, "abc");
    }

    #[test]
    fn follows_subpath_goto_redirect() {
        let base = Url::parse("https://grafana.example.com/grafana").unwrap();
        let reference = resolve_reference_with("/grafana/goto/key", &base, true, |_| {
            crate::live::GetResponse {
                status: reqwest::StatusCode::FOUND,
                location: Some("/grafana/d/uid/slug".into()),
                body: String::new(),
            }
        })
        .unwrap();
        assert_eq!(reference.uid, "uid");
    }

    #[test]
    fn resolves_range_and_derived_step() {
        let (start, end, step) =
            resolve_range(Some("2d"), None, Some("172800"), None, None, None).unwrap();
        assert_eq!((start, end, step), (0, 172800, 346));
    }

    #[test]
    fn parses_matrix_summary_without_points() {
        let v = json!({"status":"success","data":{"resultType":"matrix","result":[{"metric":{"b":"2","a":"1"},"values":[[1,"1"],[2,"3"]]}]}});
        let series = parse_prometheus(&v).unwrap();
        assert_eq!(series[0].summary.average, 2.0);
        assert_eq!(series[0].summary.point_count, 2);
        assert_eq!(label_set(&series[0].labels), "{a=\"1\", b=\"2\"}");
        let json = serde_json::to_string(&series).unwrap();
        assert!(!json.contains("points"));
    }

    #[test]
    fn rejects_non_finite_samples() {
        let v = json!({"status":"success","data":{"resultType":"matrix","result":[{"metric":{},"values":[[1,"NaN"]]}]}});
        assert!(parse_prometheus(&v).unwrap_err().contains("non-finite"));
    }

    #[test]
    fn preserves_prometheus_error_fields() {
        let v = json!({"status":"error","errorType":"bad_data","error":"invalid query"});
        assert_eq!(
            parse_prometheus(&v).unwrap_err(),
            "Prometheus query failed (bad_data): invalid query"
        );
    }

    #[test]
    fn renders_seconds() {
        assert_eq!(format_value(0.00823, Some("s")), "8.230 ms");
        assert_eq!(format_value(1.14079, Some("s")), "1.141 s");
    }
}

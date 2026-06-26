use clap::{Args, Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "gflog",
    about = "Parse Grafana/Loki logs — offline JSON exports and live LogQL queries",
    version
)]
pub struct Cli {
    #[command(subcommand)]
    pub source: Source,
}

#[derive(Args)]
pub struct Conn {
    /// Loki datasource name substring if multiple exist (e.g. QA, STG, PROD)
    #[arg(long, global = true)]
    pub datasource: Option<String>,
    /// re-discover datasource id
    #[arg(long, global = true)]
    pub refresh_datasource: bool,
    /// skip the session-cookie keepalive rotation before querying (rotation is on by
    /// default; ignored when a Bearer token is in use)
    #[arg(long, global = true)]
    pub no_rotate: bool,
}

#[derive(Args)]
pub struct Filter {
    /// keep only records whose service contains this substring (case-insensitive)
    #[arg(long, global = true)]
    pub service: Option<String>,
    /// keep only records whose logger contains this substring (case-insensitive)
    #[arg(long, global = true)]
    pub logger: Option<String>,
    /// keep only records whose message+stack contains this substring (case-insensitive)
    #[arg(long = "match", global = true)]
    pub match_: Option<String>,
    /// keep only records at/after this ISO timestamp prefix (e.g. 2026-06-04T09:30)
    #[arg(long, global = true)]
    pub after: Option<String>,
    /// keep only records at/before this ISO timestamp prefix
    #[arg(long, global = true)]
    pub before: Option<String>,
}

#[derive(Args)]
pub struct Ns {
    /// scope the stream selector to these namespaces (repeatable or comma-separated); injects namespace=~"a|b"
    #[arg(
        long = "namespace",
        visible_alias = "ns",
        global = true,
        value_delimiter = ','
    )]
    pub namespace: Vec<String>,
}

#[derive(Args)]
pub struct TimeRange {
    /// lookback window (30m, 2h, 1d). Default 1h. Ignored if --start given
    #[arg(long, default_value = "1h")]
    pub since: String,
    /// absolute start: RFC3339 (Z/±HH:MM honored; bare ISO with no offset = UTC) or epoch (s/ns)
    #[arg(long)]
    pub start: Option<String>,
    /// absolute end: RFC3339 (no offset = UTC), epoch, or 'now'. Default now
    #[arg(long, default_value = "now")]
    pub end: String,
}

#[derive(Subcommand)]
pub enum Source {
    /// Parse a downloaded Grafana/Loki JSON export
    File {
        /// export path/glob (default: newest Logs-*/Explore-logs-* in ~/Downloads); '-' reads stdin
        #[arg(short, long)]
        file: Option<String>,
        /// emit machine-readable JSON instead of compact text
        #[arg(long, global = true)]
        json: bool,
        #[command(flatten)]
        filter: Filter,
        #[command(subcommand)]
        view: View,
    },
    /// Live LogQL log query against Grafana/Loki (run `gflog cookie` first)
    Live {
        /// LogQL log query, e.g. '{service_name="my-service"} |= "error"'
        #[arg(short, long)]
        query: String,
        /// max log lines pulled from Loki
        #[arg(long, default_value_t = 1000)]
        limit: usize,
        /// emit machine-readable JSON instead of compact text
        #[arg(long, global = true)]
        json: bool,
        #[command(flatten)]
        time: TimeRange,
        #[command(flatten)]
        conn: Conn,
        #[command(flatten)]
        ns: Ns,
        #[command(flatten)]
        filter: Filter,
        #[command(subcommand)]
        view: View,
    },
    /// Run a LogQL metric query (count_over_time/rate/sum...) and render the timeseries
    Metric {
        /// LogQL metric query, e.g. 'sum by (level) (count_over_time({service_name="x"}[5m]))'
        #[arg(short, long)]
        query: String,
        /// step between data points (range query resolution)
        #[arg(long, default_value = "1m")]
        step: String,
        /// emit machine-readable JSON instead of compact text
        #[arg(long, global = true)]
        json: bool,
        #[command(flatten)]
        time: TimeRange,
        #[command(flatten)]
        conn: Conn,
        #[command(flatten)]
        ns: Ns,
    },
    /// List the label names present in the time window (discovery)
    Labels {
        /// emit machine-readable JSON instead of one-per-line
        #[arg(long, global = true)]
        json: bool,
        #[command(flatten)]
        time: TimeRange,
        #[command(flatten)]
        conn: Conn,
        #[command(flatten)]
        ns: Ns,
    },
    /// List values for a label, e.g. `values service_name` to see all services (discovery)
    Values {
        /// label name to enumerate (service_name, namespace, level, pod, app, ...)
        label: String,
        /// optional stream selector to scope values, e.g. '{namespace="prod"}'
        #[arg(short, long)]
        query: Option<String>,
        /// emit machine-readable JSON instead of one-per-line
        #[arg(long, global = true)]
        json: bool,
        #[command(flatten)]
        time: TimeRange,
        #[command(flatten)]
        conn: Conn,
        #[command(flatten)]
        ns: Ns,
    },
    /// Set / refresh / test the grafana_session cookie
    Cookie {
        /// cookie value or full 'grafana_session=...' string; omit to read clipboard
        value: Option<String>,
        /// read the value from stdin instead of the clipboard
        #[arg(long)]
        stdin: bool,
        /// only test the current cookie, don't change it
        #[arg(long)]
        test: bool,
    },
    /// Set / test a service-account Bearer token (preferred; decoupled from any browser session)
    Token {
        /// token value (a leading 'Bearer ' is stripped); omit to read clipboard
        value: Option<String>,
        /// read the value from stdin instead of the clipboard
        #[arg(long)]
        stdin: bool,
        /// only test the current token, don't change it
        #[arg(long)]
        test: bool,
        /// remove the saved token file (revert to cookie auth)
        #[arg(long)]
        clear: bool,
    },
    /// Set / show the Grafana base URL (alternative to the $GRAFANA_URL env var)
    Url {
        /// URL to save, e.g. https://grafana.example.com; omit to print the current one
        value: Option<String>,
    },
    /// Rotate the session token to keep it alive
    Keepalive,
}

#[derive(Subcommand)]
pub enum View {
    /// counts by level/service/logger + top error patterns. START HERE.
    Summary,
    /// list ERROR/FATAL (--warn adds WARN, --level X,Y sets exactly)
    Errors {
        #[arg(long)]
        warn: bool,
        /// comma list e.g. ERROR,WARN
        #[arg(long)]
        level: Option<String>,
    },
    /// regex over message+stack (client-side; prefer LogQL filters when live)
    Grep {
        pattern: String,
        #[arg(short = 'i', long)]
        ignore_case: bool,
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// all records for a traceId (prefix ok), time-ordered
    Trace { trace_id: String },
    /// full record by index incl stack_trace
    Show { index: usize },
    /// cluster ALL messages by normalized signature; top-N noisiest patterns
    Patterns {
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// restrict to these levels, comma list e.g. ERROR,WARN
        #[arg(long)]
        level: Option<String>,
    },
    /// histogram of record volume over time (with ERROR overlay) — spot spikes
    Timeline {
        #[arg(long, default_value_t = 24)]
        buckets: usize,
    },
}

#![allow(clippy::too_many_arguments, clippy::type_complexity)]

mod cli;
mod live;
mod offline;
mod record;
mod views;

use clap::Parser;
use cli::{Cli, Source};

pub fn die(msg: &str) -> ! {
    eprintln!("{msg}");
    std::process::exit(1);
}

fn main() {
    let cli = Cli::parse();
    let json = cli.json;
    let full = cli.full;
    match cli.source {
        Source::File { file, filter, view } => {
            let (recs, name) = if file.as_deref() == Some("-") {
                (offline::load_stdin(), "stdin".to_string())
            } else {
                let path = offline::resolve(file.as_deref());
                let name = path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("?")
                    .to_string();
                (offline::load(&path), name)
            };
            let total = recs.len();
            let recs = views::apply_filter(recs, &filter);
            if recs.len() == total {
                eprintln!("# {name}  ({total} records)\n");
            } else {
                eprintln!(
                    "# {name}  ({} of {total} records after filter)\n",
                    recs.len()
                );
            }
            views::dispatch(&recs, &view, json, full);
        }
        Source::Live {
            query,
            limit,
            time,
            conn,
            ns,
            filter,
            view,
        } => {
            live::run_live(&query, &ns, limit, json, full, &time, &conn, &filter, &view);
        }
        Source::Metric {
            query,
            step,
            time,
            conn,
            ns,
        } => {
            live::run_metric(&query, &ns, &step, json, &time, &conn);
        }
        Source::Labels { time, conn, ns } => live::run_labels(&ns, json, &time, &conn),
        Source::Values {
            label,
            query,
            time,
            conn,
            ns,
        } => {
            live::run_values(&label, query.as_deref(), &ns, json, &time, &conn);
        }
        Source::Cookie { value, stdin, test } => live::cmd_cookie(value, stdin, test),
        Source::Token {
            value,
            stdin,
            test,
            clear,
        } => live::cmd_token(value, stdin, test, clear),
        Source::Url { value } => live::cmd_url(value),
        Source::Keepalive => std::process::exit(if live::rotate(true) { 0 } else { 1 }),
    }
}

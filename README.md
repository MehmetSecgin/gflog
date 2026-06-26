# gflog

[![CI](https://github.com/MehmetSecgin/gflog/actions/workflows/ci.yml/badge.svg)](https://github.com/MehmetSecgin/gflog/actions/workflows/ci.yml)

A small CLI for reading Grafana/Loki logs from the terminal — both **downloaded JSON
exports** and **live LogQL queries** — without drowning in raw log lines. It normalizes
records to `ts, level, service, pod, namespace, logger, thread, trace, msg, stack` and gives
you compact, composable views: summaries, error lists, pattern clustering, timelines, traces.

## Install

One line (needs a Rust toolchain — get one at https://rustup.rs):

```
cargo install --git https://github.com/MehmetSecgin/gflog
```

Or from a local clone: `cargo install --path .`. Either way `gflog` lands on `~/.cargo/bin`;
verify with `gflog --version`.

### Download a prebuilt binary (macOS, Apple Silicon)

No Rust needed. Grab the latest `*-aarch64-apple-darwin.tar.gz` from the
[Releases page](https://github.com/MehmetSecgin/gflog/releases/latest), then:

```
tar -xzf gflog-*-aarch64-apple-darwin.tar.gz
xattr -dr com.apple.quarantine gflog        # clear macOS Gatekeeper quarantine
sudo mv gflog /usr/local/bin/                # or anywhere on your PATH
gflog --version
```

The binary is unsigned, so macOS quarantines it on download — the `xattr` line clears that.
(Building from source via `cargo` above avoids quarantine entirely.)

### Install script (macOS, Apple Silicon)

Interactive — it prints exactly what it will do (download URL, checksum, install path,
sudo, quarantine clear) and waits for your confirmation before changing anything:

```
curl -fsSL https://raw.githubusercontent.com/MehmetSecgin/gflog/main/install.sh | sh
```

As with any `curl | sh`, you're welcome to read it first:

```
curl -fsSL https://raw.githubusercontent.com/MehmetSecgin/gflog/main/install.sh -o install.sh
less install.sh && sh install.sh
```

It verifies the sha256 and refuses to install if the checksum is missing or wrong.
Overrides: `GFLOG_INSTALL_DIR=~/.local/bin`, `GFLOG_VERSION=v0.4.0`, `GFLOG_YES=1` (skip the
prompt for automation).

## Point it at your Grafana

The binary has no built-in host. Set it once:

```
gflog url https://grafana.example.com     # saved to ~/.config/grafana-logs/url
```

or export `GRAFANA_URL` (takes precedence). `gflog url` prints the current value.

The URL must be `https://` (your token/cookie is attached to every request — cleartext is
refused); `http://` is allowed only for `localhost`/`127.0.0.1`.

## Authenticate

A **service-account Bearer token** (preferred) or a **session cookie**:

```
gflog token            # read a token from the clipboard, save, and test
gflog cookie           # read a grafana_session cookie from the clipboard
gflog token --test     # check the current credential
```

Clipboard reads use `pbpaste` (macOS) or `wl-paste`/`xclip`/`xsel` (Linux); if none is
available, pass the value as an argument or pipe it with `--stdin`.

Resolution order: `$GRAFANA_TOKEN` → `~/.config/grafana-logs/token` → cookie file. A token is
decoupled from your browser session; a shared cookie is kept alive by rotation (toggle with
`--no-rotate`).

**Prefer a Bearer token for anything scripted or high-frequency.** A token never rotates, so
it is immune to the cookie-rotation races that can otherwise surface as intermittent empty
results across rapid back-to-back calls. With cookie auth, gflog refreshes once and retries a
single time on an HTTP 401 and writes the rotated cookie atomically; if the refresh still
fails it stops with a clear `auth failed (401)` message rather than returning silent 0 rows.

## Use

```
# offline: newest export in ~/Downloads (or -f PATH/glob/-)
gflog file summary
gflog file -f Logs-2026-01-01.json errors --warn

# live LogQL
gflog live -q '{service_name="my-service"} |= "error"' --since 30m summary
gflog metric -q 'sum by (level)(count_over_time({service_name="my-service"} | logfmt [5m]))' --since 3h
# a TRUE total count needs the [range] to equal --step so windows tile (no overlap):
gflog metric -q 'sum(count_over_time({service_name="my-service"}[1m]))' --step 1m --since 3h
gflog values service_name            # discover what exists
gflog labels
```

### Views (shared by `file` and `live`)
`summary` · `errors [--warn|--level A,B]` · `grep PATTERN [-i]` · `trace TRACE_ID` ·
`show INDEX` · `patterns [--level A,B]` · `timeline [--buckets N]`. Add `--json` to any
command for machine-readable output. Add `--full` to print complete, untruncated
messages and stack traces in the text views (`errors`/`grep`/`trace`/`patterns`/`summary`)
— read N full bodies in one command instead of `show`-ing them one at a time. `--json`
records always carry the complete `msg` and `stack` regardless of `--full`.

### Scoping
- `--datasource SUBSTR` — pick a Loki datasource when several exist (cached after first use).
- `--namespace`/`--ns a,b` — inject `namespace=~"a|b"` into the stream selector (repeatable or
  comma-separated; values restricted to `[A-Za-z0-9._*-]`). An explicit `namespace=...` in your
  `-q` always wins; for a fully custom matcher, write it in `-q` directly.
- `--since 30m` / `--start` / `--end`, `--limit N`, record filters `--service`/`--logger`/
  `--match`/`--after`/`--before`.

### `metric` range vs `--step`
`count_over_time({...}[range])` counts events in the trailing `[range]` at each `--step`. If
`[range]` is larger than `--step`, the windows **overlap** and summing the points over-counts;
if `--step` is larger than `[range]`, windows leave **gaps** and undercount. For a true total
over the window, make them equal — e.g. `[1m]` with `--step 1m`. `gflog` warns on a bad
mismatch. (Compound durations like `2h30m` are parsed correctly for both `[range]` and `--step`.)

Only JSON exports are handled offline; `.txt`/`.csv` are out of scope.

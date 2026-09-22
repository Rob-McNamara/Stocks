//! One-off backfill of the `open`, `high` and `low` columns on the `prices`
//! table.
//!
//! Historical bars were originally ingested close-only, so every pre-existing
//! row has NULL OHLC and the candlestick chart has nothing to draw. This tool
//! re-fetches each symbol from Yahoo at `range=max` and fills the gaps. It is
//! deliberately conservative: it only ever writes rows whose OHLC is already
//! NULL, and it never touches `close`, `volume` or `fetched_at`, so a bad run
//! cannot corrupt the price series the portfolio engine depends on.
//!
//! Usage:
//!   cargo run --bin backfill_ohlc              # every symbol with missing OHLC
//!   cargo run --bin backfill_ohlc -- --symbol BHP.AX
//!   cargo run --bin backfill_ohlc -- --dry-run
//!   cargo run --bin backfill_ohlc -- --limit 50   # one batch; re-run to continue
//!   cargo run --bin backfill_ohlc -- --all        # include already-complete symbols
//!   cargo run --bin backfill_ohlc -- --repair     # re-fetch bars whose open/close lie outside high/low
//!
//! `--repair` is the one mode that overwrites existing OHL. Before
//! `fetch_current_price` took the day's range from the bar, the live-price path
//! paired the bar's open with the chart meta's high/low, which can miss the
//! opening print, and stored bars whose open sat outside their own range. Repair
//! touches only rows that are internally inconsistent, still never writes
//! `close`, and widens the fetched range to contain the stored close so a
//! repaired row cannot be flagged again.

use chrono::{NaiveDate, TimeZone, Utc};
use reqwest::Client;
use rusqlite::{params, Connection};
use serde::Deserialize;
use std::{env, path::PathBuf, time::Duration};
use tokio::time;

/// Open the SQLite database with WAL mode and a busy timeout so this tool can
/// run while the API and price daemon are live.
fn open_db<P: AsRef<std::path::Path>>(path: P) -> Result<Connection, rusqlite::Error> {
    let conn = Connection::open(path)?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    let _: String = conn.query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))?;
    Ok(conn)
}

fn insert_event_log(
    db_path: &PathBuf,
    level: &str,
    event_type: &str,
    symbol: Option<&str>,
    details: &str,
) -> anyhow::Result<()> {
    let conn = open_db(db_path)?;
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO event_log (timestamp, level, source, event_type, symbol, details) VALUES (?1, ?2, 'backfill_ohlc', ?3, ?4, ?5)",
        params![now, level, event_type, symbol, details],
    )?;
    Ok(())
}

#[derive(Debug, Deserialize)]
struct YahooChartResponse {
    chart: YahooChart,
}

#[derive(Debug, Deserialize)]
struct YahooChart {
    result: Option<Vec<YahooResult>>,
    error: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct YahooResult {
    meta: Option<YahooMeta>,
    timestamp: Option<Vec<i64>>,
    indicators: YahooIndicators,
}

#[derive(Debug, Deserialize)]
struct YahooMeta {
    /// Exchange UTC offset in seconds — needed to date bars correctly
    gmtoffset: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct YahooIndicators {
    quote: Vec<YahooQuote>,
}

#[derive(Debug, Deserialize)]
struct YahooQuote {
    open: Option<Vec<Option<f64>>>,
    high: Option<Vec<Option<f64>>>,
    low: Option<Vec<Option<f64>>>,
}

/// Which stored rows a run works on.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Mode {
    /// Rows with any of open/high/low still NULL — fill the gaps only.
    Fill,
    /// Rows whose open or close lies outside their own high/low.
    Repair,
}

impl Mode {
    fn row_filter(self) -> &'static str {
        match self {
            Mode::Fill => "(open IS NULL OR high IS NULL OR low IS NULL)",
            Mode::Repair => "(open > high OR open < low OR close > high OR close < low)",
        }
    }

    fn verb(self) -> &'static str {
        match self {
            Mode::Fill => "filled",
            Mode::Repair => "repaired",
        }
    }
}

/// One day's OHL, keyed by the exchange-local trading date.
struct OhlBar {
    date: String,
    open: Option<f64>,
    high: Option<f64>,
    low: Option<f64>,
}

impl OhlBar {
    fn is_complete(&self) -> bool {
        self.open.is_some() && self.high.is_some() && self.low.is_some()
    }
}

/// Convert a Yahoo bar timestamp to the exchange-local trading date. Yahoo
/// stamps daily bars at the market open, so for exchanges ahead of UTC the UTC
/// date is a day early — the date must be taken in exchange time. This matches
/// `yahoo_local_date` in the API, and getting it wrong would file OHLC against
/// the neighbouring day's close.
fn yahoo_local_date(ts: i64, gmtoffset: Option<i64>) -> Option<NaiveDate> {
    Utc.timestamp_opt(ts + gmtoffset.unwrap_or(0), 0)
        .single()
        .map(|dt| dt.date_naive())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::init();
    let database_path = env::var("DATABASE_PATH").unwrap_or_else(|_| "stocks.db".to_string());
    let db_path = PathBuf::from(&database_path);

    let args: Vec<String> = env::args().collect();
    let dry_run = args.iter().any(|a| a == "--dry-run");
    let include_complete = args.iter().any(|a| a == "--all");
    let mode = if args.iter().any(|a| a == "--repair") { Mode::Repair } else { Mode::Fill };
    if mode == Mode::Repair && include_complete {
        anyhow::bail!("--repair and --all cannot be combined: repair only rewrites inconsistent rows");
    }
    let only_symbol = args
        .iter()
        .position(|a| a == "--symbol")
        .and_then(|i| args.get(i + 1))
        .map(|s| s.trim().to_uppercase());
    // Yahoo rate-limits long runs, so allow the backfill to be done in batches —
    // each run skips what previous runs already filled.
    let limit = args
        .iter()
        .position(|a| a == "--limit")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse::<usize>().ok());

    let mut symbols = load_symbols(&db_path, mode, only_symbol.as_deref(), include_complete)?;
    if let Some(limit) = limit {
        symbols.truncate(limit);
    }
    if symbols.is_empty() {
        log::info!("Nothing to backfill — no symbols matched.");
        return Ok(());
    }
    log::info!(
        "{} OHLC for {} symbol(s){}",
        if mode == Mode::Repair { "Repairing" } else { "Backfilling" },
        symbols.len(),
        if dry_run { " (dry run — no writes)" } else { "" }
    );

    let client = Client::builder()
        .user_agent("stocks-backfill/1.0")
        .build()?;

    let mut total_updated = 0usize;
    let mut failures = 0usize;
    for (index, symbol) in symbols.iter().enumerate() {
        // Only ask Yahoo for the window we actually store rows in — the backfill
        // fills existing bars, so anything outside that span is wasted payload.
        let span = match fillable_span(&db_path, mode, symbol, include_complete)? {
            Some(span) => span,
            None => {
                log::info!("[{}/{}] {}: already complete, skipping", index + 1, symbols.len(), symbol);
                continue;
            }
        };
        match fetch_daily_history(&client, symbol, span.0, span.1).await {
            Ok(bars) => {
                let updated = if dry_run {
                    count_fillable(&db_path, mode, symbol, &bars)?
                } else {
                    apply_bars(&db_path, mode, symbol, &bars)?
                };
                total_updated += updated;
                let span = match (bars.first(), bars.last()) {
                    (Some(first), Some(last)) => format!("{}..{}", first.date, last.date),
                    _ => "empty".to_string(),
                };
                log::info!(
                    "[{}/{}] {}: {} bars fetched ({}), {} row(s) {}",
                    index + 1,
                    symbols.len(),
                    symbol,
                    bars.len(),
                    span,
                    updated,
                    if dry_run { "matching" } else { mode.verb() }
                );
            }
            Err(err) => {
                failures += 1;
                let message = format!("OHLC backfill failed for {}: {}", symbol, err);
                log::warn!("{}", message);
                let _ = insert_event_log(&db_path, "error", "ohlc_backfill", Some(symbol), &message);
            }
        }

        // Yahoo throttles aggressively on long unpaced runs, and a partial
        // backfill that dies at symbol 80 of 200 is worse than a slow one.
        if index + 1 < symbols.len() {
            time::sleep(Duration::from_millis(600)).await;
        }
    }

    log::info!(
        "Backfill complete: {} row(s) {}{}, {} symbol(s) failed.",
        total_updated,
        if dry_run { "would be " } else { "" },
        mode.verb(),
        failures
    );
    if failures > 0 {
        let _ = insert_event_log(
            &db_path,
            "warn",
            "ohlc_backfill",
            None,
            &format!("OHLC backfill finished with {} failed symbol(s)", failures),
        );
    }
    Ok(())
}

/// Symbols to process. By default only those with at least one row the mode
/// applies to, so re-running after a partial failure skips the work already done.
fn load_symbols(
    db_path: &PathBuf,
    mode: Mode,
    only_symbol: Option<&str>,
    include_complete: bool,
) -> anyhow::Result<Vec<String>> {
    let conn = open_db(db_path)?;
    if let Some(symbol) = only_symbol {
        return Ok(vec![symbol.to_string()]);
    }
    let sql = if include_complete {
        "SELECT DISTINCT symbol FROM prices ORDER BY symbol".to_string()
    } else {
        format!("SELECT DISTINCT symbol FROM prices WHERE {} ORDER BY symbol", mode.row_filter())
    };
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Fetch daily bars covering `from..=to`.
///
/// `range=max` cannot be used here: Yahoo silently downsamples it to monthly
/// bars even with `interval=1d` (a 12-year span comes back as ~149 points),
/// so almost nothing would match a stored trading day. Explicit `period1` /
/// `period2` timestamps keep daily granularity.
/// Earliest and latest stored date the mode still applies to for `symbol`, or
/// `None` when there is nothing left to do.
fn fillable_span(
    db_path: &PathBuf,
    mode: Mode,
    symbol: &str,
    include_complete: bool,
) -> anyhow::Result<Option<(NaiveDate, NaiveDate)>> {
    let conn = open_db(db_path)?;
    let sql = if include_complete {
        "SELECT MIN(date), MAX(date) FROM prices WHERE symbol = ?1".to_string()
    } else {
        format!("SELECT MIN(date), MAX(date) FROM prices WHERE symbol = ?1 AND {}", mode.row_filter())
    };
    let span: (Option<String>, Option<String>) =
        conn.query_row(&sql, params![symbol], |row| Ok((row.get(0)?, row.get(1)?)))?;
    let (Some(min), Some(max)) = span else {
        return Ok(None);
    };
    let parse = |s: &str| NaiveDate::parse_from_str(s, "%Y-%m-%d");
    Ok(Some((parse(&min)?, parse(&max)?)))
}

async fn fetch_daily_history(
    client: &Client,
    symbol: &str,
    from: NaiveDate,
    to: NaiveDate,
) -> anyhow::Result<Vec<OhlBar>> {
    // Pad both ends: exchange-local dating can shift a bar by a day, and the
    // window is a half-open range on Yahoo's side.
    let period1 = from
        .checked_sub_days(chrono::Days::new(4))
        .unwrap_or(from)
        .and_hms_opt(0, 0, 0)
        .map(|dt| dt.and_utc().timestamp())
        .unwrap_or(0)
        .max(0);
    let period2 = to
        .checked_add_days(chrono::Days::new(2))
        .unwrap_or(to)
        .and_hms_opt(0, 0, 0)
        .map(|dt| dt.and_utc().timestamp())
        .unwrap_or_else(|| Utc::now().timestamp());

    let url = format!(
        "https://query1.finance.yahoo.com/v8/finance/chart/{}?interval=1d&period1={}&period2={}",
        symbol, period1, period2
    );

    let response = client
        .get(&url)
        .header("User-Agent", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
        .header("Accept", "application/json")
        .send()
        .await?
        .error_for_status()?;

    let payload: YahooChartResponse = response.json().await?;
    let result = payload
        .chart
        .result
        .as_ref()
        .and_then(|items| items.first())
        .ok_or_else(|| match payload.chart.error {
            Some(ref e) => anyhow::anyhow!("no chart result: {}", e),
            None => anyhow::anyhow!("no chart result"),
        })?;

    let timestamps = result
        .timestamp
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("no timestamp data"))?;
    let quote = result
        .indicators
        .quote
        .first()
        .ok_or_else(|| anyhow::anyhow!("no quote data"))?;
    let gmtoffset = result.meta.as_ref().and_then(|m| m.gmtoffset);

    let mut bars = Vec::with_capacity(timestamps.len());
    for (index, ts) in timestamps.iter().enumerate() {
        let Some(date) = yahoo_local_date(*ts, gmtoffset) else {
            continue;
        };
        let open = quote.open.as_ref().and_then(|v| v.get(index).cloned().flatten());
        let high = quote.high.as_ref().and_then(|v| v.get(index).cloned().flatten());
        let low = quote.low.as_ref().and_then(|v| v.get(index).cloned().flatten());
        if open.is_none() && high.is_none() && low.is_none() {
            continue;
        }
        bars.push(OhlBar {
            date: date.format("%Y-%m-%d").to_string(),
            open,
            high,
            low,
        });
    }

    if bars.is_empty() {
        anyhow::bail!("Yahoo returned no OHLC bars");
    }
    Ok(bars)
}

/// Write OHL onto existing rows only. New dates are not inserted — this tool
/// backfills, it does not extend the price series.
///
/// Fill: `COALESCE` keeps any value already present, and the WHERE clause skips
/// rows that are already complete, so re-running is a no-op.
///
/// Repair: overwrites OHL on inconsistent rows with Yahoo's bar, widening high
/// and low to the stored close. The bar's close and the stored close (from the
/// live quote) can differ in the last float digit, and without the widening a
/// repaired row could still fail the check and be rewritten on every run. Bars
/// missing any of O/H/L are skipped, as SQLite's `MAX`/`MIN` return NULL on one.
fn apply_bars(db_path: &PathBuf, mode: Mode, symbol: &str, bars: &[OhlBar]) -> anyhow::Result<usize> {
    let mut conn = open_db(db_path)?;
    let tx = conn.transaction()?;
    let mut updated = 0usize;
    {
        let sql = match mode {
            Mode::Fill => format!(
                "UPDATE prices
                    SET open = COALESCE(open, ?3),
                        high = COALESCE(high, ?4),
                        low  = COALESCE(low,  ?5)
                  WHERE symbol = ?1 AND date = ?2 AND {}",
                mode.row_filter()
            ),
            Mode::Repair => format!(
                "UPDATE prices
                    SET open = ?3,
                        high = MAX(?4, ?3, close),
                        low  = MIN(?5, ?3, close)
                  WHERE symbol = ?1 AND date = ?2 AND {}",
                mode.row_filter()
            ),
        };
        let mut stmt = tx.prepare(&sql)?;
        for bar in bars.iter().filter(|b| mode == Mode::Fill || b.is_complete()) {
            updated += stmt.execute(params![symbol, bar.date, bar.open, bar.high, bar.low])?;
        }
    }
    tx.commit()?;
    Ok(updated)
}

/// Dry-run counterpart to `apply_bars` — how many stored rows the fetched bars
/// would actually touch.
fn count_fillable(db_path: &PathBuf, mode: Mode, symbol: &str, bars: &[OhlBar]) -> anyhow::Result<usize> {
    let conn = open_db(db_path)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT COUNT(*) FROM prices WHERE symbol = ?1 AND date = ?2 AND {}",
        mode.row_filter()
    ))?;
    let mut count = 0usize;
    for bar in bars.iter().filter(|b| mode == Mode::Fill || b.is_complete()) {
        count += stmt.query_row(params![symbol, bar.date], |row| row.get::<_, i64>(0))? as usize;
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bug this guards against: an ASX bar for Monday opens 10:00 AEDT,
    /// which is 23:00 UTC *Sunday*. Dating it in UTC would attach Monday's
    /// OHLC to Sunday — a date with no stored row, silently filling nothing.
    #[test]
    fn bar_date_uses_exchange_local_day() {
        // 2026-01-04 23:00 UTC with a +11h ASX offset is Monday 5 Jan locally
        let ts = Utc.with_ymd_and_hms(2026, 1, 4, 23, 0, 0).unwrap().timestamp();
        let date = yahoo_local_date(ts, Some(11 * 3600)).unwrap();
        assert_eq!(date, NaiveDate::from_ymd_opt(2026, 1, 5).unwrap());
    }

    #[test]
    fn bar_date_without_offset_is_utc() {
        let ts = Utc.with_ymd_and_hms(2026, 1, 5, 14, 30, 0).unwrap().timestamp();
        let date = yahoo_local_date(ts, None).unwrap();
        assert_eq!(date, NaiveDate::from_ymd_opt(2026, 1, 5).unwrap());
    }

    fn seed(conn: &Connection) {
        conn.execute_batch(
            "CREATE TABLE prices (
                id INTEGER PRIMARY KEY,
                symbol TEXT NOT NULL,
                date TEXT NOT NULL,
                open REAL, high REAL, low REAL, close REAL,
                volume INTEGER,
                fetched_at TEXT NOT NULL,
                UNIQUE(symbol, date)
            );",
        )
        .unwrap();
    }

    #[test]
    fn fills_only_null_ohlc_and_never_touches_close() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.db");
        let conn = open_db(&path).unwrap();
        seed(&conn);
        conn.execute(
            "INSERT INTO prices (symbol, date, close, volume, fetched_at) VALUES ('BHP', '2026-01-05', 45.0, 100, 'x')",
            [],
        )
        .unwrap();
        // An already-complete row must be left exactly as it is
        conn.execute(
            "INSERT INTO prices (symbol, date, open, high, low, close, fetched_at) VALUES ('BHP', '2026-01-06', 1.0, 2.0, 0.5, 46.0, 'x')",
            [],
        )
        .unwrap();
        drop(conn);

        let bars = vec![
            OhlBar { date: "2026-01-05".into(), open: Some(44.0), high: Some(45.5), low: Some(43.8) },
            OhlBar { date: "2026-01-06".into(), open: Some(99.0), high: Some(99.0), low: Some(99.0) },
            // A date with no stored row must not be inserted
            OhlBar { date: "2026-01-07".into(), open: Some(50.0), high: Some(51.0), low: Some(49.0) },
        ];
        let updated = apply_bars(&path, Mode::Fill, "BHP", &bars).unwrap();
        assert_eq!(updated, 1, "only the close-only row should be touched");

        let conn = open_db(&path).unwrap();
        let (open, high, low, close): (f64, f64, f64, f64) = conn
            .query_row(
                "SELECT open, high, low, close FROM prices WHERE symbol='BHP' AND date='2026-01-05'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!((open, high, low), (44.0, 45.5, 43.8));
        assert_eq!(close, 45.0, "close must be preserved");

        let untouched: f64 = conn
            .query_row(
                "SELECT open FROM prices WHERE symbol='BHP' AND date='2026-01-06'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(untouched, 1.0, "complete rows must not be overwritten");

        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM prices", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 2, "backfill must not insert new dates");
    }

    #[test]
    fn rerunning_is_a_noop() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.db");
        let conn = open_db(&path).unwrap();
        seed(&conn);
        conn.execute(
            "INSERT INTO prices (symbol, date, close, fetched_at) VALUES ('CBA', '2026-02-02', 10.0, 'x')",
            [],
        )
        .unwrap();
        drop(conn);

        let bars = vec![OhlBar { date: "2026-02-02".into(), open: Some(9.0), high: Some(11.0), low: Some(8.5) }];
        assert_eq!(apply_bars(&path, Mode::Fill, "CBA", &bars).unwrap(), 1);
        assert_eq!(apply_bars(&path, Mode::Fill, "CBA", &bars).unwrap(), 0, "second run must change nothing");
    }

    /// EXPD 2026-09-17 as the live-price path stored it: the bar's open with the
    /// chart meta's high, which missed the opening print.
    #[test]
    fn repair_rewrites_only_inconsistent_rows_and_keeps_close() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.db");
        let conn = open_db(&path).unwrap();
        seed(&conn);
        conn.execute_batch(
            "INSERT INTO prices (symbol, date, open, high, low, close, fetched_at)
               VALUES ('EXPD', '2026-09-17', 190.9199981689453, 190.255, 187.9, 189.08, 'x');
             INSERT INTO prices (symbol, date, open, high, low, close, fetched_at)
               VALUES ('EXPD', '2026-09-18', 189.19, 191.0, 186.72, 190.94, 'x');
             INSERT INTO prices (symbol, date, close, fetched_at)
               VALUES ('EXPD', '2026-09-19', 191.0, 'x');",
        )
        .unwrap();
        drop(conn);

        let bars = vec![
            OhlBar { date: "2026-09-17".into(), open: Some(190.9199981689453), high: Some(190.9199981689453), low: Some(187.89999389648438) },
            OhlBar { date: "2026-09-18".into(), open: Some(1.0), high: Some(1.0), low: Some(1.0) },
            OhlBar { date: "2026-09-19".into(), open: Some(1.0), high: Some(1.0), low: Some(1.0) },
        ];
        assert_eq!(count_fillable(&path, Mode::Repair, "EXPD", &bars).unwrap(), 1);
        assert_eq!(apply_bars(&path, Mode::Repair, "EXPD", &bars).unwrap(), 1, "only the inconsistent row");

        let conn = open_db(&path).unwrap();
        let row = |date: &str| -> (Option<f64>, Option<f64>, Option<f64>, f64) {
            conn.query_row(
                "SELECT open, high, low, close FROM prices WHERE symbol='EXPD' AND date=?1",
                params![date],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap()
        };
        assert_eq!(row("2026-09-17"), (Some(190.9199981689453), Some(190.9199981689453), Some(187.89999389648438), 189.08));
        assert_eq!(row("2026-09-18"), (Some(189.19), Some(191.0), Some(186.72), 190.94), "consistent rows are left alone");
        assert_eq!(row("2026-09-19"), (None, None, None, 191.0), "repair does not fill gaps");
        drop(conn);
        assert_eq!(apply_bars(&path, Mode::Repair, "EXPD", &bars).unwrap(), 0, "second run must change nothing");
    }

    /// The stored close is the live quote (189.08) while Yahoo's bar carries the
    /// f32 value; a bar low a hair above the close must not leave the row
    /// inconsistent, or every run would rewrite it.
    #[test]
    fn repair_widens_the_range_to_the_stored_close() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.db");
        let conn = open_db(&path).unwrap();
        seed(&conn);
        conn.execute(
            "INSERT INTO prices (symbol, date, open, high, low, close, fetched_at) VALUES ('X', '2026-09-17', 12.0, 11.0, 10.0, 10.0, 'x')",
            [],
        )
        .unwrap();
        drop(conn);
        let bars = vec![OhlBar { date: "2026-09-17".into(), open: Some(12.0), high: Some(12.0), low: Some(10.0000002) }];
        assert_eq!(apply_bars(&path, Mode::Repair, "X", &bars).unwrap(), 1);
        assert_eq!(apply_bars(&path, Mode::Repair, "X", &bars).unwrap(), 0);
        let conn = open_db(&path).unwrap();
        let low: f64 = conn.query_row("SELECT low FROM prices WHERE symbol='X'", [], |r| r.get(0)).unwrap();
        assert_eq!(low, 10.0);
    }
}

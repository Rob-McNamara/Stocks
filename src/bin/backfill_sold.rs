//! Backfill daily price history for stocks that have been sold.
//!
//! Price collection follows what you hold and what you watch, so a position
//! stops being priced the moment it is sold and dropped from the watchlist. The
//! Hindsight screen asks what a stock did *after* the sale — a week later, six
//! weeks, three months, its peak and low since — and none of those windows can
//! be answered from bars that stop on the sale date.
//!
//! This fills them in. For every symbol with a sale it fetches the daily series
//! from shortly before the earliest sale through to today and writes the bars
//! that are missing.
//!
//! Deliberately additive: it only ever inserts dates the table does not already
//! have, and never rewrites an existing bar. A bad run can leave extra rows but
//! cannot corrupt the series the portfolio engine values holdings from.
//!
//! Usage:
//!   cargo run --bin backfill_sold                  # every symbol with a sale
//!   cargo run --bin backfill_sold -- --dry-run     # report, write nothing
//!   cargo run --bin backfill_sold -- --symbol JLG.AX
//!   cargo run --bin backfill_sold -- --limit 20    # one batch; re-run to continue

use chrono::{NaiveDate, Utc};
use reqwest::Client;
use rusqlite::{params, Connection};
use serde::Deserialize;
use std::{env, path::PathBuf, time::Duration};
use tokio::time;

/// WAL and a busy timeout so this can run while the API and daemon are live.
fn open_db<P: AsRef<std::path::Path>>(path: P) -> Result<Connection, rusqlite::Error> {
    let conn = Connection::open(path)?;
    conn.busy_timeout(Duration::from_secs(5))?;
    let _: String = conn.query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))?;
    Ok(conn)
}

fn insert_event_log(db_path: &PathBuf, level: &str, symbol: Option<&str>, details: &str) {
    let Ok(conn) = open_db(db_path) else { return };
    let _ = conn.execute(
        "INSERT INTO event_log (timestamp, level, source, event_type, symbol, details)
         VALUES (?1, ?2, 'backfill_sold', 'price_backfill', ?3, ?4)",
        params![Utc::now().to_rfc3339(), level, symbol, details],
    );
}

/// One symbol needing history, with the date its coverage has to reach back to.
struct Target {
    symbol: String,
    earliest_sale: NaiveDate,
    bars_after_sale: i64,
}

/// Every symbol with at least one sale.
///
/// Not just the fully-exited ones: a partial sale deserves the same "what
/// happened next", and a symbol already covered simply writes no new rows, so
/// including it costs one request and keeps the rule simple.
fn load_targets(db_path: &PathBuf, only: Option<&str>) -> anyhow::Result<Vec<Target>> {
    let conn = open_db(db_path)?;
    let mut stmt = conn.prepare(
        "SELECT symbol, MIN(date) FROM holdings_transactions
          WHERE transaction_type = 'sale'
          GROUP BY symbol ORDER BY symbol",
    )?;
    let rows: Vec<(String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .filter_map(|r| r.ok())
        .collect();

    let mut out = Vec::new();
    for (symbol, first_sale) in rows {
        if let Some(want) = only
            && !symbol.eq_ignore_ascii_case(want)
        {
            continue;
        }
        let Ok(earliest_sale) = NaiveDate::parse_from_str(&first_sale, "%Y-%m-%d") else {
            continue;
        };
        let bars_after_sale: i64 = conn.query_row(
            "SELECT COUNT(*) FROM prices WHERE symbol = ?1 AND date >= ?2",
            params![symbol, first_sale],
            |r| r.get(0),
        )?;
        out.push(Target { symbol, earliest_sale, bars_after_sale });
    }
    Ok(out)
}

#[derive(Deserialize)]
struct ChartResponse {
    chart: Chart,
}
#[derive(Deserialize)]
struct Chart {
    result: Option<Vec<ChartResult>>,
}
#[derive(Deserialize)]
struct ChartResult {
    timestamp: Option<Vec<i64>>,
    indicators: Indicators,
}
#[derive(Deserialize)]
struct Indicators {
    quote: Vec<Quote>,
}
#[derive(Deserialize)]
struct Quote {
    open: Option<Vec<Option<f64>>>,
    high: Option<Vec<Option<f64>>>,
    low: Option<Vec<Option<f64>>>,
    close: Option<Vec<Option<f64>>>,
    volume: Option<Vec<Option<i64>>>,
}

struct Bar {
    date: String,
    open: Option<f64>,
    high: Option<f64>,
    low: Option<f64>,
    close: f64,
    volume: Option<i64>,
}

/// Explicit period bounds rather than `range=max`: Yahoo downsamples an
/// unbounded range to monthly bars, which would silently answer "peak since
/// sold" from twelve points a year.
async fn fetch_daily(
    client: &Client,
    symbol: &str,
    from: NaiveDate,
) -> anyhow::Result<Vec<Bar>> {
    let period1 = from
        .checked_sub_days(chrono::Days::new(4))
        .unwrap_or(from)
        .and_hms_opt(0, 0, 0)
        .map(|dt| dt.and_utc().timestamp())
        .unwrap_or(0);
    let period2 = Utc::now().timestamp();

    let url = format!(
        "https://query1.finance.yahoo.com/v8/finance/chart/{}?interval=1d&period1={}&period2={}",
        symbol, period1, period2
    );
    let body = client.get(&url).send().await?.text().await?;
    let parsed: ChartResponse = serde_json::from_str(&body)?;
    let Some(result) = parsed.chart.result.and_then(|r| r.into_iter().next()) else {
        return Ok(Vec::new());
    };
    let (Some(stamps), Some(quote)) = (result.timestamp, result.indicators.quote.into_iter().next())
    else {
        return Ok(Vec::new());
    };

    let at = |v: &Option<Vec<Option<f64>>>, i: usize| v.as_ref().and_then(|s| s.get(i).copied().flatten());
    let mut bars = Vec::new();
    for (i, ts) in stamps.iter().enumerate() {
        // A bar with no close is a non-trading stub; there is nothing to record.
        let Some(close) = at(&quote.close, i) else { continue };
        let Some(dt) = chrono::DateTime::from_timestamp(*ts, 0) else { continue };
        bars.push(Bar {
            date: dt.format("%Y-%m-%d").to_string(),
            open: at(&quote.open, i),
            high: at(&quote.high, i),
            low: at(&quote.low, i),
            close,
            volume: quote.volume.as_ref().and_then(|s| s.get(i).copied().flatten()),
        });
    }
    Ok(bars)
}

/// Insert only the dates the table does not have. `INSERT OR IGNORE` leans on
/// the `(symbol, date)` uniqueness so an existing bar is never rewritten —
/// today's row in particular is the running close and belongs to the live
/// fetcher, not to this tool.
fn write_missing(conn: &Connection, symbol: &str, bars: &[Bar]) -> anyhow::Result<usize> {
    let now = Utc::now().to_rfc3339();
    let mut written = 0;
    for b in bars {
        let n = conn.execute(
            "INSERT OR IGNORE INTO prices (symbol, date, open, high, low, close, volume, fetched_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![symbol, b.date, b.open, b.high, b.low, b.close, b.volume, now],
        )?;
        written += n;
    }
    Ok(written)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::init();
    let db_path = PathBuf::from(env::var("DATABASE_PATH").unwrap_or_else(|_| "stocks.db".to_string()));

    let args: Vec<String> = env::args().collect();
    let dry_run = args.iter().any(|a| a == "--dry-run");
    let only = args
        .iter()
        .position(|a| a == "--symbol")
        .and_then(|i| args.get(i + 1))
        .map(|s| s.trim().to_uppercase());
    // Yahoo rate-limits long runs, so the work can be done in batches; each run
    // re-reads the current state, so a later run simply writes less.
    let limit = args
        .iter()
        .position(|a| a == "--limit")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse::<usize>().ok());

    let mut targets = load_targets(&db_path, only.as_deref())?;
    if let Some(limit) = limit {
        targets.truncate(limit);
    }
    if targets.is_empty() {
        println!("Nothing to do — no sold symbols matched.");
        return Ok(());
    }

    println!(
        "{} symbol(s) with a sale{}\n",
        targets.len(),
        if dry_run { ", dry run — nothing will be written" } else { "" }
    );

    let client = Client::builder().user_agent("stocks-api/1.0").build()?;
    let (mut total_written, mut failed, mut untouched) = (0usize, 0usize, 0usize);

    for (i, t) in targets.iter().enumerate() {
        if i > 0 {
            time::sleep(Duration::from_millis(600)).await;
        }
        match fetch_daily(&client, &t.symbol, t.earliest_sale).await {
            Ok(bars) if bars.is_empty() => {
                // Delisted, renamed, or simply unknown to Yahoo. Nothing to do
                // here — those need a recorded final price instead.
                failed += 1;
                println!("  {:<10} no bars returned — dead or unknown symbol", t.symbol);
                insert_event_log(&db_path, "warn", Some(&t.symbol), "Backfill returned no bars");
            }
            Ok(bars) => {
                if dry_run {
                    println!(
                        "  {:<10} {:>4} bars available, {} already stored after {}",
                        t.symbol, bars.len(), t.bars_after_sale, t.earliest_sale
                    );
                    continue;
                }
                let conn = open_db(&db_path)?;
                let written = write_missing(&conn, &t.symbol, &bars)?;
                total_written += written;
                if written == 0 {
                    untouched += 1;
                } else {
                    println!("  {:<10} +{} bars", t.symbol, written);
                }
            }
            Err(err) => {
                failed += 1;
                println!("  {:<10} fetch failed: {}", t.symbol, err);
                insert_event_log(&db_path, "error", Some(&t.symbol), &format!("Backfill failed: {}", err));
            }
        }
    }

    println!(
        "\n{} bar(s) written, {} symbol(s) already current, {} failed.",
        total_written, untouched, failed
    );
    if !dry_run {
        insert_event_log(
            &db_path,
            "info",
            None,
            &format!("Backfilled {} bar(s) across {} sold symbol(s), {} failed", total_written, targets.len(), failed),
        );
    }
    Ok(())
}

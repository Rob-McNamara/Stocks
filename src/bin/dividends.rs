use chrono::{NaiveDate, TimeZone, Utc};
use reqwest::Client;
use rusqlite::{params, Connection};
use serde::Deserialize;
use std::{collections::HashMap, env, path::PathBuf, time::Duration};
use stocks::portfolio::{self, ImpliedDividendPayment, PortfolioTx, TxType};
use tokio::time;

/// Open the SQLite database with WAL mode and a busy timeout so the API,
/// price daemon and dividends daemon can write concurrently without
/// intermittent "database is locked" failures.
fn open_db<P: AsRef<std::path::Path>>(path: P) -> Result<Connection, rusqlite::Error> {
    let conn = Connection::open(path)?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    let _: String = conn.query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))?;
    Ok(conn)
}

#[derive(Debug)]
struct DividendEvent {
    #[allow(dead_code)] // set for Debug/log symmetry; storage passes the symbol separately
    symbol: String,
    ex_date: NaiveDate,
    payment_date: Option<NaiveDate>,
    record_date: Option<NaiveDate>,
    amount: f64,
    fetched_at: String,
}

#[derive(Debug, Deserialize)]
struct YahooChartResponse {
    chart: YahooChart,
}

#[derive(Debug, Deserialize)]
struct YahooChart {
    result: Option<Vec<YahooResult>>,
}

#[derive(Debug, Deserialize)]
struct YahooResult {
    meta: Option<YahooResultMeta>,
    events: Option<YahooEvents>,
}

#[derive(Debug, Deserialize)]
struct YahooResultMeta {
    /// Exchange UTC offset in seconds — needed to date events correctly
    gmtoffset: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct YahooEvents {
    dividends: Option<HashMap<String, YahooDividendEntry>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct YahooDividendEntry {
    amount: Option<f64>,
    date: Option<i64>,
    ex_date: Option<i64>,
    payment_date: Option<i64>,
    record_date: Option<i64>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::init();
    let database_path = env::var("DATABASE_PATH").unwrap_or_else(|_| "stocks.db".to_string());
    let schedule = env::var("DIVIDEND_SCHEDULE").unwrap_or_else(|_| "daily".to_string());
    let interval_secs = match schedule.as_str() {
        "weekly" => 604_800,
        "daily" => 86_400,
        _ => env::var("DIVIDEND_CHECK_INTERVAL_SECS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(86_400),
    };
    let run_once = env::var("RUN_ONCE").is_ok() || env::args().any(|arg| arg == "--once");

    let db_path = PathBuf::from(database_path);
    init_db(&db_path)?;

    let client = Client::builder()
        .user_agent("stocks-dividends-daemon/1.0")
        .build()?;

    if run_once {
        run_dividend_check(&client, &db_path).await?;
        return Ok(());
    }

    log::info!("Starting dividend daemon, polling every {} seconds", interval_secs);
    let mut interval = time::interval(Duration::from_secs(interval_secs));
    loop {
        interval.tick().await;
        if let Err(err) = run_dividend_check(&client, &db_path).await {
            log::error!("Dividend check failed: {}", err);
        }
    }
}

async fn run_dividend_check(client: &Client, db_path: &PathBuf) -> anyhow::Result<()> {
    let holdings = load_holdings_transactions(db_path)?;
    if holdings.is_empty() {
        log::info!("No holdings transactions found; skipping dividend update.");
        return Ok(());
    }

    let unique_symbols: Vec<String> = holdings
        .iter()
        .map(|tx| tx.symbol.clone())
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();

    for symbol in unique_symbols {
        log::info!("Checking dividends for {}", symbol);
        let events = match fetch_dividend_events(client, &symbol).await {
            Ok(ev) => ev,
            Err(err) => {
                log::error!("Dividend fetch failed for {}: {}", symbol, err);
                let _ = insert_event_log(db_path, "error", "dividend_fetch", "dividends_daemon", Some(&symbol), &format!("Fetch error: {}", err));
                continue;
            }
        };

        if events.is_empty() {
            log::info!("No dividend events found for {}", symbol);
            let _ = insert_event_log(db_path, "info", "dividend_fetch", "dividends_daemon", Some(&symbol), "No dividend events found");
            continue;
        }

        store_dividend_events(db_path, &symbol, &events)?;

        let symbol_transactions: Vec<PortfolioTx> = holdings
            .iter()
            .filter(|tx| tx.symbol == symbol)
            .cloned()
            .collect();

        let payments = calculate_dividend_payments(&symbol_transactions, &events);
        print_dividend_payments(&symbol, &payments, &events);
    }

    Ok(())
}

/// Shares-held and payment math live in stocks::portfolio (shared with the
/// API); this only adapts the daemon's event type to (ex_date, amount) pairs.
fn calculate_dividend_payments(transactions: &[PortfolioTx], events: &[DividendEvent]) -> Vec<ImpliedDividendPayment> {
    let pairs: Vec<(String, f64)> = events
        .iter()
        .map(|e| (e.ex_date.format("%Y-%m-%d").to_string(), e.amount))
        .collect();
    portfolio::implied_dividend_payments(transactions, &pairs)
}

fn print_dividend_payments(symbol: &str, payments: &[ImpliedDividendPayment], events: &[DividendEvent]) {
    if payments.is_empty() {
        log::info!("No eligible dividend payments found for {}", symbol);
        return;
    }

    println!("\nDividend payment summary for {}:", symbol);
    for payment in payments {
        let payment_date = events
            .iter()
            .find(|e| e.ex_date.format("%Y-%m-%d").to_string() == payment.ex_date)
            .and_then(|e| e.payment_date);
        println!(
            "  Ex-date: {} | Payment: {} | Rate: ${:.4} | Shares: {:.2} | Total: ${:.2}",
            payment.ex_date,
            payment_date.map(|d| d.to_string()).unwrap_or_else(|| "n/a".to_string()),
            payment.amount_per_share,
            payment.shares_held,
            payment.total_payment,
        );
    }
}

fn load_holdings_transactions(db_path: &PathBuf) -> anyhow::Result<Vec<PortfolioTx>> {
    let conn = open_db(db_path)?;
    let mut stmt = conn.prepare(
        "SELECT id, symbol, transaction_type, date, quantity, price, amount, brokerage
         FROM holdings_transactions
         ORDER BY date ASC, id ASC",
    )?;

    let rows = stmt.query_map([], |row| {
        Ok(PortfolioTx {
            id: row.get(0)?,
            symbol: row.get(1)?,
            tx_type: TxType::parse(&row.get::<_, String>(2)?),
            date: row.get(3)?,
            quantity: row.get(4)?,
            price: row.get(5)?,
            native_price: None,
            amount: row.get(6)?,
            brokerage: row.get(7)?,
            dividends_total: 0.0,
        })
    })?;

    rows.collect::<Result<_, rusqlite::Error>>().map_err(|err| err.into())
}

async fn fetch_dividend_events(client: &Client, symbol: &str) -> anyhow::Result<Vec<DividendEvent>> {
    let url = format!(
        "https://query1.finance.yahoo.com/v8/finance/chart/{}?interval=1d&range=5y&events=div",
        symbol
    );

    let response = client.get(&url).send().await?;
    let response = response.error_for_status()?;
    let payload: YahooChartResponse = response.json().await?;

    let result = payload
        .chart
        .result
        .and_then(|items| items.into_iter().next())
        .ok_or_else(|| anyhow::anyhow!("No chart result found for {}", symbol))?;

    let now = Utc::now().to_rfc3339();
    let mut events = Vec::new();

    // Yahoo stamps dividend events at the market open; for exchanges ahead
    // of UTC (e.g. ASX during daylight saving) the UTC date is one day
    // early, so dates are taken in exchange time: UTC + meta.gmtoffset.
    let gmtoffset = result.meta.as_ref().and_then(|m| m.gmtoffset).unwrap_or(0);

    if let Some(events_payload) = result.events
        && let Some(dividends) = events_payload.dividends {
            for entry in dividends.values() {
                let amount = entry.amount.unwrap_or(0.0);
                if amount <= 0.0 {
                    continue;
                }

                let ex_date = entry
                    .ex_date
                    .or(entry.date)
                    .ok_or_else(|| anyhow::anyhow!("Dividend entry missing date for {}", symbol))?;
                let payment_date = entry.payment_date;
                let record_date = entry.record_date;

                let event = DividendEvent {
                    symbol: symbol.to_string(),
                    ex_date: Utc.timestamp_opt(ex_date + gmtoffset, 0)
                        .single()
                        .ok_or_else(|| anyhow::anyhow!("Invalid ex-date timestamp {}", ex_date))?
                        .date_naive(),
                    payment_date: payment_date
                        .and_then(|ts| Utc.timestamp_opt(ts + gmtoffset, 0).single())
                        .map(|dt| dt.date_naive()),
                    record_date: record_date
                        .and_then(|ts| Utc.timestamp_opt(ts + gmtoffset, 0).single())
                        .map(|dt| dt.date_naive()),
                    amount,
                    fetched_at: now.clone(),
                };
                events.push(event);
            }
        }

    events.sort_by_key(|event| event.ex_date);
    Ok(events)
}

fn init_db(path: &PathBuf) -> anyhow::Result<()> {
    let conn = open_db(path)?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS dividend_events (
            id INTEGER PRIMARY KEY,
            symbol TEXT NOT NULL,
            ex_date TEXT NOT NULL,
            payment_date TEXT,
            record_date TEXT,
            amount REAL NOT NULL,
            fetched_at TEXT NOT NULL,
            UNIQUE(symbol, ex_date)
        );
        CREATE INDEX IF NOT EXISTS idx_dividend_events_symbol_date ON dividend_events(symbol, ex_date);
        CREATE TABLE IF NOT EXISTS event_log (
            id INTEGER PRIMARY KEY,
            timestamp TEXT NOT NULL,
            level TEXT NOT NULL,
            source TEXT NOT NULL,
            event_type TEXT NOT NULL,
            symbol TEXT,
            details TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_event_log_timestamp ON event_log(timestamp);
        ",
    )?;
    Ok(())
}

fn store_dividend_events(
    db_path: &PathBuf,
    symbol: &str,
    events: &[DividendEvent],
) -> anyhow::Result<()> {
    let mut conn = open_db(db_path)?;
    let tx = conn.transaction()?;
    {
        let mut insert = tx.prepare(
            "INSERT OR REPLACE INTO dividend_events
             (symbol, ex_date, payment_date, record_date, amount, fetched_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?;

        for event in events {
            insert.execute(params![
                symbol,
                event.ex_date.format("%Y-%m-%d").to_string(),
                event
                    .payment_date
                    .map(|d| d.format("%Y-%m-%d").to_string()),
                event
                    .record_date
                    .map(|d| d.format("%Y-%m-%d").to_string()),
                event.amount,
                event.fetched_at,
            ])?;
        }
    }

    tx.commit()?;
    // Log successful dividend fetch
    let details = format!("Stored {} dividend events for {}", events.len(), symbol);
    let _ = insert_event_log(db_path, "info", "dividend_fetch", "dividends_daemon", Some(symbol), &details);
    Ok(())
}

fn insert_event_log(
    db_path: &PathBuf,
    level: &str,
    event_type: &str,
    source: &str,
    symbol: Option<&str>,
    details: &str,
) -> anyhow::Result<()> {
    let conn = open_db(db_path)?;
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO event_log (timestamp, level, source, event_type, symbol, details) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![now, level, source, event_type, symbol, details],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// gmtoffset must survive deserialisation of the dividends payload —
    /// ASX ex-dividend dates are stamped at the market open (23:00 UTC the
    /// previous day during daylight saving) and shift a day early without it.
    #[test]
    fn dividend_chart_response_parses_gmtoffset() {
        let json = r#"{
            "chart": {
                "result": [{
                    "meta": { "gmtoffset": 39600 },
                    "events": {
                        "dividends": {
                            "1767564000": { "amount": 0.44, "date": 1767564000 }
                        }
                    }
                }]
            }
        }"#;
        let payload: YahooChartResponse = serde_json::from_str(json).unwrap();
        let result = payload.chart.result.unwrap().into_iter().next().unwrap();
        let gmtoffset = result.meta.as_ref().and_then(|m| m.gmtoffset).unwrap_or(0);
        assert_eq!(gmtoffset, 39600);
        let ts = result.events.unwrap().dividends.unwrap().values().next().unwrap().date.unwrap();
        // 1767564000 = 2026-01-04 22:00 UTC = 2026-01-05 09:00 AEDT: the
        // ex-date must land on the Sydney trading day, not the UTC day.
        let ex_date = Utc.timestamp_opt(ts + gmtoffset, 0).single().unwrap().date_naive();
        assert_eq!(ex_date, NaiveDate::from_ymd_opt(2026, 1, 5).unwrap());
    }

    /// End-to-end through the daemon's own load path: seed a temp DB, load
    /// transactions as the daemon does, and assert the computed payments
    /// match the shared engine's hand-checked numbers — the same maths the
    /// API reports, now from one definition.
    #[test]
    fn daemon_payment_totals_match_the_shared_engine() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let db_path = PathBuf::from(file.path());
        init_db(&db_path).unwrap();

        let conn = open_db(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE holdings_transactions (
                id INTEGER PRIMARY KEY,
                symbol TEXT NOT NULL,
                transaction_type TEXT NOT NULL,
                date TEXT NOT NULL,
                quantity REAL,
                price REAL,
                amount REAL,
                brokerage REAL,
                notes TEXT,
                created_at TEXT NOT NULL
            );
            INSERT INTO holdings_transactions (id, symbol, transaction_type, date, quantity, price, created_at)
            VALUES (1, 'TST.AX', 'purchase', '2026-01-05', 100.0, 10.0, '2026-01-05T00:00:00Z'),
                   (2, 'TST.AX', 'sale', '2026-03-02', 40.0, 12.0, '2026-03-02T00:00:00Z');",
        )
        .unwrap();
        drop(conn);

        let holdings = load_holdings_transactions(&db_path).unwrap();
        assert_eq!(holdings.len(), 2);

        let events = vec![
            DividendEvent {
                symbol: "TST.AX".to_string(),
                ex_date: NaiveDate::from_ymd_opt(2026, 2, 1).unwrap(), // 100 shares held
                payment_date: NaiveDate::from_ymd_opt(2026, 2, 15),
                record_date: None,
                amount: 0.50,
                fetched_at: "2026-07-11T00:00:00Z".to_string(),
            },
            DividendEvent {
                symbol: "TST.AX".to_string(),
                ex_date: NaiveDate::from_ymd_opt(2026, 4, 1).unwrap(), // 60 after the sale
                payment_date: None,
                record_date: None,
                amount: 0.50,
                fetched_at: "2026-07-11T00:00:00Z".to_string(),
            },
        ];

        let payments = calculate_dividend_payments(&holdings, &events);
        assert_eq!(payments.len(), 2);
        assert!((payments[0].total_payment - 50.0).abs() < 1e-9, "100 shares × $0.50");
        assert!((payments[1].shares_held - 60.0).abs() < 1e-9);
        assert!((payments[1].total_payment - 30.0).abs() < 1e-9, "60 shares × $0.50");
        let total: f64 = payments.iter().map(|p| p.total_payment).sum();
        assert!((total - 80.0).abs() < 1e-9);
    }
}

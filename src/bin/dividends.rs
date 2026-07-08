use chrono::{NaiveDate, TimeZone, Utc};
use reqwest::Client;
use rusqlite::{params, types::Type, Connection};
use serde::Deserialize;
use std::{collections::HashMap, env, path::PathBuf, time::Duration};
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

#[allow(dead_code)] // several columns are loaded for completeness but not read by this daemon
#[derive(Debug, Clone)]
struct HoldingTransaction {
    id: i64,
    symbol: String,
    transaction_type: String,
    date: NaiveDate,
    quantity: Option<f64>,
    price: Option<f64>,
    amount: Option<f64>,
    brokerage: Option<f64>,
    notes: Option<String>,
    created_at: String,
}

#[derive(Debug)]
struct DividendEvent {
    symbol: String,
    ex_date: NaiveDate,
    payment_date: Option<NaiveDate>,
    record_date: Option<NaiveDate>,
    amount: f64,
    fetched_at: String,
}

#[allow(dead_code)] // symbol kept for Debug output symmetry with the API binary
#[derive(Debug)]
struct DividendPayment {
    symbol: String,
    ex_date: NaiveDate,
    payment_date: Option<NaiveDate>,
    amount_per_share: f64,
    shares_held: f64,
    total_payment: f64,
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
    events: Option<YahooEvents>,
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
                let _ = insert_event_log(&db_path, "error", "dividend_fetch", "dividends_daemon", Some(&symbol), &format!("Fetch error: {}", err));
                continue;
            }
        };

        if events.is_empty() {
            log::info!("No dividend events found for {}", symbol);
            let _ = insert_event_log(&db_path, "info", "dividend_fetch", "dividends_daemon", Some(&symbol), "No dividend events found");
            continue;
        }

        store_dividend_events(db_path, &symbol, &events)?;

        let symbol_transactions: Vec<HoldingTransaction> = holdings
            .iter()
            .filter(|tx| tx.symbol == symbol)
            .cloned()
            .collect();

        let payments = calculate_dividend_payments(&symbol_transactions, &events);
        print_dividend_payments(&symbol, &payments);
    }

    Ok(())
}

fn print_dividend_payments(symbol: &str, payments: &[DividendPayment]) {
    if payments.is_empty() {
        log::info!("No eligible dividend payments found for {}", symbol);
        return;
    }

    println!("\nDividend payment summary for {}:", symbol);
    for payment in payments {
        println!(
            "  Ex-date: {} | Payment: {} | Rate: ${:.4} | Shares: {:.2} | Total: ${:.2}",
            payment.ex_date,
            payment
                .payment_date
                .map(|d| d.to_string())
                .unwrap_or_else(|| "n/a".to_string()),
            payment.amount_per_share,
            payment.shares_held,
            payment.total_payment,
        );
    }
}

fn load_holdings_transactions(db_path: &PathBuf) -> anyhow::Result<Vec<HoldingTransaction>> {
    let conn = open_db(db_path)?;
    let mut stmt = conn.prepare(
        "SELECT id, symbol, transaction_type, date, quantity, price, amount, brokerage, notes, created_at
         FROM holdings_transactions
         ORDER BY date ASC, id ASC",
    )?;

    let rows = stmt.query_map([], |row| {
        let date_str: String = row.get(3)?;
        let date = NaiveDate::parse_from_str(&date_str, "%Y-%m-%d").map_err(|err| {
            rusqlite::Error::FromSqlConversionFailure(
                3,
                Type::Text,
                Box::new(err),
            )
        })?;
        Ok(HoldingTransaction {
            id: row.get(0)?,
            symbol: row.get(1)?,
            transaction_type: row.get(2)?,
            date,
            quantity: row.get(4)?,
            price: row.get(5)?,
            amount: row.get(6)?,
            brokerage: row.get(7)?,
            notes: row.get(8)?,
            created_at: row.get(9)?,
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

    if let Some(events_payload) = result.events {
        if let Some(dividends) = events_payload.dividends {
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
                    ex_date: Utc.timestamp_opt(ex_date, 0)
                        .single()
                        .ok_or_else(|| anyhow::anyhow!("Invalid ex-date timestamp {}", ex_date))?
                        .date_naive(),
                    payment_date: payment_date
                        .and_then(|ts| Utc.timestamp_opt(ts, 0).single())
                        .map(|dt| dt.date_naive()),
                    record_date: record_date
                        .and_then(|ts| Utc.timestamp_opt(ts, 0).single())
                        .map(|dt| dt.date_naive()),
                    amount,
                    fetched_at: now.clone(),
                };
                events.push(event);
            }
        }
    }

    events.sort_by_key(|event| event.ex_date);
    Ok(events)
}

fn calculate_dividend_payments(
    transactions: &[HoldingTransaction],
    events: &[DividendEvent],
) -> Vec<DividendPayment> {
    let mut payments = Vec::new();

    for event in events {
        let shares_held = calculate_shares_on_date(transactions, event.ex_date);
        let total_payment = shares_held * event.amount;

        if shares_held > 0.0 {
            payments.push(DividendPayment {
                symbol: event.symbol.clone(),
                ex_date: event.ex_date,
                payment_date: event.payment_date,
                amount_per_share: event.amount,
                shares_held,
                total_payment,
            });
        }
    }

    payments
}

fn calculate_shares_on_date(transactions: &[HoldingTransaction], date: NaiveDate) -> f64 {
    let mut shares = 0.0;
    for tx in transactions {
        if tx.date > date {
            break;
        }

        match tx.transaction_type.as_str() {
            "purchase" => shares += tx.quantity.unwrap_or(0.0),
            "sale" => shares -= tx.quantity.unwrap_or(0.0),
            _ => {}
        }
    }
    shares.max(0.0)
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

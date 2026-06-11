use chrono::{Datelike, Duration as ChronoDuration, Local, NaiveDate, TimeZone, Utc};
use reqwest::Client;
use rusqlite::{params, Connection};
use serde::Deserialize;
use std::{env, path::PathBuf, time::Duration};
use tokio::time;

#[derive(Debug)]
struct PriceRecord {
    date: String,
    open: Option<f64>,
    high: Option<f64>,
    low: Option<f64>,
    close: Option<f64>,
    volume: Option<i64>,
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
    timestamp: Option<Vec<i64>>,
    indicators: YahooIndicators,
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
    close: Option<Vec<Option<f64>>>,
    volume: Option<Vec<Option<i64>>>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::init();
    let symbols = env::var("STOCK_SYMBOLS").unwrap_or_else(|_| "BHP".to_string());
    let database_path = env::var("DATABASE_PATH").unwrap_or_else(|_| "stocks.db".to_string());
    let fetch_schedule_hour = env::var("FETCH_SCHEDULE_HOUR")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|&h| h < 24)
        .unwrap_or(16);
    let fetch_schedule_minute = env::var("FETCH_SCHEDULE_MINUTE")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|&m| m < 60)
        .unwrap_or(15);

    let symbols: Vec<String> = symbols
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(normalize_symbol)
        .collect();

    if symbols.is_empty() {
        anyhow::bail!("No STOCK_SYMBOLS configured. Use STOCK_SYMBOLS=TLS,ANZ or similar.");
    }

    let run_once = env::var("RUN_ONCE").is_ok() || env::args().any(|arg| arg == "--once");
    let watchlist_only = env::var("WATCHLIST_ONLY").is_ok() || env::args().any(|arg| arg == "--watchlist-only");
    let historical_range = parse_historical_range()?;
    let watchlist_symbols = env::var("WATCHLIST_SYMBOLS").unwrap_or_default();
    let watchlist_interval_secs = env::var("WATCHLIST_INTERVAL_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(900);
    let watchlist_symbols: Vec<String> = watchlist_symbols
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(normalize_symbol)
        .collect();
    let watchlist_enabled = !watchlist_symbols.is_empty();

    if watchlist_only && !watchlist_enabled {
        anyhow::bail!("WATCHLIST_ONLY requires WATCHLIST_SYMBOLS to be set.");
    }

    let db_path = PathBuf::from(database_path);
    init_db(&db_path)?;
    let manual_holidays = parse_asx_manual_holidays();

    let client = Client::builder()
        .user_agent("stocks-daemon/1.0")
        .build()?;

    let today = Utc::now().date_naive();
    if !watchlist_only && historical_range.is_none() && is_asx_market_closed(today, &manual_holidays) {
        log::info!("ASX is closed today ({}) - skipping update.", today);
        return Ok(());
    }

    if run_once {
        log::info!("Running one-shot ASX update for symbols: {:?}", symbols);
        run_one_shot(&client, &db_path, &symbols, historical_range.as_ref()).await?;
        if watchlist_enabled {
            log::info!("Running one-shot watchlist update for symbols: {:?}", watchlist_symbols);
            run_watchlist_once(&client, &db_path, &watchlist_symbols, historical_range.as_ref()).await?;
        }
        return Ok(());
    }

    if watchlist_only {
        log::info!("Starting watchlist-only mode for symbols: {:?}", watchlist_symbols);
        watchlist_loop(
            &client,
            &db_path,
            watchlist_interval_secs,
            historical_range.clone(),
        )
        .await;
        return Ok(());
    }

    if watchlist_enabled {
        let client_clone = client.clone();
        let db_path_clone = db_path.clone();
        let historical_range_clone = historical_range.clone();
        tokio::spawn(async move {
            watchlist_loop(
                &client_clone,
                &db_path_clone,
                watchlist_interval_secs,
                historical_range_clone,
            )
            .await;
        });
    }

    log::info!(
        "Starting ASX price daemon for symbols: {:?} daily at {:02}:{:02}",
        symbols,
        fetch_schedule_hour,
        fetch_schedule_minute
    );

    let mut next_run = next_daily_run(fetch_schedule_hour, fetch_schedule_minute);
    loop {
        let now = Local::now();
        if next_run > now {
            let duration = next_run
                .signed_duration_since(now)
                .to_std()
                .unwrap_or_else(|_| Duration::from_secs(0));
            time::sleep(duration).await;
        }

        let today = Local::now().date_naive();
        if historical_range.is_none() && is_asx_market_closed(today, &manual_holidays) {
            log::info!("ASX is closed today ({}) - skipping scheduled update.", today);
        } else {
            for symbol in symbols.iter() {
                match fetch_and_store(&client, &db_path, symbol, historical_range.as_ref()).await {
                    Ok(count) => log::info!("Stored {} rows for {}", count, symbol),
                    Err(err) => log::error!("Failed to update {}: {}", symbol, err),
                }
            }
        }

        next_run = next_run + ChronoDuration::days(1);
    }
}

async fn run_one_shot(
    client: &Client,
    db_path: &PathBuf,
    symbols: &[String],
    historical_range: Option<&DateRange>,
) -> anyhow::Result<()> {
    for symbol in symbols.iter() {
        match fetch_and_store(client, db_path, symbol, historical_range).await {
            Ok(count) => log::info!("Stored {} rows for {}", count, symbol),
            Err(err) => log::error!("Failed to update {}: {}", symbol, err),
        }
    }
    Ok(())
}

async fn run_watchlist_once(
    client: &Client,
    db_path: &PathBuf,
    symbols: &[String],
    historical_range: Option<&DateRange>,
) -> anyhow::Result<()> {
    for symbol in symbols.iter() {
        match fetch_and_store_watchlist(client, db_path, symbol, historical_range).await {
            Ok(count) => log::info!("Watchlist stored {} rows for {}", count, symbol),
            Err(err) => log::error!("Watchlist failed for {}: {}", symbol, err),
        }
    }
    Ok(())
}

fn next_daily_run(hour: u32, minute: u32) -> chrono::DateTime<Local> {
    let now = Local::now();
    let today_target = now
        .date_naive()
        .and_hms_opt(hour, minute, 0)
        .unwrap_or_else(|| now.date_naive().and_hms(16, 15, 0));

    let next = if now.time() < today_target.time() {
        Local.from_local_datetime(&today_target).unwrap()
    } else {
        Local.from_local_datetime(&(today_target + ChronoDuration::days(1))).unwrap()
    };

    next
}

async fn watchlist_loop(
    client: &Client,
    db_path: &PathBuf,
    interval_secs: u64,
    historical_range: Option<DateRange>,
) {
    let mut interval = time::interval(Duration::from_secs(interval_secs));
    loop {
        interval.tick().await;
        let today = Utc::now().date_naive();
        if let Err(err) = purge_old_watchlist_entries(db_path, today) {
            log::error!("Watchlist cleanup failed: {}", err);
        }

        match load_watchlist_symbols(db_path) {
            Ok(symbols) if !symbols.is_empty() => {
                for symbol in symbols.iter() {
                    match fetch_and_store_watchlist(client, db_path, symbol, historical_range.as_ref()).await {
                        Ok(count) => log::info!("Watchlist stored {} rows for {}", count, symbol),
                        Err(err) => log::error!("Watchlist failed for {}: {}", symbol, err),
                    }
                }
            }
            Ok(_) => log::info!("No watchlist symbols configured; skipping watchlist tick."),
            Err(err) => log::error!("Unable to load watchlist symbols: {}", err),
        }
    }
}

async fn fetch_and_store_watchlist(
    client: &Client,
    db_path: &PathBuf,
    symbol: &str,
    historical_range: Option<&DateRange>,
) -> anyhow::Result<usize> {
    let prices = fetch_closing_prices(client, symbol, historical_range).await?;
    store_watchlist_prices(db_path, symbol, prices)
}

fn purge_old_watchlist_entries(db_path: &PathBuf, today: NaiveDate) -> anyhow::Result<()> {
    let conn = Connection::open(db_path)?;
    conn.execute(
        "DELETE FROM watchlist_prices WHERE date <> ?1",
        params![today.format("%Y-%m-%d").to_string()],
    )?;
    Ok(())
}

fn normalize_symbol(symbol: &str) -> String {
    let normalized = symbol.to_uppercase();
    if normalized.ends_with(".AX") {
        normalized
    } else {
        format!("{}.AX", normalized)
    }
}

#[derive(Debug, Clone)]
struct DateRange {
    start: NaiveDate,
    end: NaiveDate,
}

fn parse_historical_range() -> anyhow::Result<Option<DateRange>> {
    let days_back = env::var("HISTORICAL_DAYS_BACK").ok();
    let business_days_back = env::var("HISTORICAL_BUSINESS_DAYS_BACK").ok();
    let start_date = env::var("HISTORICAL_START_DATE").ok();
    let end_date = env::var("HISTORICAL_END_DATE").ok();
    let manual_holidays = parse_asx_manual_holidays();

    if days_back.is_none() && business_days_back.is_none() && start_date.is_none() && end_date.is_none() {
        return Ok(None);
    }

    let today = Utc::now().date_naive();
    let start = if let Some(date) = start_date {
        NaiveDate::parse_from_str(&date, "%Y-%m-%d")?
    } else if let Some(days) = business_days_back {
        let days: i64 = days.parse()?;
        calculate_business_back_date(today, days, &manual_holidays)
    } else if let Some(days) = days_back {
        let days: i64 = days.parse()?;
        today - chrono::Duration::days(days)
    } else {
        today
    };

    let end = if let Some(date) = end_date {
        NaiveDate::parse_from_str(&date, "%Y-%m-%d")?
    } else {
        today
    };

    if end < start {
        anyhow::bail!("HISTORICAL_END_DATE must be the same as or after HISTORICAL_START_DATE");
    }

    Ok(Some(DateRange { start, end }))
}

fn calculate_business_back_date(
    mut date: NaiveDate,
    business_days: i64,
    manual_holidays: &[NaiveDate],
) -> NaiveDate {
    let mut count = 0;
    while count < business_days {
        date = date.pred_opt().unwrap();
        if !is_asx_market_closed(date, manual_holidays) {
            count += 1;
        }
    }
    date
}

fn is_asx_market_closed(date: NaiveDate, manual_holidays: &[NaiveDate]) -> bool {
    let weekday = date.weekday();
    if weekday == chrono::Weekday::Sat || weekday == chrono::Weekday::Sun {
        return true;
    }
    manual_holidays.contains(&date) || asx_holidays(date.year()).contains(&date)
}

fn parse_asx_manual_holidays() -> Vec<NaiveDate> {
    let env_value = env::var("ASX_HOLIDAY_OVERRIDES").unwrap_or_default();
    if env_value.trim().is_empty() {
        return Vec::new();
    }

    env_value
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter_map(|date| match NaiveDate::parse_from_str(date, "%Y-%m-%d") {
            Ok(d) => Some(d),
            Err(err) => {
                log::warn!("Invalid ASX_HOLIDAY_OVERRIDES date '{}': {}", date, err);
                None
            }
        })
        .collect()
}

fn asx_holidays(year: i32) -> Vec<NaiveDate> {
    let mut list = Vec::new();
    list.push(observed_new_years_day(year));
    list.push(observed_australia_day(year));
    list.push(good_friday(year));
    list.push(easter_monday(year));
    list.push(observed_anzac_day(year));
    list.push(queens_birthday(year));
    list.push(observed_christmas_day(year));
    list.push(observed_boxing_day(year));
    list
}

fn observed_new_years_day(year: i32) -> NaiveDate {
    let holiday = NaiveDate::from_ymd_opt(year, 1, 1).unwrap();
    match holiday.weekday() {
        chrono::Weekday::Sat => NaiveDate::from_ymd_opt(year, 1, 3).unwrap(),
        chrono::Weekday::Sun => NaiveDate::from_ymd_opt(year, 1, 2).unwrap(),
        _ => holiday,
    }
}

fn observed_australia_day(year: i32) -> NaiveDate {
    let holiday = NaiveDate::from_ymd_opt(year, 1, 26).unwrap();
    match holiday.weekday() {
        chrono::Weekday::Sat => NaiveDate::from_ymd_opt(year, 1, 28).unwrap(),
        chrono::Weekday::Sun => NaiveDate::from_ymd_opt(year, 1, 27).unwrap(),
        _ => holiday,
    }
}

fn good_friday(year: i32) -> NaiveDate {
    easter_sunday(year) - chrono::Duration::days(2)
}

fn easter_monday(year: i32) -> NaiveDate {
    easter_sunday(year) + chrono::Duration::days(1)
}

fn observed_anzac_day(year: i32) -> NaiveDate {
    let holiday = NaiveDate::from_ymd_opt(year, 4, 25).unwrap();
    match holiday.weekday() {
        chrono::Weekday::Sat => NaiveDate::from_ymd_opt(year, 4, 27).unwrap(),
        chrono::Weekday::Sun => NaiveDate::from_ymd_opt(year, 4, 26).unwrap(),
        _ => holiday,
    }
}

fn queens_birthday(year: i32) -> NaiveDate {
    let mut date = NaiveDate::from_ymd_opt(year, 6, 1).unwrap();
    while date.weekday() != chrono::Weekday::Mon {
        date = date.succ_opt().unwrap();
    }
    date + chrono::Duration::days(7)
}

fn observed_christmas_day(year: i32) -> NaiveDate {
    let holiday = NaiveDate::from_ymd_opt(year, 12, 25).unwrap();
    match holiday.weekday() {
        chrono::Weekday::Sat => NaiveDate::from_ymd_opt(year, 12, 27).unwrap(),
        chrono::Weekday::Sun => NaiveDate::from_ymd_opt(year, 12, 27).unwrap(),
        _ => holiday,
    }
}

fn observed_boxing_day(year: i32) -> NaiveDate {
    let holiday = NaiveDate::from_ymd_opt(year, 12, 26).unwrap();
    match holiday.weekday() {
        chrono::Weekday::Sat => NaiveDate::from_ymd_opt(year, 12, 28).unwrap(),
        chrono::Weekday::Sun => NaiveDate::from_ymd_opt(year, 12, 28).unwrap(),
        chrono::Weekday::Mon => NaiveDate::from_ymd_opt(year, 12, 26).unwrap(),
        _ => holiday,
    }
}

fn easter_sunday(year: i32) -> NaiveDate {
    let a = year % 19;
    let b = year / 100;
    let c = year % 100;
    let d = b / 4;
    let e = b % 4;
    let f = (b + 8) / 25;
    let g = (b - f + 1) / 3;
    let h = (19 * a + b - d - g + 15) % 30;
    let i = c / 4;
    let k = c % 4;
    let l = (32 + 2 * e + 2 * i - h - k) % 7;
    let m = (a + 11 * h + 22 * l) / 451;
    let month = (h + l - 7 * m + 114) / 31;
    let day = ((h + l - 7 * m + 114) % 31) + 1;
    NaiveDate::from_ymd_opt(year, month as u32, day as u32).unwrap()
}

fn init_db(path: &PathBuf) -> anyhow::Result<()> {
    let conn = Connection::open(path)?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS prices (
            id INTEGER PRIMARY KEY,
            symbol TEXT NOT NULL,
            date TEXT NOT NULL,
            open REAL,
            high REAL,
            low REAL,
            close REAL,
            volume INTEGER,
            fetched_at TEXT NOT NULL,
            UNIQUE(symbol, date)
        );
        CREATE INDEX IF NOT EXISTS idx_prices_symbol_date ON prices(symbol, date);
        CREATE TABLE IF NOT EXISTS watchlist_prices (
            id INTEGER PRIMARY KEY,
            symbol TEXT NOT NULL,
            date TEXT NOT NULL,
            fetched_at TEXT NOT NULL,
            open REAL,
            high REAL,
            low REAL,
            close REAL,
            volume INTEGER,
            UNIQUE(symbol, date)
        );
        CREATE INDEX IF NOT EXISTS idx_watchlist_prices_symbol_date ON watchlist_prices(symbol, date);
        CREATE TABLE IF NOT EXISTS watchlist_symbols (
            symbol TEXT PRIMARY KEY,
            updated_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS app_config (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );
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

async fn fetch_and_store(
    client: &Client,
    db_path: &PathBuf,
    symbol: &str,
    historical_range: Option<&DateRange>,
) -> anyhow::Result<usize> {
    let prices = fetch_closing_prices(client, symbol, historical_range).await?;
    store_prices(db_path, symbol, prices)
}

async fn fetch_closing_prices(
    client: &Client,
    symbol: &str,
    historical_range: Option<&DateRange>,
) -> anyhow::Result<Vec<PriceRecord>> {
    let url = if let Some(range) = historical_range {
        let period1 = Utc
            .from_utc_datetime(&range.start.and_hms_opt(0, 0, 0).unwrap())
            .timestamp();
        let period2 = Utc
            .from_utc_datetime(&range.end.and_hms_opt(23, 59, 59).unwrap())
            .timestamp();
        format!(
            "https://query1.finance.yahoo.com/v8/finance/chart/{}?interval=1d&period1={}&period2={}",
            symbol, period1, period2
        )
    } else {
        format!(
            "https://query1.finance.yahoo.com/v8/finance/chart/{}?interval=1d&range=1mo",
            symbol
        )
    };

    let response = client.get(&url).send().await?.error_for_status()?;
    let payload: YahooChartResponse = response.json().await?;

    let result = payload
        .chart
        .result
        .as_ref()
        .and_then(|items| items.first())
        .ok_or_else(|| {
            if let Some(error) = payload.chart.error {
                anyhow::anyhow!("No chart result found for {}: {}", symbol, error)
            } else {
                anyhow::anyhow!("No chart result found for {}", symbol)
            }
        })?;

    let timestamp = result.timestamp.as_ref().ok_or_else(|| {
        anyhow::anyhow!("No timestamp array in Yahoo response for {}", symbol)
    })?;

    let quote = result
        .indicators
        .quote
        .first()
        .ok_or_else(|| anyhow::anyhow!("No quote data in Yahoo response for {}", symbol))?;

    let mut records = Vec::with_capacity(timestamp.len());
    for (index, ts) in timestamp.iter().enumerate() {
        let date = Utc
            .timestamp_opt(*ts, 0)
            .single()
            .unwrap_or_else(|| Utc.timestamp_opt(0, 0).single().unwrap())
            .format("%Y-%m-%d")
            .to_string();

        let record = PriceRecord {
            date,
            open: quote.open.as_ref().and_then(|v| v.get(index).cloned().flatten()),
            high: quote.high.as_ref().and_then(|v| v.get(index).cloned().flatten()),
            low: quote.low.as_ref().and_then(|v| v.get(index).cloned().flatten()),
            close: quote.close.as_ref().and_then(|v| v.get(index).cloned().flatten()),
            volume: quote.volume.as_ref().and_then(|v| v.get(index).cloned().flatten()),
        };
        records.push(record);
    }

    Ok(records)
}

fn store_prices(db_path: &PathBuf, symbol: &str, records: Vec<PriceRecord>) -> anyhow::Result<usize> {
    let mut conn = Connection::open(db_path)?;
    let tx = conn.transaction()?;
    let mut insert = tx.prepare(
        "INSERT OR REPLACE INTO prices (symbol, date, open, high, low, close, volume, fetched_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
    )?;

    let fetched_at = Utc::now().to_rfc3339();
    let mut count = 0;

    for record in records.into_iter().filter(|r| r.close.is_some()) {
        insert.execute(params![
            symbol,
            record.date,
            record.open,
            record.high,
            record.low,
            record.close,
            record.volume,
            fetched_at,
        ])?;
        count += 1;
    }

    drop(insert);
    tx.commit()?;
    // Log event
    let details = format!("Inserted {} price records", count);
    let _ = insert_event_log(db_path, "info", "price_update", "daemon", Some(symbol), &details);
    Ok(count)
}

fn load_watchlist_symbols(db_path: &PathBuf) -> anyhow::Result<Vec<String>> {
    let conn = Connection::open(db_path)?;
    let mut stmt = conn.prepare("SELECT symbol FROM watchlist_symbols ORDER BY symbol")?;
    let rows = stmt
        .query_map([], |row| row.get::<usize, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn store_watchlist_prices(db_path: &PathBuf, symbol: &str, records: Vec<PriceRecord>) -> anyhow::Result<usize> {
    let mut conn = Connection::open(db_path)?;
    let tx = conn.transaction()?;
    let mut insert = tx.prepare(
        "INSERT OR REPLACE INTO watchlist_prices (symbol, date, fetched_at, open, high, low, close, volume)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
    )?;

    let fetched_at = Utc::now().to_rfc3339();
    let mut count = 0;

    for record in records.into_iter().filter(|r| r.close.is_some()) {
        insert.execute(params![
            symbol,
            record.date,
            fetched_at,
            record.open,
            record.high,
            record.low,
            record.close,
            record.volume,
        ])?;
        count += 1;
    }

    drop(insert);
    tx.commit()?;
    let details = format!("Inserted {} watchlist price records", count);
    let _ = insert_event_log(db_path, "info", "price_update", "watchlist", Some(symbol), &details);
    Ok(count)
}

fn insert_event_log(
    db_path: &PathBuf,
    level: &str,
    event_type: &str,
    source: &str,
    symbol: Option<&str>,
    details: &str,
) -> anyhow::Result<()> {
    let conn = Connection::open(db_path)?;
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO event_log (timestamp, level, source, event_type, symbol, details) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![now, level, source, event_type, symbol, details],
    )?;
    Ok(())
}

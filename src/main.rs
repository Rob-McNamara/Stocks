use chrono::{Datelike, Duration as ChronoDuration, Local, NaiveDate, TimeZone, Utc};
use reqwest::Client;
use rusqlite::{params, Connection};
use serde::Deserialize;
use std::{env, path::PathBuf, time::Duration};
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
struct YahooMeta {
    #[serde(rename = "instrumentType")]
    instrument_type: Option<String>,
    #[serde(rename = "longName")]
    long_name: Option<String>,
    currency: Option<String>,
    /// Exchange UTC offset in seconds — needed to date bars correctly
    gmtoffset: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct YahooResult {
    meta: YahooMeta,
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
    let historical_range = parse_historical_range()?;

    // The intraday watchlist pipeline was removed: watchlist prices are served
    // from the API's price cache (populated by /api/v1/refresh and on-demand
    // fetches). The daemon's job is daily closes into the `prices` table.
    if env::var("WATCHLIST_SYMBOLS").is_ok()
        || env::var("WATCHLIST_ONLY").is_ok()
        || env::var("WATCHLIST_INTERVAL_SECS").is_ok()
        || env::args().any(|arg| arg == "--watchlist-only")
    {
        log::warn!(
            "WATCHLIST_SYMBOLS/WATCHLIST_ONLY/WATCHLIST_INTERVAL_SECS are no longer used; \
             intraday watchlist prices come from the API's price cache (POST /api/v1/refresh)."
        );
    }

    let db_path = PathBuf::from(database_path);
    init_db(&db_path)?;
    let manual_holidays = parse_asx_manual_holidays();

    let client = Client::builder()
        .user_agent("stocks-daemon/1.0")
        .build()?;

    let today = Utc::now().date_naive();
    if historical_range.is_none() && is_asx_market_closed(today, &manual_holidays) {
        log::info!("ASX is closed today ({}) - skipping update.", today);
        return Ok(());
    }

    if run_once {
        log::info!("Running one-shot ASX update for symbols: {:?}", symbols);
        run_one_shot(&client, &db_path, &symbols, historical_range.as_ref()).await?;
        return Ok(());
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

        next_run += ChronoDuration::days(1);
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


fn next_daily_run(hour: u32, minute: u32) -> chrono::DateTime<Local> {
    next_daily_run_from(Local::now(), hour, minute)
}

/// Next occurrence of hh:mm strictly after `now` — today if the time is
/// still ahead, otherwise tomorrow. An invalid hour/minute falls back to
/// the default 16:15 schedule.
fn next_daily_run_from(now: chrono::DateTime<Local>, hour: u32, minute: u32) -> chrono::DateTime<Local> {
    let today_target = now
        .date_naive()
        .and_hms_opt(hour, minute, 0)
        .unwrap_or_else(|| now.date_naive().and_hms_opt(16, 15, 0).unwrap());

    if now.time() < today_target.time() {
        Local.from_local_datetime(&today_target).unwrap()
    } else {
        Local.from_local_datetime(&(today_target + ChronoDuration::days(1))).unwrap()
    }
}

fn normalize_symbol(symbol: &str) -> String {
    symbol.trim().to_uppercase()
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
    parse_holiday_list(&env::var("ASX_HOLIDAY_OVERRIDES").unwrap_or_default())
}

/// Parse a comma-separated list of YYYY-MM-DD dates; invalid entries are
/// logged and skipped rather than failing the whole list.
fn parse_holiday_list(value: &str) -> Vec<NaiveDate> {
    if value.trim().is_empty() {
        return Vec::new();
    }

    value
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
    vec![
        observed_new_years_day(year),
        observed_australia_day(year),
        good_friday(year),
        easter_monday(year),
        observed_anzac_day(year),
        queens_birthday(year),
        observed_christmas_day(year),
        observed_boxing_day(year),
    ]
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
    let conn = open_db(path)?;
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
        CREATE TABLE IF NOT EXISTS app_config (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS symbol_info (
            symbol TEXT PRIMARY KEY,
            instrument_type TEXT,
            long_name TEXT,
            currency TEXT,
            updated_at TEXT NOT NULL
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
    let (prices, instrument_type, long_name, currency) = fetch_closing_prices(client, symbol, historical_range).await?;
    store_symbol_info(db_path, symbol, instrument_type.as_deref(), long_name.as_deref(), currency.as_deref())?;
    store_prices(db_path, symbol, prices)
}

/// Date a Yahoo daily bar by its exchange-local trading day. Yahoo stamps
/// bars at the market open; for exchanges ahead of UTC (ASX opens 10:00
/// Sydney = 23:00 UTC the *previous* day during daylight saving) the UTC
/// date is one day early, so the date is taken in exchange time:
/// UTC + meta.gmtoffset.
fn bar_date(ts: i64, gmtoffset: Option<i64>) -> String {
    Utc.timestamp_opt(ts + gmtoffset.unwrap_or(0), 0)
        .single()
        .unwrap_or_else(|| Utc.timestamp_opt(0, 0).single().unwrap())
        .format("%Y-%m-%d")
        .to_string()
}

async fn fetch_closing_prices(
    client: &Client,
    symbol: &str,
    historical_range: Option<&DateRange>,
) -> anyhow::Result<(Vec<PriceRecord>, Option<String>, Option<String>, Option<String>)> {
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
            if let Some(error) = &payload.chart.error {
                anyhow::anyhow!("No chart result found for {}: {}", symbol, error)
            } else {
                anyhow::anyhow!("No chart result found for {}", symbol)
            }
        })?;

    let instrument_type = result.meta.instrument_type.clone();
    let long_name = result.meta.long_name.clone();
    let currency = result.meta.currency.clone();

    let timestamp = result.timestamp.as_ref().ok_or_else(|| {
        anyhow::anyhow!("No timestamp array in Yahoo response for {}", symbol)
    })?;

    let quote = result
        .indicators
        .quote
        .first()
        .ok_or_else(|| anyhow::anyhow!("No quote data in Yahoo response for {}", symbol))?;

    let gmtoffset = result.meta.gmtoffset;
    let mut records = Vec::with_capacity(timestamp.len());
    for (index, ts) in timestamp.iter().enumerate() {
        let date = bar_date(*ts, gmtoffset);

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

    Ok((records, instrument_type, long_name, currency))
}

fn store_symbol_info(
    db_path: &PathBuf,
    symbol: &str,
    instrument_type: Option<&str>,
    long_name: Option<&str>,
    currency: Option<&str>,
) -> anyhow::Result<()> {
    let conn = open_db(db_path)?;
    conn.execute(
        "INSERT INTO symbol_info (symbol, instrument_type, long_name, currency, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(symbol) DO UPDATE SET
             instrument_type = COALESCE(?2, instrument_type),
             long_name = COALESCE(?3, long_name),
             currency = COALESCE(?4, currency),
             updated_at = ?5",
        params![symbol, instrument_type, long_name, currency, Utc::now().to_rfc3339()],
    )?;
    Ok(())
}

fn store_prices(db_path: &PathBuf, symbol: &str, records: Vec<PriceRecord>) -> anyhow::Result<usize> {
    let mut conn = open_db(db_path)?;
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
    // Stamp the refresh so /api/v1/sync-state can tell polling clients that
    // daily closes changed (the prices table itself has no audit trigger).
    conn.execute(
        "INSERT INTO app_config (key, value) VALUES ('daily_prices_updated_at', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![Utc::now().to_rfc3339()],
    )?;
    // Log event
    let details = format!("Inserted {} price records", count);
    let _ = insert_event_log(db_path, "info", "price_update", "daemon", Some(symbol), &details);
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

    const AEDT: i64 = 11 * 3600; // Sydney daylight saving, Oct–Apr
    const AEST: i64 = 10 * 3600; // Sydney standard time
    const EST: i64 = -5 * 3600; // New York standard time

    fn utc_ts(y: i32, m: u32, d: u32, h: u32, min: u32) -> i64 {
        Utc.with_ymd_and_hms(y, m, d, h, min, 0).unwrap().timestamp()
    }

    /// The bug this guards against: an ASX bar for Monday opens 10:00 AEDT,
    /// which is 23:00 UTC *Sunday*. Dating it in UTC filed Monday's close
    /// under Sunday for every daylight-saving trading day.
    #[test]
    fn bar_date_aedt_open_dates_as_local_trading_day() {
        let ts = utc_ts(2026, 1, 4, 23, 0); // Mon 2026-01-05 10:00 AEDT
        assert_eq!(bar_date(ts, Some(AEDT)), "2026-01-05");
    }

    #[test]
    fn bar_date_aest_open_is_unchanged() {
        let ts = utc_ts(2026, 6, 1, 0, 0); // Mon 2026-06-01 10:00 AEST
        assert_eq!(bar_date(ts, Some(AEST)), "2026-06-01");
    }

    #[test]
    fn bar_date_us_open_is_unchanged() {
        let ts = utc_ts(2026, 1, 5, 14, 30); // Mon 2026-01-05 09:30 EST
        assert_eq!(bar_date(ts, Some(EST)), "2026-01-05");
    }

    #[test]
    fn bar_date_missing_offset_falls_back_to_utc() {
        let ts = utc_ts(2026, 1, 4, 23, 0);
        assert_eq!(bar_date(ts, None), "2026-01-04");
    }

    /// gmtoffset must survive deserialisation of the daemon's chart payload —
    /// if the field is dropped, every AEDT bar silently shifts a day early.
    #[test]
    fn chart_response_parses_gmtoffset() {
        let json = r#"{
            "chart": {
                "result": [{
                    "meta": {
                        "instrumentType": "EQUITY",
                        "longName": "Washington H. Soul Pattinson",
                        "currency": "AUD",
                        "gmtoffset": 39600
                    },
                    "timestamp": [1767564000],
                    "indicators": { "quote": [{
                        "open": [37.0], "high": [37.5], "low": [36.9],
                        "close": [37.39], "volume": [100000]
                    }] }
                }],
                "error": null
            }
        }"#;
        let payload: YahooChartResponse = serde_json::from_str(json).unwrap();
        let result = &payload.chart.result.unwrap()[0];
        assert_eq!(result.meta.gmtoffset, Some(AEDT));
        // 1767564000 = 2026-01-04 22:00 UTC = 2026-01-05 09:00 AEDT
        assert_eq!(bar_date(result.timestamp.as_ref().unwrap()[0], result.meta.gmtoffset), "2026-01-05");
    }

    // -------------------------------------------------------------------------
    // ASX calendar and schedule functions (P3.3). A wrong holiday means a
    // silently skipped daily close (or a junk fetch on a closed day).
    // -------------------------------------------------------------------------

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn easter_dates_for_known_years() {
        // Computus verification against published calendars
        assert_eq!(easter_sunday(2024), d(2024, 3, 31));
        assert_eq!(easter_sunday(2025), d(2025, 4, 20));
        assert_eq!(easter_sunday(2026), d(2026, 4, 5));
        assert_eq!(good_friday(2026), d(2026, 4, 3));
        assert_eq!(easter_monday(2026), d(2026, 4, 6));
    }

    #[test]
    fn holidays_observed_on_weekdays_are_unshifted() {
        assert_eq!(observed_new_years_day(2026), d(2026, 1, 1)); // Thursday
        assert_eq!(observed_australia_day(2026), d(2026, 1, 26)); // Monday
        assert_eq!(observed_anzac_day(2025), d(2025, 4, 25)); // Friday
        assert_eq!(observed_christmas_day(2026), d(2026, 12, 25)); // Friday
    }

    #[test]
    fn weekend_holidays_shift_to_the_observed_weekday() {
        assert_eq!(observed_new_years_day(2022), d(2022, 1, 3), "Sat 1 Jan → Mon 3 Jan");
        assert_eq!(observed_new_years_day(2023), d(2023, 1, 2), "Sun 1 Jan → Mon 2 Jan");
        assert_eq!(observed_australia_day(2025), d(2025, 1, 27), "Sun 26 Jan → Mon 27 Jan");
        assert_eq!(observed_anzac_day(2026), d(2026, 4, 27), "Sat 25 Apr → Mon 27 Apr");
        // Christmas Sat/Sun both observe Mon 27th; Boxing Day then takes the 28th
        assert_eq!(observed_christmas_day(2027), d(2027, 12, 27), "Sat 25 Dec → Mon 27 Dec");
        assert_eq!(observed_boxing_day(2027), d(2027, 12, 28), "Sun 26 Dec → Tue 28 Dec");
        assert_eq!(observed_boxing_day(2026), d(2026, 12, 28), "Sat 26 Dec → Mon 28 Dec");
        // Boxing Day on a Monday stays put (Christmas Sunday observes Tue 27th)
        assert_eq!(observed_boxing_day(2022), d(2022, 12, 26));
        assert_eq!(observed_christmas_day(2022), d(2022, 12, 27));
    }

    #[test]
    fn kings_birthday_is_second_monday_of_june() {
        assert_eq!(queens_birthday(2025), d(2025, 6, 9));
        assert_eq!(queens_birthday(2026), d(2026, 6, 8)); // 1 June is itself a Monday
    }

    #[test]
    fn market_closed_on_weekends_holidays_and_overrides() {
        assert!(is_asx_market_closed(d(2026, 7, 11), &[]), "Saturday");
        assert!(is_asx_market_closed(d(2026, 7, 12), &[]), "Sunday");
        assert!(is_asx_market_closed(d(2026, 4, 3), &[]), "Good Friday");
        assert!(is_asx_market_closed(d(2026, 12, 28), &[]), "observed Boxing Day");
        assert!(!is_asx_market_closed(d(2026, 7, 8), &[]), "ordinary Wednesday");
        // A manual override closes an otherwise-open day
        assert!(is_asx_market_closed(d(2026, 7, 8), &[d(2026, 7, 8)]));
    }

    #[test]
    fn business_back_date_skips_weekends_and_manual_holidays() {
        // Wed 8 Jul back 3 business days: Tue 7, Mon 6, (skip weekend) Fri 3
        assert_eq!(calculate_business_back_date(d(2026, 7, 8), 3, &[]), d(2026, 7, 3));
        // With Mon 6 Jul closed by override, the third day lands on Thu 2 Jul
        assert_eq!(calculate_business_back_date(d(2026, 7, 8), 3, &[d(2026, 7, 6)]), d(2026, 7, 2));
    }

    #[test]
    fn holiday_list_parsing_skips_invalid_entries() {
        assert_eq!(parse_holiday_list(""), Vec::<NaiveDate>::new());
        assert_eq!(parse_holiday_list("  "), Vec::<NaiveDate>::new());
        assert_eq!(
            parse_holiday_list("2026-07-08, 2026-12-31"),
            vec![d(2026, 7, 8), d(2026, 12, 31)]
        );
        // Invalid entries are dropped, valid ones kept
        assert_eq!(parse_holiday_list("garbage, 2026-07-08, 31/12/2026"), vec![d(2026, 7, 8)]);
    }

    #[test]
    fn next_daily_run_today_or_tomorrow() {
        let at = |h: u32, min: u32| Local.with_ymd_and_hms(2026, 7, 8, h, min, 0).unwrap();

        // Before the target time → today at hh:mm
        let run = next_daily_run_from(at(10, 0), 16, 15);
        assert_eq!(run, at(16, 15));

        // After the target time → tomorrow at hh:mm
        let run = next_daily_run_from(at(17, 0), 16, 15);
        assert_eq!(run, Local.with_ymd_and_hms(2026, 7, 9, 16, 15, 0).unwrap());

        // Exactly at the target time counts as passed → tomorrow
        let run = next_daily_run_from(at(16, 15), 16, 15);
        assert_eq!(run, Local.with_ymd_and_hms(2026, 7, 9, 16, 15, 0).unwrap());

        // An invalid schedule falls back to the default 16:15
        let run = next_daily_run_from(at(10, 0), 99, 0);
        assert_eq!(run, at(16, 15));
    }
}

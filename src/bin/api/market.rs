//! Everything that talks to Yahoo, and the FX rates derived from it.
//!
//! Grouped so the network boundary is one file rather than a dozen functions
//! scattered through the handlers. Two things here are easy to miss elsewhere:
//! a history fetch is throttled per symbol, so a burst of requests serves
//! stored data instead of hammering the feed; and a quote is only trusted
//! through `is_usable_quote`, because the feed answers a delisted ticker with
//! a zero rather than an error.

use chrono::{NaiveDate, TimeZone, Utc};
use reqwest::Client;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Deserialize;
use std::{collections::HashMap, path::PathBuf};

use crate::{
    cache_current_price, insert_event_log, is_usable_quote, last_expected_trading_day,
    log_event_on_conn, open_db, CurrentPrice, PriceHistoryPoint,
};

/// Tracks when each symbol's daily history was last checked against Yahoo.
/// The DB is always read fresh; only the Yahoo *supplement attempt* is
/// skipped inside the window, so simultaneous heavy endpoints (portfolio
/// overview + enriched watchlist) don't re-fetch the same data.
pub(crate) static HISTORY_CHECKED: std::sync::OnceLock<std::sync::Mutex<HashMap<String, std::time::Instant>>> =
    std::sync::OnceLock::new();
pub(crate) const HISTORY_CHECK_TTL_SECS: u64 = 600;

pub(crate) fn history_recently_checked(symbol: &str) -> bool {
    let map = HISTORY_CHECKED
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    map.get(symbol)
        .map(|t| t.elapsed().as_secs() < HISTORY_CHECK_TTL_SECS)
        .unwrap_or(false)
}

pub(crate) fn mark_history_checked(symbol: &str) {
    let mut map = HISTORY_CHECKED
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    map.insert(symbol.to_string(), std::time::Instant::now());
    // Keep the map bounded even with pathological symbol churn
    if map.len() > 2048 {
        map.retain(|_, t| t.elapsed().as_secs() < HISTORY_CHECK_TTL_SECS);
    }
}

pub(crate) fn persist_price_history(conn: &Connection, symbol: &str, records: &[PriceHistoryPoint]) {
    let now = Utc::now().to_rfc3339();
    for r in records {
        // COALESCE on the OHLC columns so a close-only refresh can never blank
        // out bars the backfill already filled.
        if let Some(close) = r.close
            && let Err(err) = conn.execute(
                "INSERT INTO prices (symbol, date, open, high, low, close, volume, fetched_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT(symbol, date) DO UPDATE SET
                   open = COALESCE(excluded.open, open),
                   high = COALESCE(excluded.high, high),
                   low = COALESCE(excluded.low, low),
                   close = excluded.close,
                   volume = excluded.volume,
                   fetched_at = excluded.fetched_at",
                params![symbol, r.date, r.open, r.high, r.low, close, r.volume, now],
            ) {
                log_event_on_conn(conn, "warn", "price_persist", Some(symbol), &format!("Failed to persist price history: {}", err));
            }
    }
}

pub(crate) async fn fetch_price_history(db_path: &PathBuf, symbol: &str, days: i64) -> Result<Vec<PriceHistoryPoint>, String> {
    // Scope the connection so this future stays Send — it can then be run
    // concurrently for many symbols via JoinSet.
    let mut history = {
        let conn = open_db(db_path).map_err(|err| err.to_string())?;
        let mut stmt = conn
            .prepare(
                "SELECT date, open, high, low, close, volume FROM prices
                 WHERE symbol = ?1 AND close IS NOT NULL
                 ORDER BY date DESC
                 LIMIT ?2",
            )
            .map_err(|err| err.to_string())?;

        let rows = stmt
            .query_map(params![symbol, days], |row| {
                Ok(PriceHistoryPoint {
                    date: row.get(0)?,
                    open: row.get(1)?,
                    high: row.get(2)?,
                    low: row.get(3)?,
                    close: row.get(4)?,
                    volume: row.get(5)?,
                })
            })
            .map_err(|err| err.to_string())?;

        rows.collect::<Result<Vec<_>, _>>().map_err(|err| err.to_string())?
    };
    history.reverse();

    let last_trading_day = last_expected_trading_day(Utc::now(), symbol);

    let last_stored = history.last().map(|h| h.date.as_str()).unwrap_or("").to_string();
    let needs_supplement = last_stored < last_trading_day;
    let has_enough_data = history.len() as i64 >= days / 2;
    // Only attempt Yahoo once per symbol per window — bursts of requests
    // (dashboard + watchlist opening together) then serve stored data.
    let wants_yahoo = history.is_empty() || !has_enough_data || needs_supplement;
    let may_fetch = wants_yahoo && !history_recently_checked(symbol);
    if may_fetch {
        mark_history_checked(symbol);
    }

    let client = Client::builder()
        .user_agent("stocks-api/1.0")
        .build()
        .map_err(|err| err.to_string())?;

    if may_fetch && (history.is_empty() || !has_enough_data) {
        match fetch_price_history_from_yahoo(&client, symbol, days).await {
            Ok(records) => {
                if records.len() > history.len() {
                    match open_db(db_path) {
                        Ok(conn) => persist_price_history(&conn, symbol, &records),
                        Err(err) => {
                            let _ = insert_event_log(db_path, "warn", "price_history_fetch", "api", Some(symbol), &format!("Fetched history could not be persisted: {}", err));
                        }
                    }
                    return Ok(records);
                }
            }
            Err(err) => {
                let _ = insert_event_log(db_path, "warn", "price_history_fetch", "api", Some(symbol), &format!("Yahoo history fetch failed, serving stored data: {}", err));
            }
        }
    } else if may_fetch && needs_supplement {
        // Stored data is behind the last trading day — fetch from Yahoo and append missing records
        match fetch_price_history_from_yahoo(&client, symbol, days).await {
            Ok(yahoo) => {
                let new_records: Vec<_> = yahoo.into_iter().filter(|r| r.date > last_stored).collect();
                match open_db(db_path) {
                    Ok(conn) => persist_price_history(&conn, symbol, &new_records),
                    Err(err) => {
                        let _ = insert_event_log(db_path, "warn", "price_history_fetch", "api", Some(symbol), &format!("Fetched supplement could not be persisted: {}", err));
                    }
                }
                history.extend(new_records);
            }
            Err(err) => {
                let _ = insert_event_log(db_path, "warn", "price_history_fetch", "api", Some(symbol), &format!("Yahoo supplement fetch failed, serving stored data: {}", err));
            }
        }
    }

    Ok(history)
}

/// Fetch daily history for many symbols with bounded concurrency.
pub(crate) async fn fetch_histories(db_path: &PathBuf, symbols: &[String], days: i64) -> HashMap<String, Vec<PriceHistoryPoint>> {
    let mut out = HashMap::new();
    for chunk in symbols.chunks(5) {
        let mut set = tokio::task::JoinSet::new();
        for sym in chunk {
            let db = db_path.clone();
            let sym = sym.clone();
            set.spawn(async move {
                let result = fetch_price_history(&db, &sym, days).await;
                (sym, result)
            });
        }
        while let Some(joined) = set.join_next().await {
            if let Ok((sym, result)) = joined {
                match result {
                    Ok(history) => {
                        out.insert(sym, history);
                    }
                    Err(err) => {
                        let _ = insert_event_log(db_path, "warn", "price_history_fetch", "api", Some(&sym), &err);
                    }
                }
            }
        }
    }
    out
}

/// Yahoo's symbol for "how many AUD one unit of `currency` buys", e.g. USDAUD=X.
///
/// FX pairs are stored in `prices` as ordinary symbols. Yahoo serves them as
/// daily OHLC bars exactly like equities, so the whole existing pipeline —
/// fetch, persist, and the backfill_ohlc tool — works on them unchanged, and
/// `prices` is already exempt from audit triggers as re-fetchable machine data.
pub(crate) fn fx_pair_symbol(currency: &str) -> String {
    format!("{}AUD=X", currency.trim().to_uppercase())
}

/// The AUD value of one unit of `currency` on `date`, from stored rates.
///
/// As-of semantics: FX does not trade at weekends, and a holding still needs
/// valuing on those days, so this takes the most recent rate on or before the
/// date rather than requiring an exact match. AUD is the base and always 1.0.
pub(crate) fn fx_rate_on(conn: &Connection, currency: &str, date: &str) -> Option<f64> {
    let ccy = currency.trim().to_uppercase();
    if ccy.is_empty() || ccy == "AUD" {
        return Some(1.0);
    }
    conn.query_row(
        "SELECT close FROM prices
          WHERE symbol = ?1 AND close IS NOT NULL AND date <= ?2
          ORDER BY date DESC LIMIT 1",
        params![fx_pair_symbol(&ccy), date],
        |row| row.get::<_, f64>(0),
    )
    .optional()
    .ok()
    .flatten()
}

#[derive(Deserialize)]
pub(crate) struct YahooQuoteResponse {
    pub(crate) chart: YahooChartData,
}

#[derive(Deserialize)]
pub(crate) struct YahooChartData {
    pub(crate) result: Option<Vec<YahooResultData>>,
}

#[derive(Deserialize)]
pub(crate) struct YahooResultData {
    pub(crate) meta: YahooMeta,
    #[serde(default)]
    pub(crate) indicators: Option<YahooHistoryIndicators>,
}

#[allow(non_snake_case)]
#[derive(Deserialize)]
pub(crate) struct YahooMeta {
    #[serde(rename = "regularMarketPrice")]
    pub(crate) regular_market_price: Option<f64>,
    #[serde(rename = "regularMarketChange")]
    pub(crate) regular_market_change: Option<f64>,
    #[serde(rename = "regularMarketChangePercent")]
    pub(crate) regular_market_change_percent: Option<f64>,
    #[serde(rename = "regularMarketVolume")]
    pub(crate) regular_market_volume: Option<i64>,
    #[serde(rename = "chartPreviousClose")]
    pub(crate) chart_previous_close: Option<f64>,
    #[serde(rename = "regularMarketDayHigh")]
    pub(crate) regular_market_day_high: Option<f64>,
    #[serde(rename = "regularMarketDayLow")]
    pub(crate) regular_market_day_low: Option<f64>,
    /// Yahoo's chart meta has no open field — filled from the quote arrays in
    /// `fetch_current_price`, hence `default` rather than a rename.
    #[serde(default)]
    pub(crate) day_open: Option<f64>,
    #[serde(rename = "instrumentType")]
    pub(crate) instrument_type: Option<String>,
    #[serde(rename = "longName")]
    pub(crate) long_name: Option<String>,
    pub(crate) currency: Option<String>,
    #[serde(rename = "regularMarketTime")]
    pub(crate) regular_market_time: Option<i64>,
    /// Exchange UTC offset in seconds — needed to date bars correctly
    pub(crate) gmtoffset: Option<i64>,
}

/// Convert a Yahoo bar/event timestamp to the exchange-local trading date.
/// Yahoo stamps daily bars at the market open; for exchanges ahead of UTC
/// (ASX opens 10:00 Sydney = 23:00 UTC the *previous* day during daylight
/// saving) the UTC date is one day early, so the date must be taken in
/// exchange time: UTC + meta.gmtoffset.
pub(crate) fn yahoo_local_date(ts: i64, gmtoffset: Option<i64>) -> Option<NaiveDate> {
    Utc.timestamp_opt(ts + gmtoffset.unwrap_or(0), 0)
        .single()
        .map(|dt| dt.date_naive())
}

#[derive(Deserialize)]
pub(crate) struct YahooHistoryResponse {
    pub(crate) chart: YahooHistoryChart,
}

#[derive(Deserialize)]
pub(crate) struct YahooHistoryChart {
    pub(crate) result: Option<Vec<YahooHistoryResult>>,
    pub(crate) error: Option<serde_json::Value>,
}

#[derive(Deserialize)]
pub(crate) struct YahooHistoryResult {
    pub(crate) meta: Option<YahooMeta>,
    pub(crate) timestamp: Option<Vec<i64>>,
    pub(crate) indicators: YahooHistoryIndicators,
}

#[derive(Deserialize)]
pub(crate) struct YahooHistoryIndicators {
    pub(crate) quote: Vec<YahooHistoryQuote>,
}

#[derive(Deserialize)]
pub(crate) struct YahooHistoryQuote {
    pub(crate) open: Option<Vec<Option<f64>>>,
    pub(crate) high: Option<Vec<Option<f64>>>,
    pub(crate) low: Option<Vec<Option<f64>>>,
    pub(crate) close: Option<Vec<Option<f64>>>,
    pub(crate) volume: Option<Vec<Option<i64>>>,
}

pub(crate) async fn fetch_price_history_from_yahoo(client: &Client, symbol: &str, days: i64) -> Result<Vec<PriceHistoryPoint>, String> {
    let range = if days <= 365 { "1y" } else { "2y" };
    let url = format!(
        "https://query1.finance.yahoo.com/v8/finance/chart/{}?interval=1d&range={}",
        symbol,
        range
    );

    let response = client
        .get(&url)
        .header("User-Agent", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|err| err.to_string())?;
    let response = response.error_for_status().map_err(|err| err.to_string())?;
    let payload: YahooHistoryResponse = response.json().await.map_err(|err| err.to_string())?;

    let result = payload
        .chart
        .result
        .as_ref()
        .and_then(|items| items.first())
        .ok_or_else(|| {
            if let Some(error) = payload.chart.error {
                anyhow::anyhow!("No chart result found for {}: {}", symbol, error).to_string()
            } else {
                format!("No chart result found for {}", symbol)
            }
        })?;

    let timestamps = result.timestamp.as_ref().ok_or_else(|| format!("No timestamp data in Yahoo response for {}", symbol))?;
    let quote = result
        .indicators
        .quote
        .first()
        .ok_or_else(|| format!("No quote data in Yahoo response for {}", symbol))?;

    let gmtoffset = result.meta.as_ref().and_then(|m| m.gmtoffset);
    let mut records = Vec::with_capacity(timestamps.len());
    for (index, ts) in timestamps.iter().enumerate() {
        let date = yahoo_local_date(*ts, gmtoffset)
            .ok_or_else(|| format!("Invalid timestamp {} for {}", ts, symbol))?
            .format("%Y-%m-%d")
            .to_string();

        let close = quote.close.as_ref().and_then(|v| v.get(index).cloned().flatten());
        let volume = quote.volume.as_ref().and_then(|v| v.get(index).cloned().flatten());
        let open = quote.open.as_ref().and_then(|v| v.get(index).cloned().flatten());
        let high = quote.high.as_ref().and_then(|v| v.get(index).cloned().flatten());
        let low = quote.low.as_ref().and_then(|v| v.get(index).cloned().flatten());

        if close.is_some() {
            records.push(PriceHistoryPoint { date, open, high, low, close, volume });
        }
    }

    if records.is_empty() {
        Err(format!("Yahoo returned no historical prices for {}", symbol))
    } else {
        Ok(records)
    }
}

pub(crate) async fn fetch_current_price(client: &Client, symbol: &str) -> Result<YahooMeta, String> {
    let url = format!(
        "https://query1.finance.yahoo.com/v8/finance/chart/{}?interval=1d&range=1d",
        symbol
    );

    let response = client
        .get(&url)
        .send()
        .await
        .map_err(|err| err.to_string())?
        .error_for_status()
        .map_err(|err| err.to_string())?;

    let data: YahooQuoteResponse = response
        .json()
        .await
        .map_err(|err| err.to_string())?;

    let result = data
        .chart
        .result
        .and_then(|r| r.into_iter().next())
        .ok_or_else(|| "No chart data available".to_string())?;

    let mut meta = result.meta;
    let day_quote = result.indicators.as_ref().and_then(|ind| ind.quote.first());
    // Fall back to the time-series volume when regularMarketVolume is absent in metadata
    if meta.regular_market_volume.is_none() {
        meta.regular_market_volume = day_quote
            .and_then(|q| q.volume.as_ref())
            .and_then(|vols| vols.iter().filter_map(|v| *v).next_back());
    }
    // range=1d returns a single daily bar, so the session's open is its first
    // non-null open; high/low fall back to the bar when meta omits them.
    if let Some(q) = day_quote {
        meta.day_open = q.open.as_ref().and_then(|v| v.iter().flatten().next().copied());
        if meta.regular_market_day_high.is_none() {
            meta.regular_market_day_high = q
                .high
                .as_ref()
                .and_then(|v| v.iter().flatten().copied().reduce(f64::max));
        }
        if meta.regular_market_day_low.is_none() {
            meta.regular_market_day_low = q
                .low
                .as_ref()
                .and_then(|v| v.iter().flatten().copied().reduce(f64::min));
        }
    }
    Ok(meta)
}

/// AUD rate per currency, served from the price cache when fresh (<1h),
/// refreshed from Yahoo and re-cached otherwise, falling back to a stale
/// cached value when Yahoo is unreachable.
pub(crate) async fn resolve_fx_rates(db_path: &PathBuf, currencies: &[String]) -> HashMap<String, Option<f64>> {
    let mut rates = HashMap::new();
    if currencies.is_empty() {
        return rates;
    }
    let client = Client::builder().user_agent("stocks-api/1.0").build().ok();
    for currency in currencies {
        let pair = format!("{}AUD=X", currency);
        let cached: Option<(Option<f64>, String)> = open_db(db_path).ok().and_then(|conn| {
            conn.query_row(
                "SELECT price, last_updated FROM cached_current_prices WHERE symbol = ?1",
                params![pair],
                |row| Ok((row.get::<_, Option<f64>>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .ok()
            .flatten()
        });
        let fresh = cached
            .as_ref()
            .map(|(price, updated)| {
                price.is_some()
                    && chrono::DateTime::parse_from_rfc3339(updated)
                        .map(|t| (Utc::now() - t.with_timezone(&Utc)).num_seconds() < 3600)
                        .unwrap_or(false)
            })
            .unwrap_or(false);
        if fresh {
            rates.insert(currency.clone(), cached.and_then(|c| c.0));
            continue;
        }
        let mut live: Option<f64> = None;
        if let Some(client) = &client {
            match fetch_current_price(client, &pair).await {
                Ok(meta) => {
                    live = meta.regular_market_price.filter(|p| is_usable_quote(Some(*p)));
                    if live.is_some()
                        && let Ok(conn) = open_db(db_path) {
                            let _ = cache_current_price(&conn, &CurrentPrice {
                                symbol: pair.clone(),
                                price: live,
                                change: None,
                                change_percent: None,
                                volume: None,
                                day_open: None,
                                day_high: None,
                                day_low: None,
                                last_updated: Utc::now().to_rfc3339(),
                                price_date: None,
                                error: None,
                            });
                        }
                }
                Err(err) => {
                    let _ = insert_event_log(db_path, "warn", "fx_fetch", "api", None, &format!("FX rate fetch failed for {}, using cached value if available: {}", currency, err));
                }
            }
        }
        rates.insert(currency.clone(), live.or(cached.and_then(|c| c.0)));
    }
    rates
}

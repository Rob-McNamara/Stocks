//! Everything that talks to Yahoo, and the FX rates derived from it.
//!
//! Grouped so the network boundary is one file rather than a dozen functions
//! scattered through the handlers. Two things here are easy to miss elsewhere:
//! a history fetch is throttled per symbol, so a burst of requests serves
//! stored data instead of hammering the feed; and a quote is only trusted
//! through `is_usable_quote`, because the feed answers a delisted ticker with
//! a zero rather than an error.

use chrono::{NaiveDate, Utc};
use reqwest::Client;
use rusqlite::{params, Connection, OptionalExtension};
use stocks::yahoo::ChartResponse;
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

/// Widen a bar's high/low to contain its own open and close.
///
/// Yahoo's daily bars are not always self-consistent: its FX bars in
/// particular (USDAUD=X) carry a close a hair outside the high/low, and a bar
/// like that draws a candle body past its own wick. The open and close are
/// trades in the session, so the true range contains them. A side with no
/// high or low stays empty — see `session_range`.
pub(crate) fn widen_bar_to_body(bar: &PriceHistoryPoint) -> PriceHistoryPoint {
    let (high, low) = session_range([bar.high, None], [bar.low, None], [bar.open, bar.close]);
    PriceHistoryPoint { date: bar.date.clone(), open: bar.open, high, low, close: bar.close, volume: bar.volume }
}

pub(crate) fn persist_price_history(conn: &Connection, symbol: &str, records: &[PriceHistoryPoint]) {
    let now = Utc::now().to_rfc3339();
    // The shared writer refuses a zero close, widens each bar to its own body
    // and never blanks a backfilled open/high/low — the same rules the price
    // daemon now follows.
    for r in records {
        let bar = stocks::prices::DailyBar { date: &r.date, open: r.open, high: r.high, low: r.low, close: r.close, volume: r.volume };
        match stocks::prices::upsert_daily_bar(conn, symbol, &bar, &now) {
            Ok(stocks::prices::Upsert::Written) => {}
            Ok(stocks::prices::Upsert::RefusedUnusableClose) => {
                if r.close.is_some() {
                    log_event_on_conn(conn, "warn", "price_persist", Some(symbol), &format!("Refusing to write non-positive close {:?} for {} — keeping the last good bar", r.close, r.date));
                }
            }
            Err(err) => {
                log_event_on_conn(conn, "warn", "price_persist", Some(symbol), &format!("Failed to persist price history: {}", err));
            }
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

    let client = http_client();

    if may_fetch && (history.is_empty() || !has_enough_data) {
        match fetch_price_history_from_yahoo(client, symbol, days).await {
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
        match fetch_price_history_from_yahoo(client, symbol, days).await {
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
pub(crate) async fn fetch_histories(db_path: &std::path::Path, symbols: &[String], days: i64) -> HashMap<String, Vec<PriceHistoryPoint>> {
    let mut out = HashMap::new();
    for chunk in symbols.chunks(5) {
        let mut set = tokio::task::JoinSet::new();
        for sym in chunk {
            let db = db_path.to_path_buf();
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

/// The API's one HTTP client.
///
/// Every request used to build its own, which threw away pooled connections
/// to Yahoo between calls and left a dozen places to keep the timeouts right.
/// A `reqwest::Client` is cheap to share and safe across threads. A caller
/// that legitimately waits longer — the AI analysis — sets a timeout on its
/// own request rather than building another client.
pub(crate) fn http_client() -> &'static Client {
    static CLIENT: std::sync::OnceLock<Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| {
        stocks::http::builder("stocks-api/1.0")
            .build()
            .expect("the HTTP client's fixed configuration is valid")
    })
}

/// A quote's chart meta. The response types live in `stocks::yahoo`, shared
/// with the daemons and backfill tools that read the same endpoint.
pub(crate) use stocks::yahoo::Meta as YahooMeta;

/// Exchange-local trading date of a Yahoo timestamp; see
/// `stocks::yahoo::local_date`.
pub(crate) fn yahoo_local_date(ts: i64, gmtoffset: Option<i64>) -> Option<NaiveDate> {
    stocks::yahoo::local_date(ts, gmtoffset)
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
    let payload: ChartResponse = response.json().await.map_err(|err| err.to_string())?;
    let result = payload.chart.into_first(symbol)?;

    let timestamps = result.timestamp.as_ref().ok_or_else(|| format!("No timestamp data in Yahoo response for {}", symbol))?;
    let quote = result
        .quote()
        .ok_or_else(|| format!("No quote data in Yahoo response for {}", symbol))?;

    let gmtoffset = result.gmtoffset();
    let mut records = Vec::with_capacity(timestamps.len());
    for (index, ts) in timestamps.iter().enumerate() {
        let date = yahoo_local_date(*ts, gmtoffset)
            .ok_or_else(|| format!("Invalid timestamp {} for {}", ts, symbol))?
            .format("%Y-%m-%d")
            .to_string();

        let close = quote.close_at(index);
        let volume = quote.volume_at(index);
        let open = quote.open_at(index);
        let high = quote.high_at(index);
        let low = quote.low_at(index);

        if close.is_some() {
            // Widened here as well as on persist, so a first load that serves
            // these records straight to the chart matches what is stored.
            records.push(widen_bar_to_body(&PriceHistoryPoint { date, open, high, low, close, volume }));
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

    let data: ChartResponse = response
        .json()
        .await
        .map_err(|err| err.to_string())?;

    let mut result = data
        .chart
        .result
        .and_then(|r| r.into_iter().next())
        .ok_or_else(|| "No chart data available".to_string())?;

    let mut meta = result.meta.take().ok_or_else(|| "No chart data available".to_string())?;
    let day_quote = result.quote();
    // Fall back to the time-series volume when regularMarketVolume is absent in metadata
    if meta.regular_market_volume.is_none() {
        meta.regular_market_volume = day_quote
            .and_then(|q| q.volume.as_ref())
            .and_then(|vols| vols.iter().filter_map(|v| *v).next_back());
    }
    // range=1d returns a single daily bar, so the session's open is its first
    // non-null open.
    if let Some(q) = day_quote {
        meta.day_open = q.open.as_ref().and_then(|v| v.iter().flatten().next().copied());
        let bar_high = q.high.as_ref().and_then(|v| v.iter().flatten().copied().reduce(f64::max));
        let bar_low = q.low.as_ref().and_then(|v| v.iter().flatten().copied().reduce(f64::min));
        let (high, low) = session_range(
            [bar_high, meta.regular_market_day_high],
            [bar_low, meta.regular_market_day_low],
            [meta.day_open, meta.regular_market_price],
        );
        meta.regular_market_day_high = high;
        meta.regular_market_day_low = low;
    }
    Ok(meta)
}

/// The session's high/low as the envelope of every figure Yahoo gives for it.
///
/// `regularMarketDayHigh`/`Low` in the chart meta come from a different feed
/// than the bar arrays and can leave out the opening print: EXPD on 2026-09-17
/// opened at its high of 190.92, but meta reported a high of 190.255. Pairing
/// that meta high with the bar's open stored a bar whose open sat above its
/// own high, and the chart drew a candle body outside its wick. Every one of
/// these figures is a real trade in the session, so the true range contains
/// them all, including the open and the latest price.
pub(crate) fn session_range(
    highs: [Option<f64>; 2],
    lows: [Option<f64>; 2],
    trades: [Option<f64>; 2],
) -> (Option<f64>, Option<f64>) {
    stocks::prices::session_range(highs, lows, trades)
}

/// AUD rate per currency, served from the price cache when fresh (<1h),
/// refreshed from Yahoo and re-cached otherwise, falling back to a stale
/// cached value when Yahoo is unreachable.
pub(crate) async fn resolve_fx_rates(db_path: &PathBuf, currencies: &[String]) -> HashMap<String, Option<f64>> {
    let mut rates = HashMap::new();
    if currencies.is_empty() {
        return rates;
    }
    let client = http_client();
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
        // Last resort: the daily rate history the FX sync keeps. A day-old
        // close is a far better answer than none, which leaves every holding
        // in this currency without an AUD value.
        let stored = || {
            open_db(db_path).ok().and_then(|conn| {
                fx_rate_on(&conn, currency, &chrono::Local::now().format("%Y-%m-%d").to_string())
            })
        };
        rates.insert(currency.clone(), live.or(cached.and_then(|c| c.0)).or_else(stored));
    }
    rates
}

//! Exchange rates: one-off lookups, the stored daily rate history, and the
//! currencies the portfolio needs rates for.

use super::*;

#[derive(Deserialize)]
pub(crate) struct FxRateQuery {
    pub(crate) currency: String,
    pub(crate) date: String,
}

/// Closest AUD rate on or before `target_date` for `currency` (e.g. "USD").
/// Returns (rate, rate_date).
pub(crate) async fn fetch_fx_rate_on_date(currency: &str, target_date: NaiveDate) -> Result<(f64, String), String> {
    let pair = format!("{}AUD=X", currency.trim().to_uppercase());
    let client = http_client();
    // Fetch a week around the target date to cover weekends/holidays
    let period1 = Utc.from_utc_datetime(&(target_date - chrono::Duration::days(7)).and_hms_opt(0, 0, 0).unwrap()).timestamp();
    let period2 = Utc.from_utc_datetime(&(target_date + chrono::Duration::days(2)).and_hms_opt(0, 0, 0).unwrap()).timestamp();
    let url = format!(
        "https://query1.finance.yahoo.com/v8/finance/chart/{}?interval=1d&period1={}&period2={}",
        pair, period1, period2
    );
    let response = client
        .get(&url)
        .send()
        .await
        .map_err(|err| format!("FX fetch failed for {}: {}", pair, err))?;
    let payload: stocks::yahoo::ChartResponse = response
        .json()
        .await
        .map_err(|err| format!("FX response parse failed for {}: {}", pair, err))?;
    let result = payload
        .chart
        .result
        .as_ref()
        .and_then(|r| r.first())
        .ok_or_else(|| format!("No FX data for {}", pair))?;
    let timestamps = result.timestamp.as_ref().ok_or_else(|| "No timestamp data".to_string())?;
    let closes = result
        .quote()
        .and_then(|q| q.close.as_ref())
        .ok_or_else(|| "No close data".to_string())?;
    // Find the entry closest to and on-or-before the target date
    let target_str = target_date.format("%Y-%m-%d").to_string();
    let mut best: Option<(f64, String)> = None;
    let gmtoffset = result.gmtoffset();
    for (i, ts) in timestamps.iter().enumerate() {
        let date_str = stocks::yahoo::bar_date(*ts, gmtoffset)
            .map(|d| d.format("%Y-%m-%d").to_string())
            .unwrap_or_default();
        if date_str <= target_str
            && let Some(Some(rate)) = closes.get(i) {
                best = Some((*rate, date_str));
            }
    }
    best.ok_or_else(|| format!("No FX rate found on or before {}", target_str))
}

/// How far before a date the last stored rate may sit and still stand for it
/// once the history is up to date: a weekend, or a day the FX market did not
/// trade. The sector rotation allows the same.
const FX_LOOKBACK_DAYS: i64 = 7;

/// One stored daily rate: the day it is for, its close, and when it was fetched.
pub(crate) struct StoredRate {
    pub(crate) date: NaiveDate,
    pub(crate) rate: f64,
    pub(crate) fetched_at: Option<chrono::DateTime<Utc>>,
}

/// The last stored rate for `currency` on or before `date`.
pub(crate) fn stored_rate_on_or_before(conn: &Connection, currency: &str, date: NaiveDate) -> rusqlite::Result<Option<StoredRate>> {
    let row = conn
        .query_row(
            "SELECT date, close, fetched_at FROM prices
              WHERE symbol = ?1 AND close IS NOT NULL AND date <= ?2
              ORDER BY date DESC LIMIT 1",
            params![fx_pair_symbol(currency), date.format("%Y-%m-%d").to_string()],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?, row.get::<_, Option<String>>(2)?)),
        )
        .optional()?;
    Ok(row.and_then(|(day, rate, fetched_at)| {
        Some(StoredRate {
            date: NaiveDate::parse_from_str(&day, "%Y-%m-%d").ok()?,
            rate,
            fetched_at: fetched_at
                .and_then(|at| chrono::DateTime::parse_from_rfc3339(&at).ok())
                .map(|at| at.with_timezone(&Utc)),
        })
    }))
}

/// Whether a stored rate is `date`'s own, so it can be given without asking
/// Yahoo first.
///
/// It is when nothing newer can exist for the date: the close is for the date
/// itself, or for the Friday before a weekend the date falls in, and it was
/// fetched after its day's trading had ended. A close fetched mid-session is
/// that moment's rate, not the day's. Anything further back is the stored
/// history falling behind — it is only brought up to date when the API starts
/// and on the startup refresh — and is days stale: that is how a purchase on
/// 15 September came to be recorded at 1 September's rate.
pub(crate) fn is_rate_for(stored: &StoredRate, date: NaiveDate) -> bool {
    let only_weekend_since = stored
        .date
        .iter_days()
        .skip(1)
        .take_while(|day| *day <= date)
        .all(|day| matches!(day.weekday(), chrono::Weekday::Sat | chrono::Weekday::Sun));
    // The FX day ends at midnight London time, which is never later than
    // midnight UTC.
    let day_closed = stored.fetched_at.is_some_and(|at| at.date_naive() > stored.date);
    stored.date <= date && only_weekend_since && day_closed
}

/// Re-read a currency's last year of daily rates from Yahoo and store them,
/// so the newest day is present and every earlier close is the day's final one.
pub(crate) async fn refresh_fx_history(db_path: &Path, currency: &str) -> Result<(), String> {
    let pair = fx_pair_symbol(currency);
    let records = fetch_price_history_from_yahoo(http_client(), &pair, 365).await?;
    let mut conn = open_db(db_path).map_err(|err| err.to_string())?;
    let tx = conn.transaction().map_err(|err| err.to_string())?;
    persist_price_history(&tx, &pair, &records);
    tx.commit().map_err(|err| err.to_string())
}

#[utoipa::path(get, path = "/api/v1/fx-rate", tag = "fx", responses((status = 200, description = "Get fx rate for date")))]
#[get("/api/fx-rate")]
pub(crate) async fn get_fx_rate_for_date(db_path: web::Data<PathBuf>, query: web::Query<FxRateQuery>) -> impl Responder {
    let currency = query.currency.trim().to_uppercase();
    let target_date = match NaiveDate::parse_from_str(&query.date, "%Y-%m-%d") {
        Ok(d) => d,
        Err(_) => return err_bad_request("Invalid date format, use YYYY-MM-DD"),
    };
    if currency == "AUD" {
        return HttpResponse::Ok().json(serde_json::json!({ "rate": 1.0, "date": query.date, "source": "base" }));
    }
    let stored = || {
        open_db(db_path.as_ref())
            .and_then(|conn| stored_rate_on_or_before(&conn, &currency, target_date))
            .unwrap_or_else(|err| {
                let _ = insert_event_log(&db_path, "warn", "fx_fetch", "api", None, &format!("Stored {currency}/AUD rates could not be read: {err}"));
                None
            })
    };
    // The date given back is the rate's own, so the form shows which day it
    // came from rather than repeating the day it was asked for.
    let reply = |found: StoredRate| {
        HttpResponse::Ok().json(serde_json::json!({
            "rate": found.rate,
            "date": found.date.format("%Y-%m-%d").to_string(),
            "source": "stored",
        }))
    };

    // The stored history, when it holds the date's own rate: it avoids a
    // Yahoo round trip and is the rate the valuation code will use.
    if let Some(found) = stored().filter(|found| is_rate_for(found, target_date)) {
        return reply(found);
    }
    // Otherwise it is behind the date, or holds a close taken mid-session:
    // bring it up to date first. A close within the lookback then stands for
    // the date, as a Friday does for a weekend.
    if let Err(err) = refresh_fx_history(&db_path, &currency).await {
        let _ = insert_event_log(&db_path, "warn", "fx_fetch", "api", None, &format!("{currency}/AUD rates not refreshed for {target_date}: {err}"));
    }
    if let Some(found) = stored().filter(|found| (target_date - found.date).num_days() <= FX_LOOKBACK_DAYS) {
        return reply(found);
    }

    match fetch_fx_rate_on_date(&currency, target_date).await {
        Ok((rate, date)) => HttpResponse::Ok().json(serde_json::json!({ "rate": rate, "date": date, "source": "yahoo" })),
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "fx_fetch", "api", None, &err);
            if err.starts_with("No ") {
                err_not_found(err)
            } else {
                err_internal(err)
            }
        }
    }
}

#[derive(Deserialize)]
pub(crate) struct FxRatesQuery {
    pub(crate) currencies: Option<String>,
}

#[utoipa::path(get, path = "/api/v1/fx-rates", tag = "fx", responses((status = 200, description = "Get fx rates")))]
#[get("/api/fx-rates")]
pub(crate) async fn get_fx_rates(db_path: web::Data<PathBuf>, query: web::Query<FxRatesQuery>) -> impl Responder {
    let client = http_client();
    // Rates are AUD per 1 unit of each requested currency (e.g. USD → "USDAUD=X").
    let mut currencies: Vec<String> = query
        .currencies
        .as_deref()
        .unwrap_or("USD")
        .split(',')
        .map(|c| c.trim().to_uppercase())
        .filter(|c| !c.is_empty() && c != "AUD" && c.chars().all(|ch| ch.is_ascii_alphabetic()) && c.len() == 3)
        .collect();
    currencies.sort();
    currencies.dedup();

    let mut rates = serde_json::Map::new();
    for currency in &currencies {
        match fetch_current_price(client, &format!("{}AUD=X", currency)).await {
            Ok(meta) => {
                rates.insert(currency.clone(), serde_json::json!(meta.regular_market_price));
            }
            Err(err) => {
                let _ = insert_event_log(&db_path, "error", "fx_fetch", "api", None, &format!("FX rate fetch failed for {}: {}", currency, err));
                rates.insert(currency.clone(), serde_json::Value::Null);
            }
        }
    }
    HttpResponse::Ok().json(serde_json::Value::Object(rates))
}

/// The oldest stored rate for a currency.
///
/// Rate history reaches back about two years; dividend history reaches back
/// decades. An old foreign dividend converted at the oldest rate held is an
/// approximation, but a far better one than leaving the payment out.
pub(crate) fn earliest_fx_rate(conn: &Connection, currency: &str) -> Option<f64> {
    conn.query_row(
        "SELECT close FROM prices WHERE symbol = ?1 AND close IS NOT NULL ORDER BY date LIMIT 1",
        params![fx_pair_symbol(currency)],
        |r| r.get::<_, f64>(0),
    )
    .optional()
    .ok()
    .flatten()
}

/// Currencies the portfolio actually holds value in, excluding AUD (the base).
///
/// Derived from the data rather than configured, so adding a GBP holding starts
/// its rate history automatically. Reads both `symbol_info` (the authority for a
/// symbol's currency) and `holdings_transactions` (which carries the currency a
/// trade actually settled in, and covers symbols missing from symbol_info).
pub(crate) fn fx_currencies_in_use(conn: &Connection) -> Vec<String> {
    let mut stmt = match conn.prepare(
        "SELECT DISTINCT UPPER(TRIM(currency)) AS ccy FROM (
             SELECT currency FROM symbol_info WHERE currency IS NOT NULL
             UNION ALL
             SELECT currency FROM holdings_transactions WHERE currency IS NOT NULL
         )
         WHERE ccy <> '' AND ccy <> 'AUD'
         ORDER BY ccy",
    ) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    stmt.query_map([], |row| row.get::<_, String>(0))
        .map(|rows| rows.flatten().collect())
        .unwrap_or_default()
}

/// Result of one FX sync pass, for logging and the manual endpoint.
#[derive(Serialize, Default)]
pub(crate) struct FxSyncReport {
    pub(crate) currencies: Vec<String>,
    pub(crate) fetched: usize,
    pub(crate) skipped: usize,
    pub(crate) bars_written: usize,
    pub(crate) errors: Vec<String>,
}

/// Bring stored FX history up to date for every currency the portfolio uses.
///
/// Valuing a foreign holding on a past date needs that date's rate, so the
/// rates live in `prices` as daily bars rather than being fetched live per
/// lookup. Already-current pairs cost one indexed query and no network, which
/// makes this safe to call on every startup.
pub(crate) async fn sync_fx_history(db_path: &PathBuf, days: i64) -> FxSyncReport {
    let mut report = FxSyncReport::default();

    let currencies = match open_db(db_path) {
        Ok(conn) => fx_currencies_in_use(&conn),
        Err(err) => {
            let message = format!("FX sync could not read currencies: {}", err);
            let _ = insert_event_log(db_path, "error", "fx_sync", "api", None, &message);
            report.errors.push(message);
            return report;
        }
    };
    report.currencies = currencies.clone();
    if currencies.is_empty() {
        return report;
    }

    let client = http_client();

    for currency in &currencies {
        let pair = fx_pair_symbol(currency);

        // Skip the network when the latest stored bar is already the most
        // recent one we could expect.
        let latest: Option<String> = open_db(db_path).ok().and_then(|conn| {
            conn.query_row(
                "SELECT MAX(date) FROM prices WHERE symbol = ?1 AND close IS NOT NULL",
                params![pair],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()
            .ok()
            .flatten()
            .flatten()
        });
        if latest.as_deref().unwrap_or("") >= last_expected_trading_day(Utc::now(), &pair).as_str() {
            report.skipped += 1;
            continue;
        }

        match fetch_price_history_from_yahoo(client, &pair, days).await {
            Ok(records) => match open_db(db_path) {
                Ok(conn) => {
                    persist_price_history(&conn, &pair, &records);
                    report.fetched += 1;
                    report.bars_written += records.len();
                }
                Err(err) => {
                    let message = format!("FX rates for {} could not be persisted: {}", pair, err);
                    let _ = insert_event_log(db_path, "error", "fx_sync", "api", Some(&pair), &message);
                    report.errors.push(message);
                }
            },
            Err(err) => {
                let message = format!("FX rate history fetch failed for {}: {}", pair, err);
                let _ = insert_event_log(db_path, "warn", "fx_sync", "api", Some(&pair), &message);
                report.errors.push(message);
            }
        }
    }

    report
}

#[utoipa::path(post, path = "/api/v1/fx/sync", tag = "prices", responses((status = 200, description = "Refresh stored FX rate history")))]
#[post("/api/fx/sync")]
pub(crate) async fn post_fx_sync(db_path: web::Data<PathBuf>) -> impl Responder {
    // `fetch_price_history_from_yahoo` asks for a 2-year range for any days > 365,
    // so this is the widest window available through the shared fetch path —
    // ample, given the earliest holding transaction is 2025-01-02. Extending
    // beyond that would need explicit period1/period2 bounds, as backfill_ohlc
    // uses, because Yahoo downsamples `range=max` to monthly bars.
    HttpResponse::Ok().json(sync_fx_history(db_path.as_ref(), 600).await)
}

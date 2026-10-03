//! Live quotes and stored daily bars: fetching and caching prices, the price
//! history endpoint, and the fallbacks (manual, last close) for symbols the
//! feed no longer quotes.

use super::*;

#[derive(Serialize)]
pub(crate) struct CurrentPrice {
    pub(crate) symbol: String,
    pub(crate) price: Option<f64>,
    pub(crate) change: Option<f64>,
    pub(crate) change_percent: Option<f64>,
    pub(crate) volume: Option<i64>,
    /// Session OHLC for the day `price_date` refers to. `price` is the running
    /// close, so these three complete today's candle while the market is open.
    pub(crate) day_open: Option<f64>,
    pub(crate) day_high: Option<f64>,
    pub(crate) day_low: Option<f64>,
    pub(crate) last_updated: String,
    pub(crate) price_date: Option<String>,
    pub(crate) error: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct CurrentPricesQuery {
    pub(crate) symbols: String,
}

#[utoipa::path(get, path = "/api/v1/cached-prices", tag = "prices", responses((status = 200, description = "Get cached prices")))]
#[get("/api/cached-prices")]
pub(crate) async fn get_cached_prices(db_path: web::Data<PathBuf>, query: web::Query<CurrentPricesQuery>) -> impl Responder {
    let symbols: Vec<String> = query.symbols.split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .map(|s| normalize_symbol(&s))
        .collect();
    match load_cached_prices_with_fallback(&db_path, &symbols) {
        Ok(prices) => HttpResponse::Ok().json(prices),
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "cached_prices_fetch", "api", None, &err);
            err_internal(err)
        }
    }
}

#[utoipa::path(get, path = "/api/v1/current-prices", tag = "prices", responses((status = 200, description = "Get current prices")))]
#[get("/api/current-prices")]
pub(crate) async fn get_current_prices(
    db_path: web::Data<PathBuf>,
    query: web::Query<CurrentPricesQuery>,
) -> impl Responder {
    let symbols: Vec<String> = query
        .symbols
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .map(|s| normalize_symbol(&s))
        .collect();

    if symbols.is_empty() {
        return HttpResponse::Ok().json(Vec::<CurrentPrice>::new());
    }

    match fetch_and_cache_current_prices(&db_path, &symbols, "holdings_prices_updated_at").await {
        Ok(prices) => HttpResponse::Ok().json(prices),
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "price_fetch", "api", None, &err);
            err_internal(err)
        }
    }
}

#[utoipa::path(get, path = "/api/v1/symbol-info", tag = "meta", responses((status = 200, description = "Get symbol info")))]
#[get("/api/symbol-info")]
pub(crate) async fn get_symbol_info(db_path: web::Data<PathBuf>) -> impl Responder {
    let conn = match open_db(db_path.as_ref()) {
        Ok(c) => c,
        Err(err) => return err_internal(err.to_string()),
    };
    let mut stmt = match conn.prepare(
        "SELECT symbol, instrument_type, long_name, currency FROM symbol_info ORDER BY symbol",
    ) {
        Ok(s) => s,
        Err(err) => return err_internal(err.to_string()),
    };
    let rows = stmt.query_map([], |row| {
        Ok(serde_json::json!({
            "symbol": row.get::<_, String>(0)?,
            "instrument_type": row.get::<_, Option<String>>(1)?,
            "long_name": row.get::<_, Option<String>>(2)?,
            "currency": row.get::<_, Option<String>>(3)?,
        }))
    });
    match rows {
        Ok(mapped) => match collect_rows(&db_path, "symbol_info_fetch", mapped) {
            Ok(items) => HttpResponse::Ok().json(items),
            Err(response) => response,
        },
        Err(err) => err_internal(err.to_string()),
    }
}

#[derive(Deserialize)]
pub(crate) struct PriceHistoryQuery {
    pub(crate) symbol: String,
    pub(crate) days: Option<i64>,
    /// Comma-separated SMA periods (e.g. "20,50,150"). When present the
    /// response is `{ points, smas }` instead of a bare array.
    pub(crate) smas: Option<String>,
    /// Inject the cached live price as/over the latest point (server-side
    /// equivalent of the chart's live-point injection).
    pub(crate) include_live: Option<bool>,
}

#[derive(Serialize)]
pub(crate) struct PriceHistoryPoint {
    pub(crate) date: String,
    /// OHLC is absent for rows written before OHLC ingest existed, and for
    /// any bar Yahoo returns without it — clients must tolerate nulls.
    pub(crate) open: Option<f64>,
    pub(crate) high: Option<f64>,
    pub(crate) low: Option<f64>,
    pub(crate) close: Option<f64>,
    pub(crate) volume: Option<i64>,
}

#[utoipa::path(get, path = "/api/v1/price-history", tag = "prices", responses((status = 200, description = "Get price history")))]
#[get("/api/price-history")]
pub(crate) async fn get_price_history(
    db_path: web::Data<PathBuf>,
    query: web::Query<PriceHistoryQuery>,
) -> impl Responder {
    let symbol = normalize_symbol(&query.symbol);
    // Clamp: a negative LIMIT in SQLite means "no limit"
    let days = query.days.unwrap_or(300).clamp(1, 2000);
    let mut history = match fetch_price_history(&db_path, &symbol, days).await {
        Ok(history) => history,
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "price_history_fetch", "api", Some(&symbol), &err);
            return err_internal(err);
        }
    };

    // Plain array response when no annotation was requested (back-compat)
    let Some(smas_param) = query.smas.as_deref() else {
        return HttpResponse::Ok().json(history);
    };

    if query.include_live == Some(true) {
        // Replace or append the latest point with the cached live price so
        // every client renders today's bar consistently.
        struct LiveBar {
            price: Option<f64>,
            price_date: Option<String>,
            volume: Option<i64>,
            open: Option<f64>,
            high: Option<f64>,
            low: Option<f64>,
        }
        let live: Option<LiveBar> = open_db(db_path.as_ref()).ok().and_then(|conn| {
            conn.query_row(
                "SELECT price, price_date, volume, day_open, day_high, day_low FROM cached_current_prices WHERE symbol = ?1",
                params![symbol],
                |row| {
                    Ok(LiveBar {
                        price: row.get(0)?,
                        price_date: row.get(1)?,
                        volume: row.get(2)?,
                        open: row.get(3)?,
                        high: row.get(4)?,
                        low: row.get(5)?,
                    })
                },
            )
            .optional()
            .ok()
            .flatten()
        });
        if let Some(bar) = live
            && let Some(price) = bar.price
        {
            let date = bar.price_date.clone().unwrap_or_else(today_local_str);
            match history.last() {
                None => history.push(PriceHistoryPoint {
                    date,
                    open: bar.open,
                    high: bar.high,
                    low: bar.low,
                    close: Some(price),
                    volume: bar.volume,
                }),
                Some(last) if date == last.date => {
                    // Live session values win, but keep whatever the stored bar
                    // already had so a partial live quote can't blank the candle.
                    let (stored_open, stored_high, stored_low, stored_volume) =
                        (last.open, last.high, last.low, last.volume);
                    let n = history.len();
                    history[n - 1] = PriceHistoryPoint {
                        date,
                        open: bar.open.or(stored_open),
                        high: bar.high.or(stored_high),
                        low: bar.low.or(stored_low),
                        close: Some(price),
                        volume: bar.volume.or(stored_volume),
                    };
                }
                Some(last) if date > last.date.clone() => {
                    history.push(PriceHistoryPoint {
                        date,
                        open: bar.open,
                        high: bar.high,
                        low: bar.low,
                        close: Some(price),
                        volume: bar.volume,
                    });
                }
                _ => {}
            }
        }
    }

    let points = indicator_points(&history);
    let mut smas = serde_json::Map::new();
    for period in smas_param.split(',').filter_map(|s| s.trim().parse::<usize>().ok()).filter(|p| *p > 0 && *p <= 500) {
        smas.insert(period.to_string(), serde_json::json!(stocks::indicators::calculate_sma(&points, period)));
    }

    HttpResponse::Ok().json(serde_json::json!({ "points": history, "smas": smas }))
}

/// Symbols the market no longer trades, keyed by symbol, to the last date each
/// one traded.
///
/// A delisted ticker is not a transient fetch failure: Yahoo answers 404 for it
/// on every run, forever. Left unmarked it burns a request per refresh and
/// writes an error to the event log each time — JLG.AX and CCLD.AX between them
/// account for 639 such rows — which buries the failures that do mean something.
pub(crate) fn dead_symbols(conn: &Connection) -> HashMap<String, String> {
    let sql = format!("SELECT key, value FROM app_config WHERE key LIKE '{DEAD_SYMBOL_PREFIX}%'");
    let mut stmt = match conn.prepare(&sql) {
        Ok(s) => s,
        Err(_) => return HashMap::new(),
    };
    stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
        .map(|rows| {
            rows.flatten()
                .filter_map(|(key, value)| {
                    let symbol = key.strip_prefix(DEAD_SYMBOL_PREFIX)?.to_string();
                    // An empty value is how the UI clears the mark, so it must
                    // not read back as a dead symbol with a blank date.
                    let date = value.trim();
                    if date.is_empty() { None } else { Some((symbol, date.to_string())) }
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Manually entered prices, keyed by symbol, from `app_config`.
///
/// Set for holdings the market no longer prices — a delisted ticker keeps its
/// last real close forever otherwise, which would quietly misstate the
/// portfolio's value from the day it stopped trading.
pub(crate) fn manual_prices(conn: &Connection) -> HashMap<String, f64> {
    let mut stmt = match conn.prepare("SELECT key, value FROM app_config WHERE key LIKE 'manual_price_%'") {
        Ok(s) => s,
        Err(_) => return HashMap::new(),
    };
    stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
        .map(|rows| {
            rows.flatten()
                .filter_map(|(key, value)| {
                    let symbol = key.strip_prefix("manual_price_")?.to_string();
                    let price = value.trim().parse::<f64>().ok().filter(|p| *p > 0.0)?;
                    Some((symbol, price))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Cached live quotes, keyed by symbol, in the symbol's own currency.
///
/// The daily bars stop at the last close the fetcher stored, which is often
/// yesterday. Past that point the portfolio is worth what it is quoted at now —
/// the same figure the Holdings screen and the Dashboard's Stock Value card
/// report — so the value chart has to end on it too, or its last point
/// disagrees with the headline beside it.
pub(crate) fn cached_quote_prices(conn: &Connection) -> HashMap<String, f64> {
    let mut stmt = match conn.prepare("SELECT symbol, price FROM cached_current_prices WHERE price > 0") {
        Ok(s) => s,
        Err(_) => return HashMap::new(),
    };
    stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?)))
        .map(|rows| rows.flatten().collect())
        .unwrap_or_default()
}

/// The most recent stored bar for each symbol, as `(close, date)`.
///
/// Used when the live feed has nothing to say — a delisted symbol keeps its
/// last traded price rather than falling out of the portfolio entirely.
pub(crate) fn latest_closes(db_path: &PathBuf, symbols: &[String]) -> HashMap<String, (f64, String)> {
    let mut out = HashMap::new();
    let Ok(conn) = open_db(db_path) else { return out };
    let Ok(mut stmt) = conn.prepare(
        "SELECT close, date FROM prices
          WHERE symbol = ?1 AND close IS NOT NULL AND close > 0
          ORDER BY date DESC LIMIT 1",
    ) else {
        return out;
    };
    for symbol in symbols {
        if let Ok(row) = stmt.query_row(params![symbol], |r| Ok((r.get::<_, f64>(0)?, r.get::<_, String>(1)?))) {
            out.insert(symbol.clone(), row);
        }
    }
    out
}

pub(crate) fn load_close_series(conn: &Connection, symbol: &str) -> Vec<(String, f64)> {
    let mut stmt = match conn
        .prepare("SELECT date, close FROM prices WHERE symbol = ?1 AND close IS NOT NULL ORDER BY date")
    {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    stmt.query_map(params![symbol], |row| Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?)))
        .map(|rows| rows.flatten().collect())
        .unwrap_or_default()
}

pub(crate) fn cache_current_price(conn: &Connection, price: &CurrentPrice) -> Result<(), String> {
    // Backstop for any caller that hasn't checked: overwriting a good cached
    // price with a delisted symbol's zero is what makes a holding vanish.
    if !is_usable_quote(price.price) {
        log_event_on_conn(
            conn,
            "warn",
            "price_cache",
            Some(&price.symbol),
            &format!("Refusing to cache non-positive price {:?} — keeping the last good quote", price.price),
        );
        return Ok(());
    }
    conn.execute(
        "INSERT OR REPLACE INTO cached_current_prices (symbol, price, change, change_percent, volume, day_open, day_high, day_low, last_updated, price_date)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            price.symbol, price.price, price.change, price.change_percent,
            price.volume, price.day_open, price.day_high, price.day_low,
            price.last_updated, price.price_date,
        ],
    ).map_err(|err| err.to_string())?;
    Ok(())
}

pub(crate) fn load_cached_prices(db_path: &PathBuf, symbols: &[String]) -> Result<Vec<CurrentPrice>, String> {
    if symbols.is_empty() { return Ok(Vec::new()); }
    let conn = open_db(db_path).map_err(|err| err.to_string())?;
    let placeholders: Vec<String> = symbols.iter().enumerate().map(|(i, _)| format!("?{}", i + 1)).collect();
    let sql = format!(
        "SELECT symbol, price, change, change_percent, volume, last_updated, price_date, day_open, day_high, day_low FROM cached_current_prices WHERE symbol IN ({})",
        placeholders.join(",")
    );
    let mut stmt = conn.prepare(&sql).map_err(|err| err.to_string())?;
    let params: Vec<&dyn rusqlite::types::ToSql> = symbols.iter().map(|s| s as &dyn rusqlite::types::ToSql).collect();
    let rows = stmt.query_map(params.as_slice(), |row| {
        Ok(CurrentPrice {
            symbol: row.get(0)?,
            price: row.get(1)?,
            change: row.get(2)?,
            change_percent: row.get(3)?,
            volume: row.get(4)?,
            last_updated: row.get(5)?,
            price_date: row.get(6)?,
            day_open: row.get(7)?,
            day_high: row.get(8)?,
            day_low: row.get(9)?,
            error: None,
        })
    }).map_err(|err| err.to_string())?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|err| err.to_string())
}

pub(crate) fn load_cached_prices_with_fallback(db_path: &PathBuf, symbols: &[String]) -> Result<Vec<CurrentPrice>, String> {
    let cached = load_cached_prices(db_path, symbols)?;
    let cached_set: std::collections::HashSet<String> = cached.iter().map(|p| p.symbol.clone()).collect();
    let mut results = cached;
    for sym in symbols {
        if !cached_set.contains(sym.as_str()) {
            results.push(CurrentPrice {
                symbol: sym.clone(),
                price: None,
                change: None,
                change_percent: None,
                volume: None,
                day_open: None,
                day_high: None,
                day_low: None,
                last_updated: String::new(),
                price_date: None,
                error: None,
            });
        }
    }
    Ok(results)
}

/// Record a warn/error against the event_log using an already-open connection.
pub(crate) fn log_event_on_conn(conn: &Connection, level: &str, event_type: &str, symbol: Option<&str>, details: &str) {
    let _ = stocks::db::log_event(conn, level, event_type, "api", symbol, details);
}

/// Whether a fetched quote is a real price rather than an absence of one.
///
/// Yahoo answers a delisted or unknown symbol with `regularMarketPrice: 0.0`
/// rather than null, so an `is_some()` check lets it through as though it were
/// a quote. Zero is never a price for an equity — it means the feed has nothing
/// — and treating it as one values the holding at nothing. That is how 3
/// ETPMPM.AX shares came to be marked at $0.00 once the symbol stopped trading,
/// and, worse, how a zero close could overwrite a good historical bar.
///
/// When there is no usable quote the last good price must stand.
pub(crate) fn is_usable_quote(price: Option<f64>) -> bool {
    stocks::prices::is_usable_price(price)
}

pub(crate) fn persist_price_to_history(conn: &Connection, symbol: &str, price: &CurrentPrice, fetched_at: &str) {
    if !is_usable_quote(price.price) {
        log_event_on_conn(
            conn,
            "warn",
            "price_persist",
            Some(symbol),
            &format!("Refusing to write non-positive close {:?} — keeping the last good bar", price.price),
        );
        return;
    }
    if let Some(date) = &price.price_date
        && let Err(err) = stocks::prices::upsert_daily_bar(
            conn,
            symbol,
            &stocks::prices::DailyBar {
                date,
                open: price.day_open,
                high: price.day_high,
                low: price.day_low,
                close: price.price,
                volume: price.volume,
            },
            fetched_at,
        ) {
            log_event_on_conn(conn, "warn", "price_persist", Some(symbol), &format!("Failed to persist current price: {}", err));
        }
}

/// Fetch live prices for `symbols` from Yahoo in small concurrent batches,
/// falling back to the latest stored close on failure, then persist results
/// to the price cache/history and stamp `updated_at_key` in app_config.
pub(crate) async fn fetch_and_cache_current_prices(
    db_path: &PathBuf,
    symbols: &[String],
    updated_at_key: &str,
) -> Result<Vec<CurrentPrice>, String> {
    let client = http_client();

    // Dedupe while preserving order (a watchlist symbol can be in several lists)
    let mut unique: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for s in symbols {
        if seen.insert(s.clone()) {
            unique.push(s.clone());
        }
    }

    // Drop the symbols marked dead. Every fetch path — watchlist, holdings and
    // sold — funnels through here, so this is the one place that has to know.
    // Their prices still resolve through the manual/last-close chain; what stops
    // is asking Yahoo a question it has already answered 404 to.
    let dead = match open_db(db_path) {
        Ok(c) => dead_symbols(&c),
        Err(err) => {
            let _ = insert_event_log(db_path, "warn", "price_fetch", "api", None, &format!("Could not read delisted markers; fetching every symbol: {}", err));
            HashMap::new()
        }
    };
    if !dead.is_empty() {
        unique.retain(|s| !dead.contains_key(s));
        if unique.is_empty() {
            return Ok(Vec::new());
        }
    }

    // Batches of 5 keep the endpoint responsive without hammering Yahoo
    let mut fetched: HashMap<String, Result<YahooMeta, String>> = HashMap::new();
    for chunk in unique.chunks(5) {
        let mut set = tokio::task::JoinSet::new();
        for sym in chunk {
            let client = client.clone();
            let sym = sym.clone();
            set.spawn(async move {
                let result = fetch_current_price(&client, &sym).await;
                (sym, result)
            });
        }
        while let Some(joined) = set.join_next().await {
            if let Ok((sym, result)) = joined {
                fetched.insert(sym, result);
            }
        }
    }

    let now = Utc::now().to_rfc3339();
    let mut prices = Vec::with_capacity(unique.len());
    for symbol in &unique {
        match fetched.remove(symbol) {
            Some(Ok(meta)) => {
                if (meta.instrument_type.is_some() || meta.long_name.is_some() || meta.currency.is_some())
                    && let Err(err) = store_symbol_info(db_path, symbol, meta.instrument_type.as_deref(), meta.long_name.as_deref(), meta.currency.as_deref())
                {
                    let _ = insert_event_log(db_path, "warn", "symbol_info", "api", Some(symbol), &format!("Name and currency not stored: {}", err));
                }
                let change = meta.regular_market_change.or_else(|| {
                    meta.regular_market_price.zip(meta.chart_previous_close).map(|(p, prev)| p - prev)
                });
                let change_percent = meta.regular_market_change_percent.or_else(|| {
                    change.zip(meta.chart_previous_close).and_then(|(ch, prev)| {
                        if prev != 0.0 { Some(ch / prev * 100.0) } else { None }
                    })
                });
                let price_date = meta.regular_market_time.and_then(|ts| {
                    yahoo_local_date(ts, meta.gmtoffset).map(|d| d.format("%Y-%m-%d").to_string())
                });
                prices.push(CurrentPrice {
                    symbol: symbol.clone(),
                    price: meta.regular_market_price,
                    change,
                    change_percent,
                    volume: meta.regular_market_volume,
                    day_open: meta.day_open,
                    day_high: meta.regular_market_day_high,
                    day_low: meta.regular_market_day_low,
                    last_updated: now.clone(),
                    price_date,
                    error: None,
                });
            }
            other => {
                let err = match other {
                    Some(Err(e)) => e,
                    _ => "fetch task did not complete".to_string(),
                };
                let fallback_price = fetch_latest_close_price(db_path, symbol).unwrap_or(None);
                let error_message = if let Some(price) = fallback_price {
                    format!("Yahoo fetch failed for {}. Returning latest close price {}. Error: {}", symbol, price, err)
                } else {
                    format!("Yahoo fetch failed for {}: {}", symbol, err)
                };
                let _ = insert_event_log(db_path, "error", "price_fetch", "api", Some(symbol), &error_message);
                prices.push(CurrentPrice {
                    symbol: symbol.clone(),
                    price: fallback_price,
                    change: None,
                    change_percent: None,
                    volume: None,
                    day_open: None,
                    day_high: None,
                    day_low: None,
                    last_updated: now.clone(),
                    price_date: None,
                    error: Some(error_message),
                });
            }
        }
    }

    // Persist fetched prices to cache and history
    match open_db(db_path) {
        Ok(conn) => {
            for p in &prices {
                // A delisted symbol comes back as 0.0, not null. Skipping it
                // leaves the last good price in place rather than marking the
                // holding worthless.
                if is_usable_quote(p.price) {
                    if let Err(err) = cache_current_price(&conn, p) {
                        let _ = insert_event_log(db_path, "error", "price_cache", "api", Some(&p.symbol), &format!("Failed to cache price: {}", err));
                    }
                    persist_price_to_history(&conn, &p.symbol, p, &now);
                } else {
                    let _ = insert_event_log(db_path, "warn", "price_fetch", "api", Some(&p.symbol),
                        &format!("No usable quote (got {:?}) — symbol may be delisted; last known price retained", p.price));
                }
            }
            if let Err(err) = conn.execute(
                "INSERT OR REPLACE INTO app_config (key, value) VALUES (?1, ?2)",
                params![updated_at_key, now],
            ) {
                let _ = insert_event_log(db_path, "error", "price_cache", "api", None, &format!("Failed to update {}: {}", updated_at_key, err));
            }
        }
        Err(err) => {
            let _ = insert_event_log(db_path, "error", "price_cache", "api", None, &format!("Failed to open DB for caching prices: {}", err));
        }
    }

    Ok(prices)
}

/// Most recent date (YYYY-MM-DD, UTC) for which a daily close bar is
/// expected to exist for `symbol`. Weekends map back to Friday. The Monday
/// cutoff is market-aware: ASX (.AX) closes ~05:00–06:00 UTC (16:00
/// Sydney), so Monday's close is expected from 07:00 UTC — hours before US
/// markets even open; US symbols keep a ~10:00 UTC cutoff, with Friday the
/// last expected bar before it. Without the split, Monday-afternoon Sydney
/// sessions would be treated as up to date on Friday's data.
pub(crate) fn last_expected_trading_day(now: chrono::DateTime<Utc>, symbol: &str) -> String {
    let monday_cutoff_hour = if symbol.to_uppercase().ends_with(".AX") { 7 } else { 10 };
    let days_back = match now.weekday() {
        chrono::Weekday::Sat => 1,
        chrono::Weekday::Sun => 2,
        chrono::Weekday::Mon => {
            // Before the cutoff, Friday is still the last expected bar
            if now.hour() < monday_cutoff_hour { 3 } else { 0 }
        }
        _ => 0,
    };
    (now - chrono::Duration::days(days_back)).format("%Y-%m-%d").to_string()
}

pub(crate) fn fetch_latest_close_price(db_path: &PathBuf, symbol: &str) -> Result<Option<f64>, String> {
    let conn = open_db(db_path).map_err(|err| err.to_string())?;
    let mut stmt = conn
        .prepare(
            "SELECT close FROM prices
             WHERE symbol = ?1 AND close IS NOT NULL
             ORDER BY date DESC
             LIMIT 1",
        )
        .map_err(|err| err.to_string())?;

    let result: Option<f64> = stmt
        .query_row(params![symbol], |row| row.get(0))
        .optional()
        .map_err(|err| err.to_string())?;
    Ok(result)
}

pub(crate) fn store_symbol_info(db_path: &PathBuf, symbol: &str, instrument_type: Option<&str>, long_name: Option<&str>, currency: Option<&str>) -> Result<(), String> {
    let conn = open_db(db_path).map_err(|e| e.to_string())?;
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO symbol_info (symbol, instrument_type, long_name, currency, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(symbol) DO UPDATE SET
           instrument_type = COALESCE(?2, instrument_type),
           long_name = COALESCE(?3, long_name),
           currency = COALESCE(?4, currency),
           updated_at = ?5",
        params![symbol, instrument_type, long_name, currency, now],
    ).map_err(|e| e.to_string())?;
    Ok(())
}

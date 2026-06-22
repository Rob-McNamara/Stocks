use actix_cors::Cors;
use actix_web::{delete, get, post, put, web, App, HttpResponse, HttpServer, Responder};
use chrono::{Datelike, NaiveDate, TimeZone, Timelike, Utc};
use reqwest::Client;
use rusqlite::{params, types::Type, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, env, path::PathBuf};

#[derive(Serialize)]
struct EventLogEntry {
    id: i64,
    timestamp: String,
    level: String,
    source: String,
    event_type: String,
    symbol: Option<String>,
    details: Option<String>,
}

#[derive(Deserialize)]
struct EventQuery {
    page: Option<u32>,
    size: Option<u32>,
    level: Option<String>,
    source: Option<String>,
    event_type: Option<String>,
    symbol: Option<String>,
}

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
}

#[derive(Serialize)]
struct WatchlistSymbol {
    id: i64,
    symbol: String,
    list_name: String,
    #[serde(rename = "added_at")]
    updated_at: String,
    notes: Option<String>,
    custom_fields: std::collections::HashMap<String, String>,
}

#[derive(Deserialize)]
struct AddWatchlistSymbol {
    symbol: String,
    list_name: Option<String>,
    notes: Option<String>,
    custom_fields: Option<std::collections::HashMap<String, String>>,
}

#[derive(Deserialize)]
struct UpdateWatchlistSymbol {
    notes: Option<String>,
    custom_fields: Option<std::collections::HashMap<String, String>>,
}

#[derive(Deserialize)]
struct WatchlistQuery {
    list: Option<String>,
}

#[derive(Serialize)]
struct ConfigItem {
    key: String,
    value: String,
}

#[derive(Deserialize)]
struct UpdateConfig {
    key: String,
    value: String,
}

#[derive(Serialize)]
struct CurrentPrice {
    symbol: String,
    price: Option<f64>,
    change: Option<f64>,
    change_percent: Option<f64>,
    volume: Option<i64>,
    last_updated: String,
    price_date: Option<String>,
    error: Option<String>,
}

#[derive(Serialize, Clone)]
struct HoldingTransaction {
    id: i64,
    symbol: String,
    transaction_type: String,
    date: String,
    quantity: Option<f64>,
    price: Option<f64>,
    amount: Option<f64>,
    brokerage: Option<f64>,
    notes: Option<String>,
    created_at: String,
    #[serde(default)]
    dividends_total: f64,
    currency: String,
    original_price: Option<f64>,
    fx_rate: Option<f64>,
}

#[derive(Debug, Clone)]
struct DividendEvent {
    symbol: String,
    ex_date: NaiveDate,
    payment_date: Option<NaiveDate>,
    record_date: Option<NaiveDate>,
    amount: f64,
    fetched_at: String,
}

#[derive(Debug)]
struct DividendPayment {
    symbol: String,
    ex_date: NaiveDate,
    payment_date: Option<NaiveDate>,
    amount_per_share: f64,
    shares_held: f64,
    total_payment: f64,
}

#[derive(Deserialize)]
struct NewHoldingTransaction {
    symbol: String,
    transaction_type: String,
    date: String,
    quantity: Option<f64>,
    price: Option<f64>,
    amount: Option<f64>,
    brokerage: Option<f64>,
    notes: Option<String>,
    currency: Option<String>,
    original_price: Option<f64>,
    fx_rate: Option<f64>,
}

#[get("/api/health")]
async fn health() -> impl Responder {
    HttpResponse::Ok().json(HealthResponse { status: "ok" })
}

#[get("/api/watchlist")]
async fn get_watchlist(db_path: web::Data<PathBuf>, query: web::Query<WatchlistQuery>) -> impl Responder {
    match load_watchlist_symbols(&db_path, query.list.as_deref()) {
        Ok(symbols) => HttpResponse::Ok().json(symbols),
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "watchlist_fetch", "api", None, &err);
            HttpResponse::InternalServerError().body(err)
        }
    }
}

#[get("/api/watchlist/lists")]
async fn get_watchlist_lists(db_path: web::Data<PathBuf>) -> impl Responder {
    match load_watchlist_lists(&db_path) {
        Ok(lists) => HttpResponse::Ok().json(lists),
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "watchlist_fetch", "api", None, &err);
            HttpResponse::InternalServerError().body(err)
        }
    }
}

#[post("/api/watchlist")]
async fn add_watchlist_symbol(
    db_path: web::Data<PathBuf>,
    payload: web::Json<AddWatchlistSymbol>,
) -> impl Responder {
    let symbol = payload.symbol.trim();
    if symbol.is_empty() {
        return HttpResponse::BadRequest().body("Symbol is required");
    }

    let normalized = normalize_symbol(symbol);
    let list_name = payload.list_name.as_deref().unwrap_or("Default");
    let notes = payload.notes.as_deref();
    let custom_fields = payload.custom_fields.as_ref();
    match insert_watchlist_symbol(&db_path, &normalized, list_name, notes, custom_fields) {
        Ok(row) => {
            // Fetch and store symbol info (long name, type, currency) in the background
            let db_path_clone = db_path.get_ref().clone();
            let sym_clone = normalized.clone();
            actix_web::rt::spawn(async move {
                if let Ok(client) = Client::builder()
                    .user_agent("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
                    .build()
                {
                    if let Ok(meta) = fetch_current_price(&client, &sym_clone).await {
                        let _ = store_symbol_info(
                            &db_path_clone,
                            &sym_clone,
                            meta.instrument_type.as_deref(),
                            meta.long_name.as_deref(),
                            meta.currency.as_deref(),
                        );
                    }
                }
            });
            HttpResponse::Ok().json(row)
        }
        Err(err) => HttpResponse::InternalServerError().body(err),
    }
}

#[put("/api/watchlist/{id}")]
async fn update_watchlist_symbol(
    db_path: web::Data<PathBuf>,
    path: web::Path<i64>,
    payload: web::Json<UpdateWatchlistSymbol>,
) -> impl Responder {
    let id = path.into_inner();
    match update_watchlist_symbol_notes(&db_path, id, payload.notes.as_deref(), payload.custom_fields.as_ref()) {
        Ok(row) => HttpResponse::Ok().json(row),
        Err(err) => HttpResponse::InternalServerError().body(err),
    }
}

#[delete("/api/watchlist/{id}")]
async fn delete_watchlist_symbol(
    db_path: web::Data<PathBuf>,
    path: web::Path<i64>,
) -> impl Responder {
    let id = path.into_inner();
    match remove_watchlist_symbol(&db_path, id) {
        Ok(true) => HttpResponse::NoContent().finish(),
        Ok(false) => HttpResponse::NotFound().body("Symbol not found"),
        Err(err) => HttpResponse::InternalServerError().body(err),
    }
}

#[get("/api/config")]
async fn get_config(db_path: web::Data<PathBuf>) -> impl Responder {
    match load_config(&db_path) {
        Ok(config) => HttpResponse::Ok().json(config),
        Err(err) => HttpResponse::InternalServerError().body(err),
    }
}

#[put("/api/config")]
async fn update_config(
    db_path: web::Data<PathBuf>,
    payload: web::Json<UpdateConfig>,
) -> impl Responder {
    let key = payload.key.trim();
    let value = payload.value.trim();
    if key.is_empty() {
        return HttpResponse::BadRequest().body("Config key is required");
    }

    match upsert_config(&db_path, key, value) {
        Ok(()) => {
            let _ = insert_event_log(&db_path, "info", "config_update", "api", Some(key), &format!("Updated config {}", key));
            HttpResponse::NoContent().finish()
        }
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "config_update", "api", Some(key), &err);
            HttpResponse::InternalServerError().body(err)
        }
    }
}

#[get("/api/watchlist/prices")]
async fn get_watchlist_prices(db_path: web::Data<PathBuf>, query: web::Query<WatchlistQuery>) -> impl Responder {
    match fetch_watchlist_current_prices(&db_path, query.list.as_deref()).await {
        Ok(prices) => HttpResponse::Ok().json(prices),
        Err(err) => HttpResponse::InternalServerError().body(err),
    }
}

#[derive(Deserialize)]
struct CurrentPricesQuery {
    symbols: String,
}

#[get("/api/current-prices")]
async fn get_current_prices(
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

    match fetch_current_prices_for_symbols(&db_path, &symbols).await {
        Ok(prices) => HttpResponse::Ok().json(prices),
        Err(err) => HttpResponse::InternalServerError().body(err),
    }
}

#[get("/api/holdings")]
async fn get_holdings(db_path: web::Data<PathBuf>) -> impl Responder {
    match fetch_holdings(&db_path) {
        Ok(history) => HttpResponse::Ok().json(history),
        Err(err) => HttpResponse::InternalServerError().body(err),
    }
}

#[get("/api/symbol-info")]
async fn get_symbol_info(db_path: web::Data<PathBuf>) -> impl Responder {
    let conn = match Connection::open(db_path.as_ref()) {
        Ok(c) => c,
        Err(err) => return HttpResponse::InternalServerError().body(err.to_string()),
    };
    let mut stmt = match conn.prepare(
        "SELECT symbol, instrument_type, long_name, currency FROM symbol_info ORDER BY symbol",
    ) {
        Ok(s) => s,
        Err(err) => return HttpResponse::InternalServerError().body(err.to_string()),
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
        Ok(mapped) => {
            let items: Vec<_> = mapped.filter_map(|r| r.ok()).collect();
            HttpResponse::Ok().json(items)
        }
        Err(err) => HttpResponse::InternalServerError().body(err.to_string()),
    }
}

#[derive(Deserialize)]
struct FxRateQuery {
    currency: String,
    date: String,
}

#[get("/api/fx-rate")]
async fn get_fx_rate_for_date(query: web::Query<FxRateQuery>) -> impl Responder {
    let pair = format!("{}AUD=X", query.currency.trim().to_uppercase());
    let client = match Client::builder().user_agent("stocks-api/1.0").build() {
        Ok(c) => c,
        Err(err) => return HttpResponse::InternalServerError().body(err.to_string()),
    };
    let target_date = match NaiveDate::parse_from_str(&query.date, "%Y-%m-%d") {
        Ok(d) => d,
        Err(_) => return HttpResponse::BadRequest().body("Invalid date format, use YYYY-MM-DD"),
    };
    // Fetch a week around the target date to cover weekends/holidays
    let period1 = Utc.from_utc_datetime(&(target_date - chrono::Duration::days(7)).and_hms_opt(0, 0, 0).unwrap()).timestamp();
    let period2 = Utc.from_utc_datetime(&(target_date + chrono::Duration::days(2)).and_hms_opt(0, 0, 0).unwrap()).timestamp();
    let url = format!(
        "https://query1.finance.yahoo.com/v8/finance/chart/{}?interval=1d&period1={}&period2={}",
        pair, period1, period2
    );
    let response = match client.get(&url).send().await {
        Ok(r) => r,
        Err(err) => return HttpResponse::InternalServerError().body(err.to_string()),
    };
    let payload: YahooHistoryResponse = match response.json().await {
        Ok(p) => p,
        Err(err) => return HttpResponse::InternalServerError().body(err.to_string()),
    };
    let result = match payload.chart.result.as_ref().and_then(|r| r.first()) {
        Some(r) => r,
        None => return HttpResponse::NotFound().body(format!("No FX data for {}", pair)),
    };
    let timestamps = match result.timestamp.as_ref() {
        Some(t) => t,
        None => return HttpResponse::NotFound().body("No timestamp data"),
    };
    let closes = match result.indicators.quote.first().and_then(|q| q.close.as_ref()) {
        Some(c) => c,
        None => return HttpResponse::NotFound().body("No close data"),
    };
    // Find the entry closest to and on-or-before the target date
    let target_str = target_date.format("%Y-%m-%d").to_string();
    let mut best_date = String::new();
    let mut best_rate: Option<f64> = None;
    for (i, ts) in timestamps.iter().enumerate() {
        let date_str = Utc.timestamp_opt(*ts, 0)
            .single()
            .map(|dt| dt.format("%Y-%m-%d").to_string())
            .unwrap_or_default();
        if date_str <= target_str {
            if let Some(Some(rate)) = closes.get(i) {
                best_date = date_str;
                best_rate = Some(*rate);
            }
        }
    }
    match best_rate {
        Some(rate) => HttpResponse::Ok().json(serde_json::json!({ "rate": rate, "date": best_date })),
        None => HttpResponse::NotFound().body(format!("No FX rate found on or before {}", target_str)),
    }
}

#[get("/api/fx-rates")]
async fn get_fx_rates() -> impl Responder {
    let client = match Client::builder().user_agent("stocks-api/1.0").build() {
        Ok(c) => c,
        Err(err) => return HttpResponse::InternalServerError().body(err.to_string()),
    };
    // USDAUD=X gives USD → AUD rate (how many AUD per 1 USD)
    match fetch_current_price(&client, "USDAUD=X").await {
        Ok(meta) => HttpResponse::Ok().json(serde_json::json!({
            "USDAUD": meta.regular_market_price
        })),
        Err(err) => HttpResponse::InternalServerError().body(err),
    }
}

#[get("/api/dividends")]
async fn get_dividends(db_path: web::Data<PathBuf>) -> impl Responder {
    let conn = match Connection::open(db_path.as_ref()) {
        Ok(c) => c,
        Err(err) => return HttpResponse::InternalServerError().body(err.to_string()),
    };
    let mut stmt = match conn.prepare(
        "SELECT symbol, ex_date, payment_date, amount FROM dividend_events ORDER BY ex_date DESC",
    ) {
        Ok(s) => s,
        Err(err) => return HttpResponse::InternalServerError().body(err.to_string()),
    };
    let rows = stmt.query_map([], |row| {
        Ok(serde_json::json!({
            "symbol": row.get::<_, String>(0)?,
            "ex_date": row.get::<_, String>(1)?,
            "payment_date": row.get::<_, Option<String>>(2)?,
            "amount": row.get::<_, f64>(3)?,
        }))
    });
    match rows {
        Ok(mapped) => {
            let items: Vec<_> = mapped.filter_map(|r| r.ok()).collect();
            HttpResponse::Ok().json(items)
        }
        Err(err) => HttpResponse::InternalServerError().body(err.to_string()),
    }
}

#[get("/api/events")]
async fn get_events(db_path: web::Data<PathBuf>, query: web::Query<EventQuery>) -> impl Responder {
    match fetch_event_log(&db_path, &query.into_inner()) {
        Ok((items, total)) => HttpResponse::Ok().json(serde_json::json!({"items": items, "total": total})),
        Err(err) => HttpResponse::InternalServerError().body(err),
    }
}

#[post("/api/holdings")]
async fn add_holding_transaction(
    db_path: web::Data<PathBuf>,
    payload: web::Json<NewHoldingTransaction>,
) -> impl Responder {
    let payload = payload.into_inner();
    let symbol = normalize_symbol(&payload.symbol);
    match insert_holding_transaction(&db_path, &symbol, payload) {
        Ok(record) => {
            let _ = insert_event_log(&db_path, "info", "holding_create", "api", Some(&record.symbol), &format!("Created holding id {}", record.id));
            HttpResponse::Ok().json(record)
        }
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "holding_create", "api", Some(&symbol), &err);
            HttpResponse::BadRequest().body(err)
        }
    }
}

#[put("/api/holdings/{id}")]
async fn update_holding_transaction(
    db_path: web::Data<PathBuf>,
    path: web::Path<i64>,
    payload: web::Json<NewHoldingTransaction>,
) -> impl Responder {
    let id = path.into_inner();
    let payload = payload.into_inner();
    let symbol = normalize_symbol(&payload.symbol);

    match modify_holding_transaction(&db_path, id, &symbol, payload) {
        Ok(record) => {
            let _ = insert_event_log(&db_path, "info", "holding_update", "api", Some(&record.symbol), &format!("Updated holding id {}", record.id));
            HttpResponse::Ok().json(record)
        }
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "holding_update", "api", Some(&symbol), &err);
            HttpResponse::BadRequest().body(err)
        }
    }
}

#[delete("/api/holdings/{id}")]
async fn delete_holding_transaction(
    db_path: web::Data<PathBuf>,
    path: web::Path<i64>,
) -> impl Responder {
    let id = path.into_inner();
    match remove_holding_transaction(&db_path, id) {
        Ok(true) => {
            let _ = insert_event_log(&db_path, "info", "holding_delete", "api", None, &format!("Deleted holding id {}", id));
            HttpResponse::NoContent().finish()
        }
        Ok(false) => {
            let _ = insert_event_log(&db_path, "warn", "holding_delete", "api", None, &format!("Delete attempted for missing id {}", id));
            HttpResponse::NotFound().body("Transaction not found")
        }
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "holding_delete", "api", None, &err);
            HttpResponse::InternalServerError().body(err)
        }
    }
}

#[derive(Deserialize)]
struct PriceHistoryQuery {
    symbol: String,
    days: Option<i64>,
}

#[derive(Serialize)]
struct PriceHistoryPoint {
    date: String,
    close: Option<f64>,
    volume: Option<i64>,
}

#[get("/api/price-history")]
async fn get_price_history(
    db_path: web::Data<PathBuf>,
    query: web::Query<PriceHistoryQuery>,
) -> impl Responder {
    let symbol = normalize_symbol(&query.symbol);
    let days = query.days.unwrap_or(300);
    match fetch_price_history(&db_path, &symbol, days).await {
        Ok(history) => HttpResponse::Ok().json(history),
        Err(err) => HttpResponse::InternalServerError().body(err),
    }
}

#[derive(Serialize)]
struct DividendRefreshResult {
    updated: usize,
    errors: Vec<String>,
}

#[derive(Deserialize)]
struct YahooDivChart {
    result: Option<Vec<YahooDivResult>>,
    error: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct YahooDivResponse {
    chart: YahooDivChart,
}

#[derive(Deserialize)]
struct YahooDivResult {
    events: Option<YahooDivEvents>,
}

#[derive(Deserialize)]
struct YahooDivEvents {
    dividends: Option<HashMap<String, YahooDivEntry>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct YahooDivEntry {
    amount: Option<f64>,
    date: Option<i64>,
    ex_date: Option<i64>,
    payment_date: Option<i64>,
    record_date: Option<i64>,
}

async fn fetch_dividend_events_for_symbol(client: &Client, symbol: &str) -> Result<Vec<DividendEvent>, String> {
    let url = format!(
        "https://query1.finance.yahoo.com/v8/finance/chart/{}?interval=1d&range=5y&events=div",
        symbol
    );

    let response = client.get(&url).send().await.map_err(|e| e.to_string())?;
    let response = response.error_for_status().map_err(|e| e.to_string())?;
    let payload: YahooDivResponse = response.json().await.map_err(|e| e.to_string())?;

    let result = payload
        .chart
        .result
        .and_then(|items| items.into_iter().next())
        .ok_or_else(|| {
            if let Some(err) = payload.chart.error {
                format!("No chart result for {}: {}", symbol, err)
            } else {
                format!("No chart result for {}", symbol)
            }
        })?;

    let now = Utc::now().to_rfc3339();
    let mut events = Vec::new();

    if let Some(ev) = result.events {
        if let Some(dividends) = ev.dividends {
            for entry in dividends.values() {
                let amount = entry.amount.unwrap_or(0.0);
                if amount <= 0.0 {
                    continue;
                }
                let ts = entry.ex_date.or(entry.date).ok_or_else(|| {
                    format!("Dividend entry missing date for {}", symbol)
                })?;
                let ex_date = Utc
                    .timestamp_opt(ts, 0)
                    .single()
                    .ok_or_else(|| format!("Invalid timestamp {} for {}", ts, symbol))?
                    .date_naive();
                let payment_date = entry
                    .payment_date
                    .and_then(|t| Utc.timestamp_opt(t, 0).single())
                    .map(|dt| dt.date_naive());
                let record_date = entry
                    .record_date
                    .and_then(|t| Utc.timestamp_opt(t, 0).single())
                    .map(|dt| dt.date_naive());
                events.push(DividendEvent {
                    symbol: symbol.to_string(),
                    ex_date,
                    payment_date,
                    record_date,
                    amount,
                    fetched_at: now.clone(),
                });
            }
        }
    }

    events.sort_by_key(|e| e.ex_date);
    Ok(events)
}

fn store_dividend_events_for_symbol(db_path: &PathBuf, symbol: &str, events: &[DividendEvent]) -> Result<(), String> {
    let conn = Connection::open(db_path).map_err(|e| e.to_string())?;
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
        CREATE INDEX IF NOT EXISTS idx_dividend_events_symbol_date ON dividend_events(symbol, ex_date);",
    ).map_err(|e| e.to_string())?;

    let mut conn = Connection::open(db_path).map_err(|e| e.to_string())?;
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    {
        let mut stmt = tx.prepare(
            "INSERT OR REPLACE INTO dividend_events (symbol, ex_date, payment_date, record_date, amount, fetched_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        ).map_err(|e| e.to_string())?;
        for event in events {
            stmt.execute(params![
                symbol,
                event.ex_date.format("%Y-%m-%d").to_string(),
                event.payment_date.map(|d| d.format("%Y-%m-%d").to_string()),
                event.record_date.map(|d| d.format("%Y-%m-%d").to_string()),
                event.amount,
                event.fetched_at,
            ]).map_err(|e| e.to_string())?;
        }
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok(())
}

fn load_holding_symbols(db_path: &PathBuf) -> Result<Vec<String>, String> {
    let conn = Connection::open(db_path).map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare(
            "SELECT symbol, SUM(CASE WHEN transaction_type='purchase' THEN quantity ELSE -quantity END) as net_qty
             FROM holdings_transactions
             WHERE transaction_type IN ('purchase', 'sale')
             GROUP BY symbol
             HAVING net_qty > 0
             ORDER BY symbol",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    Ok(rows)
}

fn load_sold_symbols(db_path: &PathBuf) -> Result<Vec<String>, String> {
    let conn = Connection::open(db_path).map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare(
            "SELECT symbol
             FROM holdings_transactions
             WHERE transaction_type IN ('purchase', 'sale')
             GROUP BY symbol
             HAVING SUM(CASE WHEN transaction_type='purchase' THEN quantity ELSE -quantity END) <= 0
             ORDER BY symbol",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    Ok(rows)
}

#[post("/api/dividends/refresh")]
async fn refresh_dividends(db_path: web::Data<PathBuf>) -> impl Responder {
    let symbols = match load_holding_symbols(&db_path) {
        Ok(s) => s,
        Err(err) => return HttpResponse::InternalServerError().body(err),
    };

    if symbols.is_empty() {
        return HttpResponse::Ok().json(DividendRefreshResult { updated: 0, errors: vec![] });
    }

    let client = match Client::builder().user_agent("stocks-api/1.0").build() {
        Ok(c) => c,
        Err(err) => return HttpResponse::InternalServerError().body(err.to_string()),
    };

    let mut updated = 0;
    let mut errors = Vec::new();

    for symbol in &symbols {
        match fetch_dividend_events_for_symbol(&client, symbol).await {
            Ok(events) => {
                let count = events.len();
                match store_dividend_events_for_symbol(&db_path, symbol, &events) {
                    Ok(()) => {
                        let details = format!("Stored {} dividend events", count);
                        let _ = insert_event_log(&db_path, "info", "dividend_fetch", "api", Some(symbol), &details);
                        updated += 1;
                    }
                    Err(err) => {
                        let _ = insert_event_log(&db_path, "error", "dividend_fetch", "api", Some(symbol), &err);
                        errors.push(format!("{}: {}", symbol, err));
                    }
                }
            }
            Err(err) => {
                let _ = insert_event_log(&db_path, "error", "dividend_fetch", "api", Some(symbol), &err);
                errors.push(format!("{}: {}", symbol, err));
            }
        }
    }

    HttpResponse::Ok().json(DividendRefreshResult { updated, errors })
}

#[post("/api/dividends/refresh-sold")]
async fn refresh_sold_dividends(db_path: web::Data<PathBuf>) -> impl Responder {
    let symbols = match load_sold_symbols(&db_path) {
        Ok(s) => s,
        Err(err) => return HttpResponse::InternalServerError().body(err),
    };

    if symbols.is_empty() {
        return HttpResponse::Ok().json(DividendRefreshResult { updated: 0, errors: vec![] });
    }

    let client = match Client::builder().user_agent("stocks-api/1.0").build() {
        Ok(c) => c,
        Err(err) => return HttpResponse::InternalServerError().body(err.to_string()),
    };

    let mut updated = 0;
    let mut errors = Vec::new();

    for symbol in &symbols {
        match fetch_dividend_events_for_symbol(&client, symbol).await {
            Ok(events) => {
                let count = events.len();
                match store_dividend_events_for_symbol(&db_path, symbol, &events) {
                    Ok(()) => {
                        let details = format!("Stored {} dividend events", count);
                        let _ = insert_event_log(&db_path, "info", "dividend_fetch", "api", Some(symbol), &details);
                        updated += 1;
                    }
                    Err(err) => {
                        let _ = insert_event_log(&db_path, "error", "dividend_fetch", "api", Some(symbol), &err);
                        errors.push(format!("{}: {}", symbol, err));
                    }
                }
            }
            Err(err) => {
                let _ = insert_event_log(&db_path, "error", "dividend_fetch", "api", Some(symbol), &err);
                errors.push(format!("{}: {}", symbol, err));
            }
        }
    }

    HttpResponse::Ok().json(DividendRefreshResult { updated, errors })
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    let database_path = env::var("DATABASE_PATH").unwrap_or_else(|_| "stocks.db".to_string());
    let db_path = PathBuf::from(database_path);
    init_db(&db_path).map_err(|err| {
        eprintln!("Failed to initialize database: {err}");
        std::io::Error::new(std::io::ErrorKind::Other, err)
    })?;

    let host = env::var("API_HOST").unwrap_or_else(|_| "0.0.0.0".to_string());
    let port = env::var("API_PORT").ok().and_then(|value| value.parse::<u16>().ok()).unwrap_or(3001);
    let bind = format!("{host}:{port}");

    println!("Starting stock API server at http://{bind}");

    HttpServer::new(move || {
        App::new()
            .wrap(Cors::permissive())
            .app_data(web::Data::new(db_path.clone()))
            .service(health)
            .service(get_watchlist)
            .service(get_watchlist_lists)
            .service(add_watchlist_symbol)
            .service(update_watchlist_symbol)
            .service(delete_watchlist_symbol)
            .service(get_config)
            .service(update_config)
            .service(get_watchlist_prices)
            .service(get_current_prices)
            .service(get_holdings)
            .service(add_holding_transaction)
            .service(update_holding_transaction)
            .service(delete_holding_transaction)
            .service(get_price_history)
            .service(get_symbol_info)
            .service(get_fx_rate_for_date)
            .service(get_fx_rates)
            .service(get_dividends)
            .service(get_events)
            .service(refresh_dividends)
            .service(refresh_sold_dividends)
    })
    .bind(bind)?
    .run()
    .await
}

fn init_db(path: &PathBuf) -> Result<(), String> {
    let conn = Connection::open(path).map_err(|err| err.to_string())?;
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
        CREATE TABLE IF NOT EXISTS holdings_transactions (
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
        CREATE INDEX IF NOT EXISTS idx_holdings_symbol_date ON holdings_transactions(symbol, date);
        CREATE TABLE IF NOT EXISTS app_config (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );",
    )
    .map_err(|err| err.to_string())?;
    // Add event_log table
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS event_log (
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
    )
    .map_err(|err| err.to_string())?;

    // dividend_events table
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
        CREATE INDEX IF NOT EXISTS idx_dividend_events_symbol_date ON dividend_events(symbol, ex_date);",
    )
    .map_err(|err| err.to_string())?;

    // symbol_info table for instrument type and long name
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS symbol_info (
            symbol TEXT PRIMARY KEY,
            instrument_type TEXT,
            long_name TEXT,
            updated_at TEXT NOT NULL
        );",
    )
    .map_err(|err| err.to_string())?;

    // watchlist_symbol_fields: per-symbol values for user-defined custom fields
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS watchlist_symbol_fields (
            symbol TEXT NOT NULL,
            field_key TEXT NOT NULL,
            value TEXT NOT NULL,
            PRIMARY KEY (symbol, field_key)
        );",
    )
    .map_err(|err| err.to_string())?;

    // Migrate: add columns if they don't exist
    add_column_if_missing(&conn, "holdings_transactions", "brokerage", "REAL")?;
    add_column_if_missing(&conn, "holdings_transactions", "currency", "TEXT NOT NULL DEFAULT 'AUD'")?;
    add_column_if_missing(&conn, "holdings_transactions", "original_price", "REAL")?;
    add_column_if_missing(&conn, "holdings_transactions", "fx_rate", "REAL")?;
    add_column_if_missing(&conn, "symbol_info", "currency", "TEXT")?;

    // Migrate watchlist_symbols to the normalised two-table design:
    //   watchlist_symbols     — one row per symbol (holds notes)
    //   watchlist_memberships — one row per symbol/list pair
    let cols: Vec<String> = {
        let mut stmt = conn.prepare("PRAGMA table_info(watchlist_symbols)").map_err(|e| e.to_string())?;
        stmt.query_map([], |row| row.get::<_, String>(1))
            .map_err(|e| e.to_string())?
            .filter_map(|r| r.ok())
            .collect()
    };

    // Step 1: old single-column table → multi-list table (legacy migration)
    if !cols.contains(&"list_name".to_string()) {
        conn.execute_batch(
            "ALTER TABLE watchlist_symbols RENAME TO watchlist_symbols_old;
             CREATE TABLE watchlist_symbols (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 symbol TEXT NOT NULL,
                 list_name TEXT NOT NULL DEFAULT 'Default',
                 updated_at TEXT NOT NULL,
                 UNIQUE(symbol, list_name)
             );
             INSERT INTO watchlist_symbols (symbol, list_name, updated_at)
                 SELECT symbol, 'Default', updated_at FROM watchlist_symbols_old;
             DROP TABLE watchlist_symbols_old;",
        ).map_err(|e| e.to_string())?;
    }

    // Step 2: multi-list table → normalised two-table design
    let has_memberships = conn
        .query_row("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='watchlist_memberships'", [], |row| row.get::<_, i64>(0))
        .unwrap_or(0) > 0;
    if !has_memberships {
        // notes may or may not exist on the old multi-list table; add it if needed before copying
        let _ = conn.execute("ALTER TABLE watchlist_symbols ADD COLUMN notes TEXT", []);
        conn.execute_batch(
            "CREATE TABLE watchlist_memberships (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 symbol TEXT NOT NULL,
                 list_name TEXT NOT NULL DEFAULT 'Default',
                 added_at TEXT NOT NULL,
                 UNIQUE(symbol, list_name)
             );
             CREATE TABLE watchlist_symbols_new (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 symbol TEXT NOT NULL UNIQUE,
                 notes TEXT,
                 updated_at TEXT NOT NULL
             );
             INSERT OR IGNORE INTO watchlist_symbols_new (symbol, notes, updated_at)
                 SELECT DISTINCT symbol, notes, updated_at FROM watchlist_symbols;
             INSERT OR IGNORE INTO watchlist_memberships (symbol, list_name, added_at)
                 SELECT symbol, list_name, updated_at FROM watchlist_symbols;
             DROP TABLE watchlist_symbols;
             ALTER TABLE watchlist_symbols_new RENAME TO watchlist_symbols;",
        ).map_err(|e| e.to_string())?;
    }

    // Step 3: recover from half-completed Step 2 migration — watchlist_memberships exists but
    // watchlist_symbols still has the old multi-list schema (list_name present, notes absent).
    let ws_cols: Vec<String> = {
        let mut stmt = conn.prepare("PRAGMA table_info(watchlist_symbols)").map_err(|e| e.to_string())?;
        stmt.query_map([], |row| row.get::<_, String>(1))
            .map_err(|e| e.to_string())?
            .filter_map(|r| r.ok())
            .collect()
    };
    if has_memberships && ws_cols.contains(&"list_name".to_string()) && !ws_cols.contains(&"notes".to_string()) {
        // The memberships table already has the correct data; just rebuild watchlist_symbols.
        conn.execute_batch(
            "CREATE TABLE watchlist_symbols_new (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 symbol TEXT NOT NULL UNIQUE,
                 notes TEXT,
                 updated_at TEXT NOT NULL
             );
             INSERT OR IGNORE INTO watchlist_symbols_new (symbol, updated_at)
                 SELECT DISTINCT symbol, updated_at FROM watchlist_symbols;
             DROP TABLE watchlist_symbols;
             ALTER TABLE watchlist_symbols_new RENAME TO watchlist_symbols;",
        ).map_err(|e| e.to_string())?;
    }

    // Ensure notes column exists (safety net for any remaining edge cases)
    add_column_if_missing(&conn, "watchlist_symbols", "notes", "TEXT")?;

    Ok(())
}

fn add_column_if_missing(
    conn: &Connection,
    table: &str,
    column: &str,
    col_type: &str,
) -> Result<(), String> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({})", table))
        .map_err(|err| err.to_string())?;

    let has_column = stmt
        .query_map([], |row| Ok(row.get::<_, String>(1)?))
        .map_err(|err| err.to_string())?
        .any(|result| result.ok().as_deref() == Some(column));

    if !has_column {
        conn.execute(
            &format!("ALTER TABLE {} ADD COLUMN {} {}", table, column, col_type),
            [],
        )
        .map_err(|err| err.to_string())?;
    }

    Ok(())
}

fn normalize_symbol(symbol: &str) -> String {
    symbol.trim().to_uppercase()
}

fn load_custom_fields(conn: &Connection, symbol: &str) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    if let Ok(mut stmt) = conn.prepare("SELECT field_key, value FROM watchlist_symbol_fields WHERE symbol = ?1") {
        if let Ok(rows) = stmt.query_map(params![symbol], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))) {
            for row in rows.flatten() { map.insert(row.0, row.1); }
        }
    }
    map
}

fn save_custom_fields(conn: &Connection, symbol: &str, fields: &std::collections::HashMap<String, String>) -> Result<(), String> {
    conn.execute("DELETE FROM watchlist_symbol_fields WHERE symbol = ?1", params![symbol])
        .map_err(|e| e.to_string())?;
    for (key, value) in fields {
        if !value.trim().is_empty() {
            conn.execute(
                "INSERT OR REPLACE INTO watchlist_symbol_fields (symbol, field_key, value) VALUES (?1, ?2, ?3)",
                params![symbol, key, value.trim()],
            ).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

fn load_watchlist_symbols(db_path: &PathBuf, list: Option<&str>) -> Result<Vec<WatchlistSymbol>, String> {
    let conn = Connection::open(db_path).map_err(|err| err.to_string())?;
    let mut rows: Vec<WatchlistSymbol> = if let Some(list_name) = list {
        let mut stmt = conn
            .prepare(
                "SELECT wm.id, ws.symbol, wm.list_name, wm.added_at, ws.notes
                 FROM watchlist_memberships wm
                 JOIN watchlist_symbols ws ON wm.symbol = ws.symbol
                 WHERE wm.list_name = ?1 ORDER BY ws.symbol",
            )
            .map_err(|err| err.to_string())?;
        stmt.query_map(params![list_name], |row| {
            Ok(WatchlistSymbol { id: row.get(0)?, symbol: row.get(1)?, list_name: row.get(2)?, updated_at: row.get(3)?, notes: row.get(4)?, custom_fields: Default::default() })
        })
        .map_err(|err| err.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| err.to_string())?
    } else {
        let mut stmt = conn
            .prepare(
                "SELECT wm.id, ws.symbol, wm.list_name, wm.added_at, ws.notes
                 FROM watchlist_memberships wm
                 JOIN watchlist_symbols ws ON wm.symbol = ws.symbol
                 ORDER BY wm.list_name, ws.symbol",
            )
            .map_err(|err| err.to_string())?;
        stmt.query_map([], |row| {
            Ok(WatchlistSymbol { id: row.get(0)?, symbol: row.get(1)?, list_name: row.get(2)?, updated_at: row.get(3)?, notes: row.get(4)?, custom_fields: Default::default() })
        })
        .map_err(|err| err.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| err.to_string())?
    };
    for row in &mut rows {
        row.custom_fields = load_custom_fields(&conn, &row.symbol);
    }
    Ok(rows)
}

fn load_watchlist_lists(db_path: &PathBuf) -> Result<Vec<String>, String> {
    let conn = Connection::open(db_path).map_err(|err| err.to_string())?;
    let mut stmt = conn
        .prepare("SELECT DISTINCT list_name FROM watchlist_memberships ORDER BY list_name")
        .map_err(|err| err.to_string())?;
    let mut lists: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|err| err.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| err.to_string())?;
    if !lists.contains(&"Default".to_string()) {
        lists.insert(0, "Default".to_string());
    }
    Ok(lists)
}

fn insert_watchlist_symbol(db_path: &PathBuf, symbol: &str, list_name: &str, notes: Option<&str>, custom_fields: Option<&std::collections::HashMap<String, String>>) -> Result<WatchlistSymbol, String> {
    let conn = Connection::open(db_path).map_err(|err| err.to_string())?;
    let now = Utc::now().to_rfc3339();
    if notes.is_some() {
        conn.execute(
            "INSERT INTO watchlist_symbols (symbol, notes, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(symbol) DO UPDATE SET notes = excluded.notes, updated_at = excluded.updated_at",
            params![symbol, notes, now],
        ).map_err(|err| err.to_string())?;
    } else {
        conn.execute(
            "INSERT OR IGNORE INTO watchlist_symbols (symbol, notes, updated_at) VALUES (?1, NULL, ?2)",
            params![symbol, now],
        ).map_err(|err| err.to_string())?;
    }
    if let Some(fields) = custom_fields {
        save_custom_fields(&conn, symbol, fields)?;
    }
    conn.execute(
        "INSERT OR IGNORE INTO watchlist_memberships (symbol, list_name, added_at) VALUES (?1, ?2, ?3)",
        params![symbol, list_name, now],
    ).map_err(|err| err.to_string())?;

    let mut stmt = conn
        .prepare(
            "SELECT wm.id, ws.symbol, wm.list_name, wm.added_at, ws.notes
             FROM watchlist_memberships wm
             JOIN watchlist_symbols ws ON wm.symbol = ws.symbol
             WHERE wm.symbol = ?1 AND wm.list_name = ?2",
        )
        .map_err(|err| err.to_string())?;
    let mut rows = stmt.query_map(params![symbol, list_name], |row| {
        Ok(WatchlistSymbol { id: row.get(0)?, symbol: row.get(1)?, list_name: row.get(2)?, updated_at: row.get(3)?, notes: row.get(4)?, custom_fields: Default::default() })
    }).map_err(|err| err.to_string())?;
    let mut result = rows.next()
        .transpose()
        .map_err(|err| err.to_string())?
        .ok_or_else(|| "Failed to load inserted symbol".to_string())?;
    result.custom_fields = load_custom_fields(&conn, &result.symbol);
    Ok(result)
}

fn update_watchlist_symbol_notes(db_path: &PathBuf, id: i64, notes: Option<&str>, custom_fields: Option<&std::collections::HashMap<String, String>>) -> Result<WatchlistSymbol, String> {
    let conn = Connection::open(db_path).map_err(|err| err.to_string())?;
    let symbol: String = conn
        .query_row("SELECT symbol FROM watchlist_memberships WHERE id = ?1", params![id], |row| row.get(0))
        .map_err(|_| format!("Membership id {} not found", id))?;
    conn.execute(
        "UPDATE watchlist_symbols SET notes = ?1 WHERE symbol = ?2",
        params![notes, symbol],
    ).map_err(|err| err.to_string())?;
    if let Some(fields) = custom_fields {
        save_custom_fields(&conn, &symbol, fields)?;
    }
    let mut stmt = conn
        .prepare(
            "SELECT wm.id, ws.symbol, wm.list_name, wm.added_at, ws.notes
             FROM watchlist_memberships wm
             JOIN watchlist_symbols ws ON wm.symbol = ws.symbol
             WHERE wm.id = ?1",
        )
        .map_err(|err| err.to_string())?;
    let mut rows = stmt.query_map(params![id], |row| {
        Ok(WatchlistSymbol { id: row.get(0)?, symbol: row.get(1)?, list_name: row.get(2)?, updated_at: row.get(3)?, notes: row.get(4)?, custom_fields: Default::default() })
    }).map_err(|err| err.to_string())?;
    let mut result = rows.next()
        .transpose()
        .map_err(|err| err.to_string())?
        .ok_or_else(|| "Symbol not found after update".to_string())?;
    result.custom_fields = load_custom_fields(&conn, &result.symbol);
    Ok(result)
}

fn remove_watchlist_symbol(db_path: &PathBuf, id: i64) -> Result<bool, String> {
    let conn = Connection::open(db_path).map_err(|err| err.to_string())?;
    // Remove membership; if last membership, also remove the symbol row
    let symbol: Option<String> = conn
        .query_row("SELECT symbol FROM watchlist_memberships WHERE id = ?1", params![id], |row| row.get(0))
        .optional()
        .map_err(|err| err.to_string())?;
    let affected = conn
        .execute("DELETE FROM watchlist_memberships WHERE id = ?1", params![id])
        .map_err(|err| err.to_string())?;
    if let Some(sym) = symbol {
        let remaining: i64 = conn
            .query_row("SELECT COUNT(*) FROM watchlist_memberships WHERE symbol = ?1", params![sym], |row| row.get(0))
            .unwrap_or(0);
        if remaining == 0 {
            let _ = conn.execute("DELETE FROM watchlist_symbols WHERE symbol = ?1", params![sym]);
        }
    }
    Ok(affected > 0)
}

fn fetch_holdings(db_path: &PathBuf) -> Result<Vec<HoldingTransaction>, String> {
    let conn = Connection::open(db_path).map_err(|err| err.to_string())?;
    let mut stmt = conn
        .prepare(
            "SELECT id, symbol, transaction_type, date, quantity, price, amount, brokerage, notes, created_at, currency, original_price, fx_rate
             FROM holdings_transactions
             ORDER BY date DESC, id DESC",
        )
        .map_err(|err| err.to_string())?;

    let rows = stmt
        .query_map([], |row| {
            Ok(HoldingTransaction {
                id: row.get(0)?,
                symbol: row.get(1)?,
                transaction_type: row.get(2)?,
                date: row.get(3)?,
                quantity: row.get(4)?,
                price: row.get(5)?,
                amount: row.get(6)?,
                brokerage: row.get(7)?,
                notes: row.get(8)?,
                created_at: row.get(9)?,
                dividends_total: 0.0,
                currency: row.get::<_, Option<String>>(10)?.unwrap_or_else(|| "AUD".to_string()),
                original_price: row.get(11)?,
                fx_rate: row.get(12)?,
            })
        })
        .map_err(|err| err.to_string())?;

    let mut transactions = rows.collect::<Result<Vec<_>, _>>()
        .map_err(|err| err.to_string())?;

    let dividend_totals = calculate_dividend_totals(db_path, &transactions)?;
    for tx in &mut transactions {
        tx.dividends_total = *dividend_totals.get(&tx.symbol).unwrap_or(&0.0);
    }

    Ok(transactions)
}

fn calculate_dividend_totals(db_path: &PathBuf, transactions: &[HoldingTransaction]) -> Result<std::collections::HashMap<String, f64>, String> {
    use std::collections::{HashMap, HashSet};

    let symbols: HashSet<String> = transactions.iter().map(|tx| tx.symbol.clone()).collect();
    if symbols.is_empty() {
        return Ok(HashMap::new());
    }

    let events = load_dividend_events(db_path, &symbols)?;
    let mut totals = HashMap::new();
    for symbol in symbols {
        let symbol_transactions: Vec<HoldingTransaction> = transactions
            .iter()
            .filter(|tx| tx.symbol == symbol)
            .cloned()
            .collect();
        let symbol_events: Vec<DividendEvent> = events
            .iter()
            .filter(|event| event.symbol == symbol)
            .cloned()
            .collect();

        let payments = calculate_dividend_payments(&symbol_transactions, &symbol_events);
        let total_payment = payments.iter().map(|payment| payment.total_payment).sum();
        totals.insert(symbol, total_payment);
    }

    Ok(totals)
}

fn load_dividend_events(db_path: &PathBuf, symbols: &std::collections::HashSet<String>) -> Result<Vec<DividendEvent>, String> {
    let conn = Connection::open(db_path).map_err(|err| err.to_string())?;
    let mut stmt = conn
        .prepare(
            "SELECT symbol, ex_date, payment_date, record_date, amount, fetched_at
             FROM dividend_events
             ORDER BY symbol, ex_date",
        )
        .map_err(|err| err.to_string())?;

    let rows = stmt
        .query_map([], |row| {
            let ex_date_str: String = row.get(1)?;
            let payment_date_str: Option<String> = row.get(2)?;
            let record_date_str: Option<String> = row.get(3)?;
            Ok(DividendEvent {
                symbol: row.get(0)?,
                ex_date: NaiveDate::parse_from_str(&ex_date_str, "%Y-%m-%d")
                    .map_err(|err| rusqlite::Error::FromSqlConversionFailure(1, Type::Text, Box::new(err)))?,
                payment_date: payment_date_str
                    .map(|date| NaiveDate::parse_from_str(&date, "%Y-%m-%d").ok())
                    .flatten(),
                record_date: record_date_str
                    .map(|date| NaiveDate::parse_from_str(&date, "%Y-%m-%d").ok())
                    .flatten(),
                amount: row.get(4)?,
                fetched_at: row.get(5)?,
            })
        })
        .map_err(|err| err.to_string())?;

    let events = rows
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| err.to_string())?
        .into_iter()
        .filter(|event| symbols.contains(&event.symbol))
        .collect();

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
    let mut sorted_transactions = transactions.to_vec();
    sorted_transactions.sort_by(|a, b| a.date.cmp(&b.date).then(a.id.cmp(&b.id)));

    for tx in sorted_transactions {
        if let Ok(tx_date) = NaiveDate::parse_from_str(&tx.date, "%Y-%m-%d") {
            if tx_date > date {
                break;
            }
            match tx.transaction_type.as_str() {
                "purchase" => shares += tx.quantity.unwrap_or(0.0),
                "sale" => shares -= tx.quantity.unwrap_or(0.0),
                _ => {}
            }
        }
    }

    shares.max(0.0)
}

fn insert_holding_transaction(
    db_path: &PathBuf,
    symbol: &str,
    transaction: NewHoldingTransaction,
) -> Result<HoldingTransaction, String> {
    let parsed_date = NaiveDate::parse_from_str(&transaction.date, "%Y-%m-%d")
        .map_err(|_| "Invalid date format. Use YYYY-MM-DD.".to_string())?;

    let tx_type = transaction.transaction_type.as_str();
    match tx_type {
        "purchase" | "sale" => {
            if transaction.quantity.unwrap_or(0.0) <= 0.0 {
                return Err("Quantity must be greater than zero for purchases and sales".to_string());
            }
            if transaction.price.unwrap_or(0.0) <= 0.0 {
                return Err("Price must be greater than zero for purchases and sales".to_string());
            }
        }
        "dividend" => {
            if transaction.amount.unwrap_or(0.0) <= 0.0 {
                return Err("Amount must be greater than zero for dividends".to_string());
            }
        }
        _ => return Err("Transaction type must be purchase, sale, or dividend".to_string()),
    }

    let conn = Connection::open(db_path).map_err(|err| err.to_string())?;
    let created_at = Utc::now().to_rfc3339();
    let currency = transaction.currency.as_deref().unwrap_or("AUD");
    conn.execute(
        "INSERT INTO holdings_transactions (symbol, transaction_type, date, quantity, price, amount, brokerage, notes, created_at, currency, original_price, fx_rate)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            symbol,
            tx_type,
            parsed_date.format("%Y-%m-%d").to_string(),
            transaction.quantity,
            transaction.price,
            transaction.amount,
            transaction.brokerage,
            transaction.notes,
            created_at,
            currency,
            transaction.original_price,
            transaction.fx_rate,
        ],
    )
    .map_err(|err| err.to_string())?;

    let id = conn.last_insert_rowid();
    let mut stmt = conn
        .prepare(
            "SELECT id, symbol, transaction_type, date, quantity, price, amount, brokerage, notes, created_at, currency, original_price, fx_rate
             FROM holdings_transactions
             WHERE id = ?1",
        )
        .map_err(|err| err.to_string())?;

    let mut rows = stmt
        .query_map(params![id], |row| {
            Ok(HoldingTransaction {
                id: row.get(0)?,
                symbol: row.get(1)?,
                transaction_type: row.get(2)?,
                date: row.get(3)?,
                quantity: row.get(4)?,
                price: row.get(5)?,
                amount: row.get(6)?,
                brokerage: row.get(7)?,
                notes: row.get(8)?,
                created_at: row.get(9)?,
                dividends_total: 0.0,
                currency: row.get::<_, Option<String>>(10)?.unwrap_or_else(|| "AUD".to_string()),
                original_price: row.get(11)?,
                fx_rate: row.get(12)?,
            })
        })
        .map_err(|err| err.to_string())?;

    rows.next()
        .transpose()
        .map_err(|err| err.to_string())?
        .ok_or_else(|| "Failed to retrieve holding transaction".to_string())
}

fn modify_holding_transaction(
    db_path: &PathBuf,
    id: i64,
    symbol: &str,
    transaction: NewHoldingTransaction,
) -> Result<HoldingTransaction, String> {
    let parsed_date = NaiveDate::parse_from_str(&transaction.date, "%Y-%m-%d")
        .map_err(|_| "Invalid date format. Use YYYY-MM-DD.".to_string())?;

    let tx_type = transaction.transaction_type.as_str();
    match tx_type {
        "purchase" | "sale" => {
            if transaction.quantity.unwrap_or(0.0) <= 0.0 {
                return Err("Quantity must be greater than zero for purchases and sales".to_string());
            }
            if transaction.price.unwrap_or(0.0) <= 0.0 {
                return Err("Price must be greater than zero for purchases and sales".to_string());
            }
        }
        "dividend" => {
            if transaction.amount.unwrap_or(0.0) <= 0.0 {
                return Err("Amount must be greater than zero for dividends".to_string());
            }
        }
        _ => return Err("Transaction type must be purchase, sale, or dividend".to_string()),
    }

    let conn = Connection::open(db_path).map_err(|err| err.to_string())?;
    let currency = transaction.currency.as_deref().unwrap_or("AUD");
    conn.execute(
        "UPDATE holdings_transactions SET symbol = ?1, transaction_type = ?2, date = ?3, quantity = ?4, price = ?5, amount = ?6, brokerage = ?7, notes = ?8, currency = ?9, original_price = ?10, fx_rate = ?11 WHERE id = ?12",
        params![
            symbol,
            tx_type,
            parsed_date.format("%Y-%m-%d").to_string(),
            transaction.quantity,
            transaction.price,
            transaction.amount,
            transaction.brokerage,
            transaction.notes,
            currency,
            transaction.original_price,
            transaction.fx_rate,
            id,
        ],
    )
    .map_err(|err| err.to_string())?;

    let mut stmt = conn
        .prepare(
            "SELECT id, symbol, transaction_type, date, quantity, price, amount, brokerage, notes, created_at, currency, original_price, fx_rate
             FROM holdings_transactions
             WHERE id = ?1",
        )
        .map_err(|err| err.to_string())?;

    let mut rows = stmt
        .query_map(params![id], |row| {
            Ok(HoldingTransaction {
                id: row.get(0)?,
                symbol: row.get(1)?,
                transaction_type: row.get(2)?,
                date: row.get(3)?,
                quantity: row.get(4)?,
                price: row.get(5)?,
                amount: row.get(6)?,
                brokerage: row.get(7)?,
                notes: row.get(8)?,
                created_at: row.get(9)?,
                dividends_total: 0.0,
                currency: row.get::<_, Option<String>>(10)?.unwrap_or_else(|| "AUD".to_string()),
                original_price: row.get(11)?,
                fx_rate: row.get(12)?,
            })
        })
        .map_err(|err| err.to_string())?;

    rows.next()
        .transpose()
        .map_err(|err| err.to_string())?
        .ok_or_else(|| "Failed to retrieve updated holding transaction".to_string())
}

fn remove_holding_transaction(db_path: &PathBuf, id: i64) -> Result<bool, String> {
    let conn = Connection::open(db_path).map_err(|err| err.to_string())?;
    let affected = conn
        .execute("DELETE FROM holdings_transactions WHERE id = ?1", params![id])
        .map_err(|err| err.to_string())?;
    Ok(affected > 0)
}

fn load_config(db_path: &PathBuf) -> Result<Vec<ConfigItem>, String> {
    let conn = Connection::open(db_path).map_err(|err| err.to_string())?;
    let mut stmt = conn
        .prepare("SELECT key, value FROM app_config ORDER BY key")
        .map_err(|err| err.to_string())?;

    let rows = stmt
        .query_map([], |row| {
            Ok(ConfigItem {
                key: row.get(0)?,
                value: row.get(1)?,
            })
        })
        .map_err(|err| err.to_string())?;

    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|err| err.to_string())
}

fn upsert_config(db_path: &PathBuf, key: &str, value: &str) -> Result<(), String> {
    let conn = Connection::open(db_path).map_err(|err| err.to_string())?;
    conn.execute(
        "INSERT INTO app_config (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )
    .map_err(|err| err.to_string())?;
    Ok(())
}

async fn fetch_watchlist_current_prices(db_path: &PathBuf, list: Option<&str>) -> Result<Vec<CurrentPrice>, String> {
    let symbols = load_watchlist_symbols(db_path, list)?;
    if symbols.is_empty() {
        return Ok(Vec::new());
    }

    let client = Client::builder()
        .user_agent("stocks-api/1.0")
        .build()
        .map_err(|err| err.to_string())?;

    let mut prices = Vec::new();
    let last_updated = Utc::now().to_rfc3339();

    for symbol_data in symbols {
        match fetch_current_price(&client, &symbol_data.symbol).await {
            Ok(price_data) => {
                let change = price_data.regular_market_change.or_else(|| {
                    price_data.regular_market_price.zip(price_data.chart_previous_close).map(|(p, prev)| p - prev)
                });
                let change_percent = price_data.regular_market_change_percent.or_else(|| {
                    change.zip(price_data.chart_previous_close).and_then(|(ch, prev)| {
                        if prev != 0.0 { Some(ch / prev * 100.0) } else { None }
                    })
                });
                let price_date = price_data.regular_market_time.and_then(|ts| {
                    Utc.timestamp_opt(ts, 0).single().map(|dt| dt.format("%Y-%m-%d").to_string())
                });
                prices.push(CurrentPrice {
                    symbol: symbol_data.symbol,
                    price: price_data.regular_market_price,
                    change,
                    change_percent,
                    volume: price_data.regular_market_volume,
                    last_updated: last_updated.clone(),
                    price_date,
                    error: None,
                });
            }
            Err(err) => {
                // Log error but continue with other symbols
                eprintln!("Failed to fetch price for {}: {}", symbol_data.symbol, err);
                let fallback_price = fetch_latest_close_price(db_path, &symbol_data.symbol).unwrap_or(None);
                let error_message = if let Some(price) = fallback_price {
                    format!("Yahoo fetch failed for {}. Returning latest close price {}. Error: {}", symbol_data.symbol, price, err)
                } else {
                    format!("Yahoo fetch failed for {}: {}", symbol_data.symbol, err)
                };
                prices.push(CurrentPrice {
                    symbol: symbol_data.symbol,
                    price: fallback_price,
                    change: None,
                    change_percent: None,
                    volume: None,
                    last_updated: last_updated.clone(),
                    price_date: None,
                    error: Some(error_message),
                });
            }
        }
    }

    Ok(prices)
}

async fn fetch_price_history(db_path: &PathBuf, symbol: &str, days: i64) -> Result<Vec<PriceHistoryPoint>, String> {
    let conn = Connection::open(db_path).map_err(|err| err.to_string())?;
    let mut stmt = conn
        .prepare(
            "SELECT date, close, volume FROM prices
             WHERE symbol = ?1 AND close IS NOT NULL
             ORDER BY date DESC
             LIMIT ?2",
        )
        .map_err(|err| err.to_string())?;

    let rows = stmt
        .query_map(params![symbol, days], |row| {
            Ok(PriceHistoryPoint {
                date: row.get(0)?,
                close: row.get(1)?,
                volume: row.get(2)?,
            })
        })
        .map_err(|err| err.to_string())?;

    let mut history = rows.collect::<Result<Vec<_>, _>>().map_err(|err| err.to_string())?;
    history.reverse();

    // Determine the last weekday (most recent expected trading day) in UTC
    let now = Utc::now();
    let days_back = match now.weekday() {
        chrono::Weekday::Sat => 1,
        chrono::Weekday::Sun => 2,
        chrono::Weekday::Mon => {
            // Before ~10am UTC Monday, Friday is still the last trading day for US stocks
            if now.hour() < 10 { 3 } else { 0 }
        }
        _ => 0,
    };
    let last_trading_day = (now - chrono::Duration::days(days_back)).format("%Y-%m-%d").to_string();

    let last_stored = history.last().map(|h| h.date.as_str()).unwrap_or("").to_string();
    let needs_supplement = last_stored < last_trading_day;

    let client = Client::builder()
        .user_agent("stocks-api/1.0")
        .build()
        .map_err(|err| err.to_string())?;

    if history.is_empty() {
        if let Ok(records) = fetch_price_history_from_yahoo(&client, symbol, days).await {
            return Ok(records);
        }
    } else if needs_supplement {
        // Stored data is behind the last trading day — fetch from Yahoo and append missing records
        if let Ok(yahoo) = fetch_price_history_from_yahoo(&client, symbol, days).await {
            let new_records: Vec<_> = yahoo.into_iter().filter(|r| r.date > last_stored).collect();
            history.extend(new_records);
        }
    }

    Ok(history)
}

fn fetch_latest_close_price(db_path: &PathBuf, symbol: &str) -> Result<Option<f64>, String> {
    let conn = Connection::open(db_path).map_err(|err| err.to_string())?;
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

#[derive(Deserialize)]
struct YahooQuoteResponse {
    chart: YahooChartData,
}

#[derive(Deserialize)]
struct YahooChartData {
    result: Option<Vec<YahooResultData>>,
}

#[derive(Deserialize)]
struct YahooResultData {
    meta: YahooMeta,
    #[serde(default)]
    indicators: Option<YahooHistoryIndicators>,
}

#[allow(non_snake_case)]
#[derive(Deserialize)]
struct YahooMeta {
    #[serde(rename = "regularMarketPrice")]
    regular_market_price: Option<f64>,
    #[serde(rename = "regularMarketChange")]
    regular_market_change: Option<f64>,
    #[serde(rename = "regularMarketChangePercent")]
    regular_market_change_percent: Option<f64>,
    #[serde(rename = "regularMarketVolume")]
    regular_market_volume: Option<i64>,
    #[serde(rename = "chartPreviousClose")]
    chart_previous_close: Option<f64>,
    #[serde(rename = "instrumentType")]
    instrument_type: Option<String>,
    #[serde(rename = "longName")]
    long_name: Option<String>,
    currency: Option<String>,
    #[serde(rename = "regularMarketTime")]
    regular_market_time: Option<i64>,
}

#[derive(Deserialize)]
struct YahooHistoryResponse {
    chart: YahooHistoryChart,
}

#[derive(Deserialize)]
struct YahooHistoryChart {
    result: Option<Vec<YahooHistoryResult>>,
    error: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct YahooHistoryResult {
    timestamp: Option<Vec<i64>>,
    indicators: YahooHistoryIndicators,
}

#[derive(Deserialize)]
struct YahooHistoryIndicators {
    quote: Vec<YahooHistoryQuote>,
}

#[derive(Deserialize)]
struct YahooHistoryQuote {
    close: Option<Vec<Option<f64>>>,
    volume: Option<Vec<Option<i64>>>,
}

async fn fetch_price_history_from_yahoo(client: &Client, symbol: &str, days: i64) -> Result<Vec<PriceHistoryPoint>, String> {
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

    let mut records = Vec::with_capacity(timestamps.len());
    for (index, ts) in timestamps.iter().enumerate() {
        let date = Utc
            .timestamp_opt(*ts, 0)
            .single()
            .ok_or_else(|| format!("Invalid timestamp {} for {}", ts, symbol))?
            .format("%Y-%m-%d")
            .to_string();

        let close = quote.close.as_ref().and_then(|v| v.get(index).cloned().flatten());
        let volume = quote.volume.as_ref().and_then(|v| v.get(index).cloned().flatten());

        if close.is_some() {
            records.push(PriceHistoryPoint { date, close, volume });
        }
    }

    if records.is_empty() {
        Err(format!("Yahoo returned no historical prices for {}", symbol))
    } else {
        Ok(records)
    }
}

async fn fetch_current_price(client: &Client, symbol: &str) -> Result<YahooMeta, String> {
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
    // Fall back to the time-series volume when regularMarketVolume is absent in metadata
    if meta.regular_market_volume.is_none() {
        meta.regular_market_volume = result.indicators
            .as_ref()
            .and_then(|ind| ind.quote.first())
            .and_then(|q| q.volume.as_ref())
            .and_then(|vols| vols.iter().filter_map(|v| *v).last());
    }
    Ok(meta)
}

async fn fetch_current_prices_for_symbols(
    db_path: &PathBuf,
    symbols: &[String],
) -> Result<Vec<CurrentPrice>, String> {
    let client = Client::builder()
        .user_agent("stocks-api/1.0")
        .build()
        .map_err(|err| err.to_string())?;
    let mut prices = Vec::new();
    let now = Utc::now().to_rfc3339();

    for symbol in symbols {
        match fetch_current_price(&client, symbol).await {
            Ok(meta) => {
                let _ = insert_event_log(db_path, "info", "price_fetch", "api", Some(symbol), &format!("Fetched price from Yahoo: {:?}", meta.regular_market_price));
                // Store instrument type and long name if available
                if meta.instrument_type.is_some() || meta.long_name.is_some() || meta.currency.is_some() {
                    let _ = store_symbol_info(db_path, symbol, meta.instrument_type.as_deref(), meta.long_name.as_deref(), meta.currency.as_deref());
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
                    Utc.timestamp_opt(ts, 0).single().map(|dt| dt.format("%Y-%m-%d").to_string())
                });
                prices.push(CurrentPrice {
                    symbol: symbol.clone(),
                    price: meta.regular_market_price,
                    change,
                    change_percent,
                    volume: meta.regular_market_volume,
                    last_updated: now.clone(),
                    price_date,
                    error: None,
                });
            }
            Err(err) => {
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
                    last_updated: now.clone(),
                    price_date: None,
                    error: Some(error_message),
                });
            }
        }
    }

    Ok(prices)
}

fn store_symbol_info(db_path: &PathBuf, symbol: &str, instrument_type: Option<&str>, long_name: Option<&str>, currency: Option<&str>) -> Result<(), String> {
    let conn = Connection::open(db_path).map_err(|e| e.to_string())?;
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

fn insert_event_log(
    db_path: &PathBuf,
    level: &str,
    event_type: &str,
    source: &str,
    symbol: Option<&str>,
    details: &str,
) -> Result<(), String> {
    let conn = Connection::open(db_path).map_err(|err| err.to_string())?;
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO event_log (timestamp, level, source, event_type, symbol, details) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![now, level, source, event_type, symbol, details],
    )
    .map_err(|err| err.to_string())?;
    Ok(())
}

fn fetch_event_log(db_path: &PathBuf, q: &EventQuery) -> Result<(Vec<EventLogEntry>, i64), String> {
    let conn = Connection::open(db_path).map_err(|err| err.to_string())?;
    let page = q.page.unwrap_or(1).max(1);
    let size = q.size.unwrap_or(50).clamp(1, 1000);
    let offset = ((page - 1) as i64) * (size as i64);

    let mut conditions = Vec::new();
    let mut params_vals: Vec<String> = Vec::new();

    if let Some(ref level) = q.level {
        conditions.push("level = ?".to_string());
        params_vals.push(level.clone());
    }
    if let Some(ref source) = q.source {
        conditions.push("source = ?".to_string());
        params_vals.push(source.clone());
    }
    if let Some(ref event_type) = q.event_type {
        conditions.push("event_type = ?".to_string());
        params_vals.push(event_type.clone());
    }
    if let Some(ref symbol) = q.symbol {
        conditions.push("symbol = ?".to_string());
        params_vals.push(symbol.clone());
    }

    let where_clause = if conditions.is_empty() { "".to_string() } else { format!("WHERE {}", conditions.join(" AND ")) };

    // total count
    let count_sql = format!("SELECT COUNT(*) FROM event_log {}", where_clause);
    let mut count_stmt = conn.prepare(&count_sql).map_err(|e| e.to_string())?;
    let total: i64 = if params_vals.is_empty() {
        count_stmt.query_row([], |r| r.get(0)).map_err(|e| e.to_string())?
    } else {
        // Box the parameters so we can take &dyn ToSql references
        let mut params_box: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        for s in &params_vals {
            params_box.push(Box::new(s.clone()));
        }
        let params_refs: Vec<&dyn rusqlite::ToSql> = params_box.iter().map(|b| b.as_ref() as &dyn rusqlite::ToSql).collect();
        count_stmt.query_row(rusqlite::params_from_iter(params_refs), |r| r.get(0)).map_err(|e| e.to_string())?
    };

    // select items
    let sql = format!("SELECT id, timestamp, level, source, event_type, symbol, details FROM event_log {} ORDER BY id DESC LIMIT ? OFFSET ?", where_clause);
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;

    // build final params with limit and offset
    let mut final_params_box: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    for v in &params_vals {
        final_params_box.push(Box::new(v.clone()));
    }
    let size_i64 = size as i64;
    final_params_box.push(Box::new(size_i64));
    final_params_box.push(Box::new(offset));
    let final_params_refs: Vec<&dyn rusqlite::ToSql> = final_params_box.iter().map(|b| b.as_ref() as &dyn rusqlite::ToSql).collect();

    let rows = stmt.query_map(rusqlite::params_from_iter(final_params_refs), |row| {
        Ok(EventLogEntry {
            id: row.get(0)?,
            timestamp: row.get(1)?,
            level: row.get(2)?,
            source: row.get(3)?,
            event_type: row.get(4)?,
            symbol: row.get::<_, Option<String>>(5)?,
            details: row.get::<_, Option<String>>(6)?,
        })
    }).map_err(|e| e.to_string())?;

    let items = rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?;
    Ok((items, total))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use rusqlite::Connection;
    use tempfile::NamedTempFile;

    // -------------------------------------------------------------------------
    // Helpers
    // -------------------------------------------------------------------------

    fn make_tx(id: i64, tx_type: &str, date: &str, quantity: f64, price: f64) -> HoldingTransaction {
        HoldingTransaction {
            id,
            symbol: "TST.AX".to_string(),
            transaction_type: tx_type.to_string(),
            date: date.to_string(),
            quantity: Some(quantity),
            price: Some(price),
            amount: None,
            brokerage: None,
            notes: None,
            created_at: "2024-01-01T00:00:00Z".to_string(),
            dividends_total: 0.0,
        }
    }

    fn make_event(date: &str, amount: f64) -> DividendEvent {
        DividendEvent {
            symbol: "TST.AX".to_string(),
            ex_date: NaiveDate::parse_from_str(date, "%Y-%m-%d").unwrap(),
            payment_date: None,
            record_date: None,
            amount,
            fetched_at: "2024-01-01T00:00:00Z".to_string(),
        }
    }

    fn setup_test_db() -> (NamedTempFile, PathBuf) {
        let file = NamedTempFile::new().unwrap();
        let path = PathBuf::from(file.path());
        init_db(&path).unwrap();
        (file, path)
    }

    fn insert_tx(db_path: &PathBuf, id: i64, tx_type: &str, date: &str, qty: f64, price: f64, brokerage: f64) {
        let conn = Connection::open(db_path).unwrap();
        conn.execute(
            "INSERT INTO holdings_transactions (id, symbol, transaction_type, date, quantity, price, brokerage, created_at)
             VALUES (?1, 'TST.AX', ?2, ?3, ?4, ?5, ?6, '2024-01-01T00:00:00Z')",
            rusqlite::params![id, tx_type, date, qty, price, brokerage],
        ).unwrap();
    }

    fn insert_dividend_event(db_path: &PathBuf, ex_date: &str, amount: f64) {
        let conn = Connection::open(db_path).unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO dividend_events (symbol, ex_date, amount, fetched_at)
             VALUES ('TST.AX', ?1, ?2, '2024-01-01T00:00:00Z')",
            rusqlite::params![ex_date, amount],
        ).unwrap();
    }

    // -------------------------------------------------------------------------
    // calculate_shares_on_date
    // -------------------------------------------------------------------------

    #[test]
    fn shares_on_date_single_purchase() {
        let txs = vec![make_tx(1, "purchase", "2024-01-15", 100.0, 10.0)];
        let date = NaiveDate::parse_from_str("2024-02-01", "%Y-%m-%d").unwrap();
        assert_eq!(calculate_shares_on_date(&txs, date), 100.0);
    }

    #[test]
    fn shares_on_date_before_purchase() {
        let txs = vec![make_tx(1, "purchase", "2024-03-01", 100.0, 10.0)];
        let date = NaiveDate::parse_from_str("2024-02-01", "%Y-%m-%d").unwrap();
        assert_eq!(calculate_shares_on_date(&txs, date), 0.0);
    }

    #[test]
    fn shares_on_date_on_purchase_day() {
        // ex_date is inclusive — purchase on the same day counts
        let txs = vec![make_tx(1, "purchase", "2024-02-01", 100.0, 10.0)];
        let date = NaiveDate::parse_from_str("2024-02-01", "%Y-%m-%d").unwrap();
        assert_eq!(calculate_shares_on_date(&txs, date), 100.0);
    }

    #[test]
    fn shares_on_date_after_partial_sale() {
        let txs = vec![
            make_tx(1, "purchase", "2024-01-01", 200.0, 10.0),
            make_tx(2, "sale",     "2024-03-01",  50.0, 12.0),
        ];
        let date = NaiveDate::parse_from_str("2024-04-01", "%Y-%m-%d").unwrap();
        assert_eq!(calculate_shares_on_date(&txs, date), 150.0);
    }

    #[test]
    fn shares_on_date_between_purchase_and_sale() {
        let txs = vec![
            make_tx(1, "purchase", "2024-01-01", 200.0, 10.0),
            make_tx(2, "sale",     "2024-06-01",  50.0, 12.0),
        ];
        // Date is after purchase but before sale
        let date = NaiveDate::parse_from_str("2024-03-01", "%Y-%m-%d").unwrap();
        assert_eq!(calculate_shares_on_date(&txs, date), 200.0);
    }

    #[test]
    fn shares_on_date_fully_sold_returns_zero() {
        let txs = vec![
            make_tx(1, "purchase", "2024-01-01", 100.0, 10.0),
            make_tx(2, "sale",     "2024-06-01", 100.0, 15.0),
        ];
        let date = NaiveDate::parse_from_str("2024-12-01", "%Y-%m-%d").unwrap();
        assert_eq!(calculate_shares_on_date(&txs, date), 0.0);
    }

    #[test]
    fn shares_on_date_multiple_purchases() {
        let txs = vec![
            make_tx(1, "purchase", "2024-01-01", 100.0, 10.0),
            make_tx(2, "purchase", "2024-03-01",  50.0, 11.0),
        ];
        let date = NaiveDate::parse_from_str("2024-04-01", "%Y-%m-%d").unwrap();
        assert_eq!(calculate_shares_on_date(&txs, date), 150.0);
    }

    // -------------------------------------------------------------------------
    // calculate_dividend_payments
    // -------------------------------------------------------------------------

    #[test]
    fn dividend_payment_for_shares_held() {
        let txs = vec![make_tx(1, "purchase", "2024-01-01", 100.0, 10.0)];
        let events = vec![make_event("2024-06-01", 0.50)];
        let payments = calculate_dividend_payments(&txs, &events);
        assert_eq!(payments.len(), 1);
        assert_eq!(payments[0].shares_held, 100.0);
        assert!((payments[0].total_payment - 50.0).abs() < 0.001);
    }

    #[test]
    fn dividend_payment_before_purchase_is_zero() {
        let txs = vec![make_tx(1, "purchase", "2024-07-01", 100.0, 10.0)];
        let events = vec![make_event("2024-06-01", 0.50)]; // ex_date before purchase
        let payments = calculate_dividend_payments(&txs, &events);
        assert_eq!(payments.len(), 0);
    }

    #[test]
    fn dividend_payment_after_full_sale_is_zero() {
        let txs = vec![
            make_tx(1, "purchase", "2024-01-01", 100.0, 10.0),
            make_tx(2, "sale",     "2024-04-01", 100.0, 12.0),
        ];
        let events = vec![make_event("2024-06-01", 0.50)];
        let payments = calculate_dividend_payments(&txs, &events);
        assert_eq!(payments.len(), 0);
    }

    #[test]
    fn dividend_payment_proportional_to_shares_held() {
        let txs = vec![
            make_tx(1, "purchase", "2024-01-01", 200.0, 10.0),
            make_tx(2, "sale",     "2024-04-01", 100.0, 12.0), // 100 remain
        ];
        let events = vec![make_event("2024-06-01", 0.50)];
        let payments = calculate_dividend_payments(&txs, &events);
        assert_eq!(payments.len(), 1);
        assert_eq!(payments[0].shares_held, 100.0);
        assert!((payments[0].total_payment - 50.0).abs() < 0.001);
    }

    #[test]
    fn dividend_payment_multiple_events() {
        let txs = vec![make_tx(1, "purchase", "2024-01-01", 100.0, 10.0)];
        let events = vec![
            make_event("2024-03-01", 0.30),
            make_event("2024-09-01", 0.35),
        ];
        let payments = calculate_dividend_payments(&txs, &events);
        assert_eq!(payments.len(), 2);
        let total: f64 = payments.iter().map(|p| p.total_payment).sum();
        assert!((total - 65.0).abs() < 0.001);
    }

    // -------------------------------------------------------------------------
    // Integration tests: DB-backed operations
    // -------------------------------------------------------------------------

    #[test]
    fn integration_load_holding_symbols_excludes_fully_sold() {
        let (_file, db_path) = setup_test_db();
        insert_tx(&db_path, 1, "purchase", "2024-01-01", 100.0, 10.0, 0.0);
        insert_tx(&db_path, 2, "sale",     "2024-06-01", 100.0, 12.0, 0.0);

        let symbols = load_holding_symbols(&db_path).unwrap();
        assert!(!symbols.contains(&"TST.AX".to_string()), "fully sold symbol should be excluded");
    }

    #[test]
    fn integration_load_holding_symbols_includes_partial_holding() {
        let (_file, db_path) = setup_test_db();
        insert_tx(&db_path, 1, "purchase", "2024-01-01", 100.0, 10.0, 0.0);
        insert_tx(&db_path, 2, "sale",     "2024-06-01",  40.0, 12.0, 0.0);

        let symbols = load_holding_symbols(&db_path).unwrap();
        assert!(symbols.contains(&"TST.AX".to_string()), "partially sold symbol should be included");
    }

    #[test]
    fn integration_calculate_dividend_totals_filters_pre_purchase() {
        let (_file, db_path) = setup_test_db();
        // Insert a purchase
        insert_tx(&db_path, 1, "purchase", "2024-06-01", 100.0, 10.0, 0.0);
        // Dividend before purchase — should be excluded
        insert_dividend_event(&db_path, "2024-01-01", 0.50);
        // Dividend after purchase — should be included
        insert_dividend_event(&db_path, "2024-09-01", 0.30);

        let conn = Connection::open(&db_path).unwrap();
        let txs = fetch_holdings(&db_path).unwrap();
        drop(conn);

        let totals = calculate_dividend_totals(&db_path, &txs).unwrap();
        let total = totals.get("TST.AX").copied().unwrap_or(0.0);
        // Only the September dividend (100 shares × $0.30 = $30) should count
        assert!((total - 30.0).abs() < 0.001, "expected $30 dividend, got ${}", total);
    }

    #[test]
    fn integration_insert_and_fetch_holding_transaction() {
        let (_file, db_path) = setup_test_db();

        let payload = NewHoldingTransaction {
            symbol: "TST.AX".to_string(),
            transaction_type: "purchase".to_string(),
            date: "2024-01-15".to_string(),
            quantity: Some(50.0),
            price: Some(12.50),
            amount: None,
            brokerage: Some(9.95),
            notes: Some("initial buy".to_string()),
        };

        let result = insert_holding_transaction(&db_path, "TST.AX", payload);
        assert!(result.is_ok(), "insert failed: {:?}", result.err());

        let tx = result.unwrap();
        assert_eq!(tx.quantity, Some(50.0));
        assert_eq!(tx.price, Some(12.50));
        assert_eq!(tx.brokerage, Some(9.95));

        let holdings = fetch_holdings(&db_path).unwrap();
        assert_eq!(holdings.len(), 1);
        assert_eq!(holdings[0].symbol, "TST.AX");
    }

    #[test]
    fn integration_insert_transaction_rejects_zero_quantity() {
        let (_file, db_path) = setup_test_db();

        let payload = NewHoldingTransaction {
            symbol: "TST.AX".to_string(),
            transaction_type: "purchase".to_string(),
            date: "2024-01-15".to_string(),
            quantity: Some(0.0),
            price: Some(10.0),
            amount: None,
            brokerage: None,
            notes: None,
        };

        let result = insert_holding_transaction(&db_path, "TST.AX", payload);
        assert!(result.is_err(), "should reject zero quantity");
    }
}

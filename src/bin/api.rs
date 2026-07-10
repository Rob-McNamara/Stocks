use actix_cors::Cors;
use actix_web::{delete, get, post, put, web, App, HttpResponse, HttpServer, Responder};
use chrono::{Datelike, NaiveDate, TimeZone, Timelike, Utc};
use reqwest::Client;
use rusqlite::{params, types::Type, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, env, path::PathBuf};
use stocks::portfolio::{self, PortfolioTx, TxType};

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
    added_at: String,
    notes: Option<String>,
    breakthrough_price: Option<f64>,
    stop_loss_price: Option<f64>,
    custom_fields: std::collections::HashMap<String, String>,
}

#[derive(Deserialize)]
struct AddWatchlistSymbol {
    symbol: String,
    list_name: Option<String>,
    notes: Option<String>,
    breakthrough_price: Option<f64>,
    stop_loss_price: Option<f64>,
    custom_fields: Option<std::collections::HashMap<String, String>>,
}

/// Deserializer that distinguishes "field absent from the JSON" (outer None)
/// from "field explicitly set to null" (Some(None)), so partial updates keep
/// values the client didn't send instead of wiping them.
fn deserialize_explicit_null<'de, T, D>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    T: serde::Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    Ok(Some(Option::<T>::deserialize(deserializer)?))
}

#[derive(Deserialize)]
struct UpdateWatchlistSymbol {
    #[serde(default, deserialize_with = "deserialize_explicit_null")]
    notes: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_explicit_null")]
    breakthrough_price: Option<Option<f64>>,
    #[serde(default, deserialize_with = "deserialize_explicit_null")]
    stop_loss_price: Option<Option<f64>>,
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
    #[serde(default)]
    custom_fields: std::collections::HashMap<String, String>,
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

#[allow(dead_code)] // only total_payment is aggregated today; other fields document the calculation
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
    custom_fields: Option<std::collections::HashMap<String, String>>,
    /// Set true to record a sale of more shares than currently held
    /// (the API responds 409 with a warning otherwise).
    confirm: Option<bool>,
}

/// Shared pre-processing for holding create/update:
/// - Server-side FX: a foreign-currency payload may send just `original_price`
///   and `currency`; the AUD price and rate are resolved here so thin clients
///   never do currency math.
/// - Over-sell guard (create only, when `check_oversell`): selling more than
///   held returns 409 unless `confirm: true` is supplied.
async fn prepare_holding_payload(
    db_path: &PathBuf,
    symbol: &str,
    payload: &mut NewHoldingTransaction,
    check_oversell: bool,
) -> Result<(), HttpResponse> {
    let currency = payload.currency.clone().unwrap_or_else(|| "AUD".to_string());
    if currency != "AUD" && payload.price.is_none() {
        let Some(original_price) = payload.original_price else {
            return Err(err_bad_request("original_price is required for foreign-currency transactions"));
        };
        let target_date = NaiveDate::parse_from_str(&payload.date, "%Y-%m-%d")
            .map_err(|_| err_bad_request("Invalid date format. Use YYYY-MM-DD."))?;
        match fetch_fx_rate_on_date(&currency, target_date).await {
            Ok((rate, _)) => {
                payload.fx_rate = Some(rate);
                payload.price = Some(original_price * rate);
            }
            Err(err) => {
                let _ = insert_event_log(db_path, "error", "fx_fetch", "api", Some(symbol), &err);
                return Err(err_unprocessable(format!(
                    "No {}/AUD exchange rate available for {}: {}",
                    currency, payload.date, err
                )));
            }
        }
    }

    if check_oversell && payload.transaction_type == "sale" && payload.confirm != Some(true) {
        let held: f64 = open_db(db_path)
            .ok()
            .and_then(|conn| {
                conn.query_row(
                    "SELECT COALESCE(SUM(CASE WHEN transaction_type = 'purchase' THEN quantity ELSE -quantity END), 0)
                     FROM holdings_transactions
                     WHERE symbol = ?1 AND transaction_type IN ('purchase', 'sale')",
                    params![symbol],
                    |row| row.get(0),
                )
                .ok()
            })
            .unwrap_or(0.0);
        if let Some(qty) = payload.quantity {
            if qty > held + 1e-9 {
                return Err(HttpResponse::Conflict().json(serde_json::json!({
                    "error": {
                        "code": "oversell_confirmation_required",
                        "message": format!("Selling {} shares but only {:.2} held for {}. Re-submit with confirm=true to record anyway.", qty, held, symbol),
                        "held": held,
                    }
                })));
            }
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Consistent v1 error envelope: every non-2xx response carries
// {"error": {"code": "...", "message": "..."}} so all clients (web, iOS,
// Android) parse failures the same way.
// ---------------------------------------------------------------------------
fn api_error(status: actix_web::http::StatusCode, code: &str, message: impl Into<String>) -> HttpResponse {
    HttpResponse::build(status).json(serde_json::json!({
        "error": { "code": code, "message": message.into() }
    }))
}

fn err_internal(message: impl Into<String>) -> HttpResponse {
    api_error(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, "internal_error", message)
}

fn err_bad_request(message: impl Into<String>) -> HttpResponse {
    api_error(actix_web::http::StatusCode::BAD_REQUEST, "bad_request", message)
}

fn err_not_found(message: impl Into<String>) -> HttpResponse {
    api_error(actix_web::http::StatusCode::NOT_FOUND, "not_found", message)
}

fn err_unprocessable(message: impl Into<String>) -> HttpResponse {
    api_error(actix_web::http::StatusCode::UNPROCESSABLE_ENTITY, "unprocessable", message)
}

/// Open the SQLite database with WAL mode and a busy timeout so the API,
/// price daemon and dividends daemon can write concurrently without
/// intermittent "database is locked" failures.
fn open_db<P: AsRef<std::path::Path>>(path: P) -> Result<Connection, rusqlite::Error> {
    let conn = Connection::open(path)?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    let _: String = conn.query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))?;
    Ok(conn)
}

#[utoipa::path(get, path = "/api/v1/health", tag = "system", responses((status = 200, description = "Health")))]
#[get("/api/health")]
async fn health() -> impl Responder {
    HttpResponse::Ok().json(HealthResponse { status: "ok" })
}

#[utoipa::path(get, path = "/api/v1/watchlist", tag = "watchlist", responses((status = 200, description = "Get watchlist")))]
#[get("/api/watchlist")]
async fn get_watchlist(db_path: web::Data<PathBuf>, query: web::Query<WatchlistQuery>) -> impl Responder {
    match load_watchlist_symbols(&db_path, query.list.as_deref()) {
        Ok(symbols) => HttpResponse::Ok().json(symbols),
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "watchlist_fetch", "api", None, &err);
            err_internal(err)
        }
    }
}

#[utoipa::path(get, path = "/api/v1/watchlist/lists", tag = "watchlist", responses((status = 200, description = "Get watchlist lists")))]
#[get("/api/watchlist/lists")]
async fn get_watchlist_lists(db_path: web::Data<PathBuf>) -> impl Responder {
    match load_watchlist_lists(&db_path) {
        Ok(lists) => HttpResponse::Ok().json(lists),
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "watchlist_fetch", "api", None, &err);
            err_internal(err)
        }
    }
}

#[utoipa::path(post, path = "/api/v1/watchlist", tag = "watchlist", responses((status = 200, description = "Add watchlist symbol")))]
#[post("/api/watchlist")]
async fn add_watchlist_symbol(
    db_path: web::Data<PathBuf>,
    payload: web::Json<AddWatchlistSymbol>,
) -> impl Responder {
    let symbol = payload.symbol.trim();
    if symbol.is_empty() {
        return err_bad_request("Symbol is required");
    }

    let normalized = normalize_symbol(symbol);
    let list_name = payload.list_name.as_deref().unwrap_or("Default");
    let notes = payload.notes.as_deref();
    let custom_fields = payload.custom_fields.as_ref();
    let breakthrough_price = payload.breakthrough_price;
    let stop_loss_price = payload.stop_loss_price;
    match insert_watchlist_symbol(&db_path, &normalized, list_name, notes, breakthrough_price, stop_loss_price, custom_fields) {
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
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "watchlist_add", "api", Some(&normalized), &err);
            err_internal(err)
        }
    }
}

#[utoipa::path(put, path = "/api/v1/watchlist/{id}", tag = "watchlist", params(("id" = i64, Path, description = "id")), responses((status = 200, description = "Update watchlist symbol")))]
#[put("/api/watchlist/{id}")]
async fn update_watchlist_symbol(
    db_path: web::Data<PathBuf>,
    path: web::Path<i64>,
    payload: web::Json<UpdateWatchlistSymbol>,
) -> impl Responder {
    let id = path.into_inner();
    let payload = payload.into_inner();
    match update_watchlist_symbol_notes(&db_path, id, payload.notes, payload.breakthrough_price, payload.stop_loss_price, payload.custom_fields.as_ref()) {
        Ok(row) => HttpResponse::Ok().json(row),
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "watchlist_update", "api", None, &err);
            err_internal(err)
        }
    }
}

#[utoipa::path(delete, path = "/api/v1/watchlist/{id}", tag = "watchlist", params(("id" = i64, Path, description = "id")), responses((status = 200, description = "Delete watchlist symbol")))]
#[delete("/api/watchlist/{id}")]
async fn delete_watchlist_symbol(
    db_path: web::Data<PathBuf>,
    path: web::Path<i64>,
) -> impl Responder {
    let id = path.into_inner();
    match remove_watchlist_symbol(&db_path, id) {
        Ok(true) => HttpResponse::NoContent().finish(),
        Ok(false) => err_not_found("Symbol not found"),
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "watchlist_delete", "api", None, &err);
            err_internal(err)
        }
    }
}

#[derive(Deserialize)]
struct RenameWatchlistList {
    old_name: String,
    new_name: String,
}

#[utoipa::path(put, path = "/api/v1/watchlist/lists/rename", tag = "watchlist", responses((status = 200, description = "Rename watchlist list")))]
#[put("/api/watchlist/lists/rename")]
async fn rename_watchlist_list(
    db_path: web::Data<PathBuf>,
    payload: web::Json<RenameWatchlistList>,
) -> impl Responder {
    let old_name = payload.old_name.trim();
    let new_name = payload.new_name.trim();
    if old_name.is_empty() || new_name.is_empty() {
        return err_bad_request("Both old and new list names are required");
    }
    if old_name == new_name {
        return HttpResponse::Ok().json("ok");
    }
    let mut conn = match open_db(db_path.as_ref()) {
        Ok(c) => c,
        Err(err) => return err_internal(err.to_string()),
    };
    // Renaming onto an existing list merges the two: memberships that would
    // collide with UNIQUE(symbol, list_name) are dropped, the rest are moved.
    let result = (|| -> Result<usize, rusqlite::Error> {
        let tx = conn.transaction()?;
        let deduped = tx.execute(
            "DELETE FROM watchlist_memberships WHERE list_name = ?1
             AND symbol IN (SELECT symbol FROM watchlist_memberships WHERE list_name = ?2)",
            params![old_name, new_name],
        )?;
        let moved = tx.execute(
            "UPDATE watchlist_memberships SET list_name = ?1 WHERE list_name = ?2",
            params![new_name, old_name],
        )?;
        tx.commit()?;
        Ok(deduped + moved)
    })();
    match result {
        Ok(affected) if affected > 0 => {
            let _ = insert_event_log(&db_path, "info", "watchlist_list_rename", "api", None, &format!("Renamed list '{}' to '{}'", old_name, new_name));
            HttpResponse::Ok().json("ok")
        }
        Ok(_) => err_not_found(format!("List '{}' not found", old_name)),
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "watchlist_list_rename", "api", None, &format!("Failed to rename list: {}", err));
            err_internal(err.to_string())
        }
    }
}

#[utoipa::path(get, path = "/api/v1/config", tag = "config", responses((status = 200, description = "Get config")))]
#[get("/api/config")]
async fn get_config(db_path: web::Data<PathBuf>) -> impl Responder {
    match load_config(&db_path) {
        Ok(config) => {
            // Never send the AI API key to clients; expose only whether one is set.
            let has_key = config.iter().any(|c| c.key == "ai_api_key" && !c.value.is_empty());
            let mut items: Vec<ConfigItem> = config.into_iter().filter(|c| c.key != "ai_api_key").collect();
            items.push(ConfigItem { key: "ai_api_key_configured".to_string(), value: has_key.to_string() });
            HttpResponse::Ok().json(items)
        }
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "config_fetch", "api", None, &err);
            err_internal(err)
        }
    }
}

#[utoipa::path(put, path = "/api/v1/config", tag = "config", responses((status = 200, description = "Update config")))]
#[put("/api/config")]
async fn update_config(
    db_path: web::Data<PathBuf>,
    payload: web::Json<UpdateConfig>,
) -> impl Responder {
    let key = payload.key.trim();
    let value = payload.value.trim();
    if key.is_empty() {
        return err_bad_request("Config key is required");
    }

    match upsert_config(&db_path, key, value) {
        Ok(()) => {
            let _ = insert_event_log(&db_path, "info", "config_update", "api", Some(key), &format!("Updated config {}", key));
            HttpResponse::NoContent().finish()
        }
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "config_update", "api", Some(key), &err);
            err_internal(err)
        }
    }
}

#[utoipa::path(get, path = "/api/v1/watchlist/prices", tag = "watchlist", responses((status = 200, description = "Get watchlist prices")))]
#[get("/api/watchlist/prices")]
async fn get_watchlist_prices(db_path: web::Data<PathBuf>, query: web::Query<WatchlistQuery>) -> impl Responder {
    match fetch_watchlist_current_prices(&db_path, query.list.as_deref()).await {
        Ok(prices) => HttpResponse::Ok().json(prices),
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "price_fetch", "api", None, &err);
            err_internal(err)
        }
    }
}

#[derive(Deserialize)]
struct CurrentPricesQuery {
    symbols: String,
}

#[utoipa::path(get, path = "/api/v1/watchlist/cached-prices", tag = "watchlist", responses((status = 200, description = "Get watchlist cached prices")))]
#[get("/api/watchlist/cached-prices")]
async fn get_watchlist_cached_prices(db_path: web::Data<PathBuf>, query: web::Query<WatchlistQuery>) -> impl Responder {
    let symbols_result = load_watchlist_symbols(&db_path, query.list.as_deref());
    match symbols_result {
        Ok(symbols) => {
            let sym_names: Vec<String> = symbols.into_iter().map(|s| s.symbol).collect();
            match load_cached_prices_with_fallback(&db_path, &sym_names) {
                Ok(prices) => HttpResponse::Ok().json(prices),
                Err(err) => {
                    let _ = insert_event_log(&db_path, "error", "cached_prices_fetch", "api", None, &err);
                    err_internal(err)
                }
            }
        }
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "cached_prices_fetch", "api", None, &err);
            err_internal(err)
        }
    }
}

#[utoipa::path(get, path = "/api/v1/cached-prices", tag = "prices", responses((status = 200, description = "Get cached prices")))]
#[get("/api/cached-prices")]
async fn get_cached_prices(db_path: web::Data<PathBuf>, query: web::Query<CurrentPricesQuery>) -> impl Responder {
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

    match fetch_and_cache_current_prices(&db_path, &symbols, "holdings_prices_updated_at").await {
        Ok(prices) => HttpResponse::Ok().json(prices),
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "price_fetch", "api", None, &err);
            err_internal(err)
        }
    }
}

#[utoipa::path(get, path = "/api/v1/holdings", tag = "holdings", responses((status = 200, description = "Get holdings")))]
#[get("/api/holdings")]
async fn get_holdings(db_path: web::Data<PathBuf>) -> impl Responder {
    match fetch_holdings(&db_path) {
        Ok(history) => HttpResponse::Ok().json(history),
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "holdings_fetch", "api", None, &err);
            err_internal(err)
        }
    }
}

#[utoipa::path(get, path = "/api/v1/symbol-info", tag = "meta", responses((status = 200, description = "Get symbol info")))]
#[get("/api/symbol-info")]
async fn get_symbol_info(db_path: web::Data<PathBuf>) -> impl Responder {
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
        Ok(mapped) => {
            let items: Vec<_> = mapped.filter_map(|r| r.ok()).collect();
            HttpResponse::Ok().json(items)
        }
        Err(err) => err_internal(err.to_string()),
    }
}

#[derive(Deserialize)]
struct FxRateQuery {
    currency: String,
    date: String,
}

/// Closest AUD rate on or before `target_date` for `currency` (e.g. "USD").
/// Returns (rate, rate_date).
async fn fetch_fx_rate_on_date(currency: &str, target_date: NaiveDate) -> Result<(f64, String), String> {
    let pair = format!("{}AUD=X", currency.trim().to_uppercase());
    let client = Client::builder().user_agent("stocks-api/1.0").build().map_err(|e| e.to_string())?;
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
    let payload: YahooHistoryResponse = response
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
        .indicators
        .quote
        .first()
        .and_then(|q| q.close.as_ref())
        .ok_or_else(|| "No close data".to_string())?;
    // Find the entry closest to and on-or-before the target date
    let target_str = target_date.format("%Y-%m-%d").to_string();
    let mut best: Option<(f64, String)> = None;
    for (i, ts) in timestamps.iter().enumerate() {
        let date_str = Utc.timestamp_opt(*ts, 0)
            .single()
            .map(|dt| dt.format("%Y-%m-%d").to_string())
            .unwrap_or_default();
        if date_str <= target_str {
            if let Some(Some(rate)) = closes.get(i) {
                best = Some((*rate, date_str));
            }
        }
    }
    best.ok_or_else(|| format!("No FX rate found on or before {}", target_str))
}

#[utoipa::path(get, path = "/api/v1/fx-rate", tag = "fx", responses((status = 200, description = "Get fx rate for date")))]
#[get("/api/fx-rate")]
async fn get_fx_rate_for_date(db_path: web::Data<PathBuf>, query: web::Query<FxRateQuery>) -> impl Responder {
    let currency = query.currency.trim().to_uppercase();
    let target_date = match NaiveDate::parse_from_str(&query.date, "%Y-%m-%d") {
        Ok(d) => d,
        Err(_) => return err_bad_request("Invalid date format, use YYYY-MM-DD"),
    };
    match fetch_fx_rate_on_date(&currency, target_date).await {
        Ok((rate, date)) => HttpResponse::Ok().json(serde_json::json!({ "rate": rate, "date": date })),
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
struct FxRatesQuery {
    currencies: Option<String>,
}

#[utoipa::path(get, path = "/api/v1/fx-rates", tag = "fx", responses((status = 200, description = "Get fx rates")))]
#[get("/api/fx-rates")]
async fn get_fx_rates(db_path: web::Data<PathBuf>, query: web::Query<FxRatesQuery>) -> impl Responder {
    let client = match Client::builder().user_agent("stocks-api/1.0").build() {
        Ok(c) => c,
        Err(err) => return err_internal(err.to_string()),
    };
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
        match fetch_current_price(&client, &format!("{}AUD=X", currency)).await {
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

#[utoipa::path(get, path = "/api/v1/dividends", tag = "dividends", responses((status = 200, description = "Get dividends")))]
#[get("/api/dividends")]
async fn get_dividends(db_path: web::Data<PathBuf>) -> impl Responder {
    let conn = match open_db(db_path.as_ref()) {
        Ok(c) => c,
        Err(err) => return err_internal(err.to_string()),
    };
    let mut stmt = match conn.prepare(
        "SELECT symbol, ex_date, payment_date, amount FROM dividend_events ORDER BY ex_date DESC",
    ) {
        Ok(s) => s,
        Err(err) => return err_internal(err.to_string()),
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
        Err(err) => err_internal(err.to_string()),
    }
}

#[utoipa::path(get, path = "/api/v1/events", tag = "events", responses((status = 200, description = "Get events")))]
#[get("/api/events")]
async fn get_events(db_path: web::Data<PathBuf>, query: web::Query<EventQuery>) -> impl Responder {
    match fetch_event_log(&db_path, &query.into_inner()) {
        Ok((items, total)) => HttpResponse::Ok().json(serde_json::json!({"items": items, "total": total})),
        Err(err) => err_internal(err),
    }
}

#[utoipa::path(post, path = "/api/v1/holdings", tag = "holdings", responses((status = 200, description = "Add holding transaction")))]
#[post("/api/holdings")]
async fn add_holding_transaction(
    db_path: web::Data<PathBuf>,
    payload: web::Json<NewHoldingTransaction>,
) -> impl Responder {
    let mut payload = payload.into_inner();
    let symbol = normalize_symbol(&payload.symbol);
    if let Err(response) = prepare_holding_payload(&db_path, &symbol, &mut payload, true).await {
        return response;
    }
    match insert_holding_transaction(&db_path, &symbol, payload) {
        Ok(record) => {
            let _ = insert_event_log(&db_path, "info", "holding_create", "api", Some(&record.symbol), &format!("Created holding id {}", record.id));
            HttpResponse::Ok().json(record)
        }
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "holding_create", "api", Some(&symbol), &err);
            err_bad_request(err)
        }
    }
}

#[utoipa::path(put, path = "/api/v1/holdings/{id}", tag = "holdings", params(("id" = i64, Path, description = "id")), responses((status = 200, description = "Update holding transaction")))]
#[put("/api/holdings/{id}")]
async fn update_holding_transaction(
    db_path: web::Data<PathBuf>,
    path: web::Path<i64>,
    payload: web::Json<NewHoldingTransaction>,
) -> impl Responder {
    let id = path.into_inner();
    let mut payload = payload.into_inner();
    let symbol = normalize_symbol(&payload.symbol);
    if let Err(response) = prepare_holding_payload(&db_path, &symbol, &mut payload, false).await {
        return response;
    }

    match modify_holding_transaction(&db_path, id, &symbol, payload) {
        Ok(record) => {
            let _ = insert_event_log(&db_path, "info", "holding_update", "api", Some(&record.symbol), &format!("Updated holding id {}", record.id));
            HttpResponse::Ok().json(record)
        }
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "holding_update", "api", Some(&symbol), &err);
            err_bad_request(err)
        }
    }
}

#[utoipa::path(delete, path = "/api/v1/holdings/{id}", tag = "holdings", params(("id" = i64, Path, description = "id")), responses((status = 200, description = "Delete holding transaction")))]
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
            err_not_found("Transaction not found")
        }
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "holding_delete", "api", None, &err);
            err_internal(err)
        }
    }
}

#[derive(Deserialize)]
struct RenameHoldingSymbol {
    new_symbol: String,
}

// Two-segment path so it can never collide with `PUT /api/holdings/{id}`
// (which would otherwise try to parse "rename-symbol" as an i64).
#[utoipa::path(put, path = "/api/v1/holdings/rename-symbol/{old_symbol}", tag = "holdings", params(("old_symbol" = String, Path, description = "old_symbol")), responses((status = 200, description = "Rename holding symbol")))]
#[put("/api/holdings/rename-symbol/{old_symbol}")]
async fn rename_holding_symbol(
    db_path: web::Data<PathBuf>,
    path: web::Path<String>,
    payload: web::Json<RenameHoldingSymbol>,
) -> impl Responder {
    let old_symbol = normalize_symbol(&path.into_inner());
    let new_symbol = normalize_symbol(&payload.new_symbol);
    if new_symbol.is_empty() {
        return err_bad_request("New symbol is required");
    }
    if old_symbol == new_symbol {
        return HttpResponse::Ok().json(serde_json::json!({ "renamed": 0 }));
    }
    match rename_holdings_symbol(&db_path, &old_symbol, &new_symbol) {
        Ok(affected) if affected > 0 => {
            let _ = insert_event_log(&db_path, "info", "holding_rename", "api", Some(&new_symbol), &format!("Renamed holding symbol '{}' to '{}' across {} transaction(s)", old_symbol, new_symbol, affected));
            HttpResponse::Ok().json(serde_json::json!({ "renamed": affected }))
        }
        Ok(_) => err_not_found(format!("No holdings found for '{}'", old_symbol)),
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "holding_rename", "api", Some(&old_symbol), &err);
            err_internal(err)
        }
    }
}

/// Rename a holding's symbol across all of its transactions and symbol-level
/// metadata, in a single transaction. Per-transaction custom fields are keyed
/// by transaction id, so they follow automatically. Returns the number of
/// holdings_transactions rows updated.
fn rename_holdings_symbol(db_path: &PathBuf, old_symbol: &str, new_symbol: &str) -> Result<usize, String> {
    let mut conn = open_db(db_path).map_err(|e| e.to_string())?;
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    let affected = tx
        .execute(
            "UPDATE holdings_transactions SET symbol = ?1 WHERE symbol = ?2",
            params![new_symbol, old_symbol],
        )
        .map_err(|e| e.to_string())?;
    // Move symbol-level fields, skipping any keys already present on the target.
    tx.execute(
        "INSERT OR IGNORE INTO holdings_symbol_fields (symbol, field_key, value)
         SELECT ?1, field_key, value FROM holdings_symbol_fields WHERE symbol = ?2",
        params![new_symbol, old_symbol],
    )
    .map_err(|e| e.to_string())?;
    tx.execute(
        "DELETE FROM holdings_symbol_fields WHERE symbol = ?1",
        params![old_symbol],
    )
    .map_err(|e| e.to_string())?;
    // Carry over instrument info if the target doesn't already have it.
    tx.execute(
        "INSERT OR IGNORE INTO symbol_info (symbol, instrument_type, long_name, currency, updated_at)
         SELECT ?1, instrument_type, long_name, currency, updated_at FROM symbol_info WHERE symbol = ?2",
        params![new_symbol, old_symbol],
    )
    .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(affected)
}

#[derive(Deserialize)]
struct HoldingsSymbolFieldsPayload {
    notes: Option<String>,
    custom_fields: Option<std::collections::HashMap<String, String>>,
}

#[utoipa::path(put, path = "/api/v1/holdings/symbol-fields/{symbol}", tag = "holdings", params(("symbol" = String, Path, description = "symbol")), responses((status = 200, description = "Update holdings symbol fields")))]
#[put("/api/holdings/symbol-fields/{symbol}")]
async fn update_holdings_symbol_fields(
    db_path: web::Data<PathBuf>,
    path: web::Path<String>,
    payload: web::Json<HoldingsSymbolFieldsPayload>,
) -> impl Responder {
    let symbol = normalize_symbol(&path.into_inner());
    let conn = match open_db(db_path.as_ref()) {
        Ok(c) => c,
        Err(err) => return err_internal(err.to_string()),
    };
    // Save notes as a special field
    if let Some(ref notes) = payload.notes {
        if let Err(err) = conn.execute(
            "INSERT OR REPLACE INTO holdings_symbol_fields (symbol, field_key, value) VALUES (?1, '_notes', ?2)",
            params![symbol, notes],
        ) {
            let _ = insert_event_log(&db_path, "error", "holdings_symbol_fields_update", "api", Some(&symbol), &format!("Failed to save notes: {}", err));
            return err_internal(err.to_string());
        }
    }
    if let Some(ref fields) = payload.custom_fields {
        if let Err(err) = upsert_holdings_symbol_fields(&conn, &symbol, fields) {
            return err_internal(err);
        }
    }
    HttpResponse::Ok().json("ok")
}

#[utoipa::path(get, path = "/api/v1/holdings/symbol-fields", tag = "holdings", responses((status = 200, description = "Get holdings symbol fields")))]
#[get("/api/holdings/symbol-fields")]
async fn get_holdings_symbol_fields(db_path: web::Data<PathBuf>) -> impl Responder {
    match load_holdings_symbol_fields(&db_path) {
        Ok(fields) => HttpResponse::Ok().json(fields),
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "holdings_symbol_fields_fetch", "api", None, &err);
            err_internal(err)
        }
    }
}

#[derive(Deserialize)]
struct PriceHistoryQuery {
    symbol: String,
    days: Option<i64>,
    /// Comma-separated SMA periods (e.g. "20,50,150"). When present the
    /// response is `{ points, smas }` instead of a bare array.
    smas: Option<String>,
    /// Inject the cached live price as/over the latest point (server-side
    /// equivalent of the chart's live-point injection).
    include_live: Option<bool>,
}

#[derive(Serialize)]
struct PriceHistoryPoint {
    date: String,
    close: Option<f64>,
    volume: Option<i64>,
}

#[utoipa::path(get, path = "/api/v1/price-history", tag = "prices", responses((status = 200, description = "Get price history")))]
#[get("/api/price-history")]
async fn get_price_history(
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
        let live: Option<(Option<f64>, Option<String>, Option<i64>)> = open_db(db_path.as_ref()).ok().and_then(|conn| {
            conn.query_row(
                "SELECT price, price_date, volume FROM cached_current_prices WHERE symbol = ?1",
                params![symbol],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .ok()
            .flatten()
        });
        if let Some((Some(price), price_date, volume)) = live {
            let date = price_date.unwrap_or_else(|| Utc::now().format("%Y-%m-%d").to_string());
            match history.last() {
                None => history.push(PriceHistoryPoint { date, close: Some(price), volume }),
                Some(last) if date == last.date => {
                    let existing_volume = last.volume;
                    let n = history.len();
                    history[n - 1] = PriceHistoryPoint { date, close: Some(price), volume: volume.or(existing_volume) };
                }
                Some(last) if date > last.date.clone() => {
                    history.push(PriceHistoryPoint { date, close: Some(price), volume });
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
    let mut conn = open_db(db_path).map_err(|e| e.to_string())?;
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
    let conn = open_db(db_path).map_err(|e| e.to_string())?;
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
    let conn = open_db(db_path).map_err(|e| e.to_string())?;
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

/// Fetch and store dividend events for the given symbols (in small concurrent
/// batches) and report how many symbols were updated. Shared by the dividend
/// refresh endpoints and /api/refresh.
async fn refresh_dividends_for_symbols(db_path: &PathBuf, symbols: Vec<String>) -> DividendRefreshResult {
    if symbols.is_empty() {
        return DividendRefreshResult { updated: 0, errors: vec![] };
    }

    let client = match Client::builder().user_agent("stocks-api/1.0").build() {
        Ok(c) => c,
        Err(err) => return DividendRefreshResult { updated: 0, errors: vec![err.to_string()] },
    };

    let mut updated = 0;
    let mut errors = Vec::new();

    for chunk in symbols.chunks(5) {
        let mut set = tokio::task::JoinSet::new();
        for symbol in chunk {
            let client = client.clone();
            let sym = symbol.clone();
            set.spawn(async move {
                let result = fetch_dividend_events_for_symbol(&client, &sym).await;
                (sym, result)
            });
        }
        while let Some(joined) = set.join_next().await {
            let Ok((symbol, result)) = joined else { continue };
            match result {
                Ok(events) => {
                    let count = events.len();
                    match store_dividend_events_for_symbol(db_path, &symbol, &events) {
                        Ok(()) => {
                            let details = format!("Stored {} dividend events", count);
                            let _ = insert_event_log(db_path, "info", "dividend_fetch", "api", Some(&symbol), &details);
                            updated += 1;
                        }
                        Err(err) => {
                            let _ = insert_event_log(db_path, "error", "dividend_fetch", "api", Some(&symbol), &err);
                            errors.push(format!("{}: {}", symbol, err));
                        }
                    }
                }
                Err(err) => {
                    let _ = insert_event_log(db_path, "error", "dividend_fetch", "api", Some(&symbol), &err);
                    errors.push(format!("{}: {}", symbol, err));
                }
            }
        }
    }

    DividendRefreshResult { updated, errors }
}

#[utoipa::path(post, path = "/api/v1/dividends/refresh", tag = "dividends", responses((status = 200, description = "Refresh dividends")))]
#[post("/api/dividends/refresh")]
async fn refresh_dividends(db_path: web::Data<PathBuf>) -> impl Responder {
    match load_holding_symbols(&db_path) {
        Ok(symbols) => HttpResponse::Ok().json(refresh_dividends_for_symbols(&db_path, symbols).await),
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "dividend_fetch", "api", None, &err);
            err_internal(err)
        }
    }
}

#[utoipa::path(post, path = "/api/v1/dividends/refresh-sold", tag = "dividends", responses((status = 200, description = "Refresh sold dividends")))]
#[post("/api/dividends/refresh-sold")]
async fn refresh_sold_dividends(db_path: web::Data<PathBuf>) -> impl Responder {
    match load_sold_symbols(&db_path) {
        Ok(symbols) => HttpResponse::Ok().json(refresh_dividends_for_symbols(&db_path, symbols).await),
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "dividend_fetch", "api", None, &err);
            err_internal(err)
        }
    }
}

/// Move a watchlist stock into holdings atomically: record the transaction,
/// then remove every watchlist membership for the symbol. Replaces the
/// multi-request handshake the browser used to orchestrate.
#[utoipa::path(post, path = "/api/v1/holdings/from-watchlist", tag = "holdings", responses((status = 200, description = "Add holding from watchlist")))]
#[post("/api/holdings/from-watchlist")]
async fn add_holding_from_watchlist(
    db_path: web::Data<PathBuf>,
    payload: web::Json<NewHoldingTransaction>,
) -> impl Responder {
    let mut payload = payload.into_inner();
    let symbol = normalize_symbol(&payload.symbol);
    if let Err(response) = prepare_holding_payload(&db_path, &symbol, &mut payload, true).await {
        return response;
    }
    let record = match insert_holding_transaction(&db_path, &symbol, payload) {
        Ok(record) => record,
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "holding_create", "api", Some(&symbol), &err);
            return err_bad_request(err);
        }
    };

    let removed = (|| -> Result<usize, String> {
        let mut conn = open_db(db_path.as_ref()).map_err(|e| e.to_string())?;
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        let n = tx
            .execute("DELETE FROM watchlist_memberships WHERE symbol = ?1", params![symbol])
            .map_err(|e| e.to_string())?;
        tx.execute("DELETE FROM watchlist_symbols WHERE symbol = ?1", params![symbol])
            .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(n)
    })();

    match removed {
        Ok(n) => {
            let _ = insert_event_log(&db_path, "info", "holding_create", "api", Some(&record.symbol), &format!("Created holding id {} from watchlist ({} membership(s) removed)", record.id, n));
            HttpResponse::Ok().json(serde_json::json!({ "transaction": record, "removed_memberships": n }))
        }
        Err(err) => {
            // The holding exists — report the partial failure rather than lying
            let _ = insert_event_log(&db_path, "error", "holding_create", "api", Some(&record.symbol), &format!("Holding id {} created but watchlist cleanup failed: {}", record.id, err));
            HttpResponse::Ok().json(serde_json::json!({ "transaction": record, "removed_memberships": 0, "warning": format!("Holding recorded but watchlist cleanup failed: {}", err) }))
        }
    }
}

#[derive(Deserialize)]
struct WatchlistSymbolUpdate {
    lists: Vec<String>,
    notes: Option<String>,
    breakthrough_price: Option<f64>,
    stop_loss_price: Option<f64>,
    custom_fields: Option<std::collections::HashMap<String, String>>,
}

/// Set a watchlist symbol's list memberships, notes and fields in one
/// transactional call — replaces the parallel add/remove/update fan-out the
/// browser used to perform.
#[utoipa::path(put, path = "/api/v1/watchlist/symbol/{symbol}", tag = "watchlist", params(("symbol" = String, Path, description = "symbol")), responses((status = 200, description = "Update watchlist symbol lists")))]
#[put("/api/watchlist/symbol/{symbol}")]
async fn update_watchlist_symbol_lists(
    db_path: web::Data<PathBuf>,
    path: web::Path<String>,
    payload: web::Json<WatchlistSymbolUpdate>,
) -> impl Responder {
    let symbol = normalize_symbol(&path.into_inner());
    let payload = payload.into_inner();
    let lists: Vec<String> = payload
        .lists
        .iter()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    if lists.is_empty() {
        return err_bad_request("At least one list is required");
    }

    let result = (|| -> Result<(), String> {
        let mut conn = open_db(db_path.as_ref()).map_err(|e| e.to_string())?;
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        let now = Utc::now().to_rfc3339();
        tx.execute(
            "INSERT INTO watchlist_symbols (symbol, notes, breakthrough_price, stop_loss_price, updated_at) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(symbol) DO UPDATE SET notes = excluded.notes, breakthrough_price = excluded.breakthrough_price, stop_loss_price = excluded.stop_loss_price, updated_at = excluded.updated_at",
            params![symbol, payload.notes, payload.breakthrough_price, payload.stop_loss_price, now],
        )
        .map_err(|e| e.to_string())?;
        // Remove memberships no longer wanted
        let placeholders: Vec<String> = (0..lists.len()).map(|i| format!("?{}", i + 2)).collect();
        let sql = format!(
            "DELETE FROM watchlist_memberships WHERE symbol = ?1 AND list_name NOT IN ({})",
            placeholders.join(",")
        );
        let mut delete_params: Vec<&dyn rusqlite::ToSql> = vec![&symbol];
        for list in &lists {
            delete_params.push(list);
        }
        tx.execute(&sql, rusqlite::params_from_iter(delete_params))
            .map_err(|e| e.to_string())?;
        // Add missing memberships
        for list in &lists {
            tx.execute(
                "INSERT OR IGNORE INTO watchlist_memberships (symbol, list_name, added_at) VALUES (?1, ?2, ?3)",
                params![symbol, list, now],
            )
            .map_err(|e| e.to_string())?;
        }
        tx.commit().map_err(|e| e.to_string())
    })();
    if let Err(err) = result {
        let _ = insert_event_log(&db_path, "error", "watchlist_update", "api", Some(&symbol), &err);
        return err_internal(err);
    }

    // Merge custom fields after the membership transaction (same semantics as
    // the single-row update endpoint)
    if let Some(fields) = payload.custom_fields.as_ref() {
        if let Ok(conn) = open_db(db_path.as_ref()) {
            if let Err(err) = save_custom_fields(&conn, &symbol, fields) {
                let _ = insert_event_log(&db_path, "error", "watchlist_update", "api", Some(&symbol), &err);
                return err_internal(err);
            }
        }
    }

    match load_watchlist_symbols(&db_path, None) {
        Ok(rows) => {
            let rows: Vec<WatchlistSymbol> = rows.into_iter().filter(|r| r.symbol == symbol).collect();
            HttpResponse::Ok().json(rows)
        }
        Err(err) => err_internal(err),
    }
}

/// Unified transaction ledger: manual transactions merged with fetched
/// dividend events — deduped by (symbol, date), events filtered to on/after
/// the symbol's first purchase, sorted newest-first. Replaces the merge the
/// Transactions screen performed client-side.
#[utoipa::path(get, path = "/api/v1/transactions/ledger", tag = "transactions", responses((status = 200, description = "Get transactions ledger")))]
#[get("/api/transactions/ledger")]
async fn get_transactions_ledger(db_path: web::Data<PathBuf>) -> impl Responder {
    let txs = match fetch_holdings(&db_path) {
        Ok(t) => t,
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "ledger_fetch", "api", None, &err);
            return err_internal(err);
        }
    };

    let mut first_purchase: HashMap<String, String> = HashMap::new();
    let mut manual_dividend_keys: std::collections::HashSet<String> = std::collections::HashSet::new();
    for tx in &txs {
        if tx.transaction_type == "purchase" {
            match first_purchase.get(&tx.symbol) {
                Some(existing) if *existing <= tx.date => {}
                _ => {
                    first_purchase.insert(tx.symbol.clone(), tx.date.clone());
                }
            }
        }
        if tx.transaction_type == "dividend" {
            manual_dividend_keys.insert(format!("{}|{}", tx.symbol, tx.date));
        }
    }

    let mut rows: Vec<serde_json::Value> = txs
        .iter()
        .map(|tx| serde_json::json!({
            "key": format!("tx-{}", tx.id),
            "id": tx.id,
            "symbol": tx.symbol,
            "transaction_type": tx.transaction_type,
            "date": tx.date,
            "quantity": tx.quantity,
            "price": tx.price,
            "currency": tx.currency,
            "original_price": tx.original_price,
            "fx_rate": tx.fx_rate,
            "amount": tx.amount,
            "brokerage": tx.brokerage,
            "notes": tx.notes,
            "per_share": false,
            "payment_date": serde_json::Value::Null,
            "custom_fields": tx.custom_fields,
        }))
        .collect();

    if let Ok(conn) = open_db(db_path.as_ref()) {
        if let Ok(mut stmt) = conn.prepare("SELECT symbol, ex_date, payment_date, amount FROM dividend_events ORDER BY ex_date DESC") {
            if let Ok(event_rows) = stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, Option<String>>(2)?, row.get::<_, f64>(3)?))
            }) {
                for (symbol, ex_date, payment_date, amount) in event_rows.flatten() {
                    let Some(first) = first_purchase.get(&symbol) else { continue };
                    if ex_date < *first {
                        continue;
                    }
                    if manual_dividend_keys.contains(&format!("{}|{}", symbol, ex_date)) {
                        continue;
                    }
                    rows.push(serde_json::json!({
                        "key": format!("div-{}-{}", symbol, ex_date),
                        "id": serde_json::Value::Null,
                        "symbol": symbol,
                        "transaction_type": "dividend",
                        "date": ex_date,
                        "quantity": serde_json::Value::Null,
                        "price": serde_json::Value::Null,
                        "currency": "AUD",
                        "original_price": serde_json::Value::Null,
                        "fx_rate": serde_json::Value::Null,
                        "amount": amount,
                        "brokerage": serde_json::Value::Null,
                        "notes": serde_json::Value::Null,
                        "per_share": true,
                        "payment_date": payment_date,
                        "custom_fields": serde_json::json!({}),
                    }));
                }
            }
        }
    }

    rows.sort_by(|a, b| b["date"].as_str().unwrap_or("").cmp(a["date"].as_str().unwrap_or("")));
    HttpResponse::Ok().json(serde_json::json!({ "rows": rows }))
}

#[derive(Deserialize)]
struct RefreshQuery {
    force: Option<bool>,
}

/// One-call startup refresh: watchlist prices, holdings prices and dividends,
/// debounced server-side so a client opening repeatedly doesn't hammer Yahoo.
#[utoipa::path(post, path = "/api/v1/refresh", tag = "system", responses((status = 200, description = "Refresh all")))]
#[post("/api/refresh")]
async fn refresh_all(db_path: web::Data<PathBuf>, query: web::Query<RefreshQuery>) -> impl Responder {
    const DEBOUNCE_SECS: i64 = 600;
    if query.force != Some(true) {
        let last = load_config(&db_path)
            .ok()
            .and_then(|c| c.into_iter().find(|i| i.key == "last_full_refresh_at").map(|i| i.value));
        if let Some(last) = last {
            if let Ok(t) = chrono::DateTime::parse_from_rfc3339(&last) {
                if (Utc::now() - t.with_timezone(&Utc)).num_seconds() < DEBOUNCE_SECS {
                    return HttpResponse::Ok().json(serde_json::json!({ "skipped": true, "last_refreshed_at": last }));
                }
            }
        }
    }
    if let Err(err) = upsert_config(&db_path, "last_full_refresh_at", &Utc::now().to_rfc3339()) {
        let _ = insert_event_log(&db_path, "error", "refresh_all", "api", None, &err);
    }

    let mut errors: Vec<String> = Vec::new();

    let watchlist_count = match fetch_watchlist_current_prices(&db_path, None).await {
        Ok(prices) => {
            errors.extend(prices.iter().filter_map(|p| p.error.clone()));
            prices.len()
        }
        Err(err) => {
            errors.push(err);
            0
        }
    };

    let holding_symbols = load_holding_symbols(&db_path).unwrap_or_default();
    let holdings_count = if holding_symbols.is_empty() {
        0
    } else {
        match fetch_and_cache_current_prices(&db_path, &holding_symbols, "holdings_prices_updated_at").await {
            Ok(prices) => {
                errors.extend(prices.iter().filter_map(|p| p.error.clone()));
                prices.len()
            }
            Err(err) => {
                errors.push(err);
                0
            }
        }
    };

    let dividends = refresh_dividends_for_symbols(&db_path, holding_symbols).await;
    errors.extend(dividends.errors.clone());

    let _ = insert_event_log(&db_path, "info", "refresh_all", "api", None, &format!("Refreshed {} watchlist prices, {} holdings prices, dividends for {} symbols ({} error(s))", watchlist_count, holdings_count, dividends.updated, errors.len()));

    HttpResponse::Ok().json(serde_json::json!({
        "skipped": false,
        "watchlist_prices": watchlist_count,
        "holdings_prices": holdings_count,
        "dividends_updated": dividends.updated,
        "errors": errors,
    }))
}

#[derive(Deserialize)]
struct AnalysisMessage {
    role: String,
    content: String,
}

#[derive(Deserialize)]
struct AnalysisRequest {
    symbol: String,
    messages: Vec<AnalysisMessage>,
}

#[derive(Deserialize)]
struct AnalysisHistoryQuery {
    symbol: String,
}

#[derive(Serialize)]
struct AnalysisHistoryEntry {
    id: i64,
    role: String,
    content: String,
    model_used: Option<String>,
    created_at: String,
}

#[utoipa::path(get, path = "/api/v1/stock-analysis/history", tag = "analysis", responses((status = 200, description = "Get analysis history")))]
#[get("/api/stock-analysis/history")]
async fn get_analysis_history(db_path: web::Data<PathBuf>, query: web::Query<AnalysisHistoryQuery>) -> impl Responder {
    // Messages are stored under the normalized symbol — query the same way.
    let symbol = normalize_symbol(&query.symbol);
    let conn = match open_db(db_path.as_ref()) {
        Ok(c) => c,
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "analysis_history_fetch", "api", Some(&symbol), &err.to_string());
            return err_internal(err.to_string());
        }
    };
    let mut stmt = match conn.prepare(
        "SELECT id, role, content, model_used, created_at FROM stock_analysis_messages WHERE symbol = ?1 ORDER BY created_at ASC, id ASC"
    ) {
        Ok(s) => s,
        Err(err) => return err_internal(err.to_string()),
    };
    let rows = stmt.query_map(params![symbol], |row| {
        Ok(AnalysisHistoryEntry {
            id: row.get(0)?,
            role: row.get(1)?,
            content: row.get(2)?,
            model_used: row.get(3)?,
            created_at: row.get(4)?,
        })
    });
    match rows {
        Ok(mapped) => {
            let entries: Vec<AnalysisHistoryEntry> = mapped.filter_map(|r| r.ok()).collect();
            HttpResponse::Ok().json(entries)
        }
        Err(err) => err_internal(err.to_string()),
    }
}

#[utoipa::path(delete, path = "/api/v1/stock-analysis/history", tag = "analysis", responses((status = 200, description = "Delete analysis history")))]
#[delete("/api/stock-analysis/history")]
async fn delete_analysis_history(db_path: web::Data<PathBuf>, query: web::Query<AnalysisHistoryQuery>) -> impl Responder {
    let symbol = normalize_symbol(&query.symbol);
    let conn = match open_db(db_path.as_ref()) {
        Ok(c) => c,
        Err(err) => return err_internal(err.to_string()),
    };
    match conn.execute("DELETE FROM stock_analysis_messages WHERE symbol = ?1", params![symbol]) {
        Ok(_) => HttpResponse::NoContent().finish(),
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "analysis_history_delete", "api", Some(&symbol), &err.to_string());
            err_internal(err.to_string())
        }
    }
}

#[utoipa::path(post, path = "/api/v1/stock-analysis", tag = "analysis", responses((status = 200, description = "Post stock analysis")))]
#[post("/api/stock-analysis")]
async fn post_stock_analysis(
    db_path: web::Data<PathBuf>,
    payload: web::Json<AnalysisRequest>,
) -> impl Responder {
    let symbol = normalize_symbol(&payload.symbol);

    // Load AI config
    let config = match load_config(&db_path) {
        Ok(items) => items.into_iter().map(|c| (c.key, c.value)).collect::<HashMap<String, String>>(),
        Err(err) => return err_internal(format!("Failed to load config: {}", err)),
    };
    let provider = config.get("ai_provider").map(|s| s.as_str()).unwrap_or("anthropic");
    let api_key = match config.get("ai_api_key") {
        Some(k) if !k.is_empty() => k.clone(),
        _ => return err_bad_request("AI API key not configured. Set it in Configuration."),
    };
    let model = config.get("ai_model").map(|s| s.as_str()).unwrap_or("claude-sonnet-4-20250514").to_string();

    // Build local context for the system prompt
    let mut context_parts: Vec<String> = Vec::new();
    if let Ok(conn) = open_db(db_path.as_ref()) {
        // Cached price
        if let Ok(row) = conn.query_row(
            "SELECT price, change, change_percent, volume, price_date FROM cached_current_prices WHERE symbol = ?1",
            params![symbol],
            |row| Ok((
                row.get::<_, Option<f64>>(0)?,
                row.get::<_, Option<f64>>(1)?,
                row.get::<_, Option<f64>>(2)?,
                row.get::<_, Option<i64>>(3)?,
                row.get::<_, Option<String>>(4)?,
            )),
        ) {
            if let Some(price) = row.0 {
                let mut line = format!("Current price: ${:.2}", price);
                if let Some(chg) = row.1 { line.push_str(&format!(", change: {:.2}", chg)); }
                if let Some(pct) = row.2 { line.push_str(&format!(" ({:.2}%)", pct)); }
                if let Some(vol) = row.3 { line.push_str(&format!(", volume: {}", vol)); }
                if let Some(ref date) = row.4 { line.push_str(&format!(", as of {}", date)); }
                context_parts.push(line);
            }
        }
        // Symbol info
        if let Ok(info) = conn.query_row(
            "SELECT instrument_type, long_name, currency FROM symbol_info WHERE symbol = ?1",
            params![symbol],
            |row| Ok((
                row.get::<_, Option<String>>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
            )),
        ) {
            let mut parts = Vec::new();
            if let Some(ref name) = info.1 { parts.push(format!("Name: {}", name)); }
            if let Some(ref itype) = info.0 { parts.push(format!("Type: {}", itype)); }
            if let Some(ref cur) = info.2 { parts.push(format!("Currency: {}", cur)); }
            if !parts.is_empty() { context_parts.push(parts.join(", ")); }
        }
        // Built-in watchlist price fields
        if let Ok(row) = conn.query_row(
            "SELECT breakthrough_price, stop_loss_price FROM watchlist_symbols WHERE symbol = ?1",
            params![symbol],
            |row| Ok((row.get::<_, Option<f64>>(0)?, row.get::<_, Option<f64>>(1)?)),
        ) {
            let mut parts = Vec::new();
            if let Some(bp) = row.0 { parts.push(format!("Breakthrough Price: {:.2}", bp)); }
            if let Some(sl) = row.1 { parts.push(format!("Stop Loss Price: {:.2}", sl)); }
            if !parts.is_empty() { context_parts.push(parts.join(", ")); }
        }
        // Custom fields (watchlist + holdings)
        let mut fields_stmt = conn.prepare("SELECT field_key, value FROM watchlist_symbol_fields WHERE symbol = ?1").ok();
        if let Some(ref mut stmt) = fields_stmt {
            if let Ok(rows) = stmt.query_map(params![symbol], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))) {
                let fields: Vec<String> = rows.filter_map(|r| r.ok()).map(|(k, v)| format!("{}: {}", k, v)).collect();
                if !fields.is_empty() { context_parts.push(format!("Watchlist fields: {}", fields.join(", "))); }
            }
        }
        let mut hf_stmt = conn.prepare("SELECT field_key, value FROM holdings_symbol_fields WHERE symbol = ?1").ok();
        if let Some(ref mut stmt) = hf_stmt {
            if let Ok(rows) = stmt.query_map(params![symbol], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))) {
                let fields: Vec<String> = rows.filter_map(|r| r.ok()).map(|(k, v)| format!("{}: {}", k, v)).collect();
                if !fields.is_empty() { context_parts.push(format!("Holdings fields: {}", fields.join(", "))); }
            }
        }
    }

    let system_prompt = format!(
        "You are a stock market analyst. Analyze the stock {} using web search to find the latest news, analyst ratings, financial data, and technical analysis. \
         Provide a comprehensive but concise analysis covering: recent news, fundamental outlook, technical indicators, and a summary recommendation.\n\n\
         Local data from user's portfolio:\n{}",
        symbol,
        if context_parts.is_empty() { "No local data available.".to_string() } else { context_parts.join("\n") }
    );

    let client = match Client::builder().user_agent("stocks-api/1.0").build() {
        Ok(c) => c,
        Err(err) => return err_internal(err.to_string()),
    };

    // Save user message to history
    let now = Utc::now().to_rfc3339();
    if let Some(last_msg) = payload.messages.last() {
        if let Ok(conn) = open_db(db_path.as_ref()) {
            let _ = conn.execute(
                "INSERT INTO stock_analysis_messages (symbol, role, content, created_at) VALUES (?1, ?2, ?3, ?4)",
                params![symbol, last_msg.role, last_msg.content, now],
            );
        }
    }

    let result = if provider == "openai" {
        call_openai_api(&client, &api_key, &model, &system_prompt, &payload.messages).await
    } else {
        call_anthropic_api(&client, &api_key, &model, &system_prompt, &payload.messages).await
    };

    match result {
        Ok(response_text) => {
            // Save assistant response to history
            let now2 = Utc::now().to_rfc3339();
            if let Ok(conn) = open_db(db_path.as_ref()) {
                let _ = conn.execute(
                    "INSERT INTO stock_analysis_messages (symbol, role, content, model_used, created_at) VALUES (?1, 'assistant', ?2, ?3, ?4)",
                    params![symbol, response_text, model, now2],
                );
            }
            HttpResponse::Ok().json(serde_json::json!({ "role": "assistant", "content": response_text }))
        }
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "stock_analysis", "api", Some(&symbol), &err);
            err_internal(err)
        }
    }
}

async fn call_anthropic_api(
    client: &Client,
    api_key: &str,
    model: &str,
    system_prompt: &str,
    messages: &[AnalysisMessage],
) -> Result<String, String> {
    let api_messages: Vec<serde_json::Value> = messages.iter().map(|m| {
        serde_json::json!({ "role": m.role, "content": m.content })
    }).collect();

    let body = serde_json::json!({
        "model": model,
        "max_tokens": 4096,
        "system": system_prompt,
        "tools": [{ "type": "web_search_20250305", "name": "web_search", "max_uses": 5 }],
        "messages": api_messages,
    });

    let response = client
        .post("https://api.anthropic.com/v1/messages")
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("Anthropic API request failed: {}", e))?;

    let status = response.status();
    let response_text = response.text().await.map_err(|e| format!("Failed to read response: {}", e))?;

    if !status.is_success() {
        return Err(format!("Anthropic API error ({}): {}", status, response_text));
    }

    let data: serde_json::Value = serde_json::from_str(&response_text)
        .map_err(|e| format!("Failed to parse Anthropic response: {}", e))?;

    // Extract text from content blocks
    let mut result_text = String::new();
    if let Some(content) = data["content"].as_array() {
        for block in content {
            if block["type"] == "text" {
                if let Some(text) = block["text"].as_str() {
                    result_text.push_str(text);
                }
            }
        }
    }

    if result_text.is_empty() {
        Err(format!("No text in Anthropic response: {}", response_text))
    } else {
        Ok(result_text)
    }
}

async fn call_openai_api(
    client: &Client,
    api_key: &str,
    model: &str,
    system_prompt: &str,
    messages: &[AnalysisMessage],
) -> Result<String, String> {
    let mut api_messages = vec![serde_json::json!({ "role": "system", "content": system_prompt })];
    for m in messages {
        api_messages.push(serde_json::json!({ "role": m.role, "content": m.content }));
    }

    let body = serde_json::json!({
        "model": model,
        "messages": api_messages,
        "max_tokens": 4096,
    });

    let response = client
        .post("https://api.openai.com/v1/chat/completions")
        .header("Authorization", format!("Bearer {}", api_key))
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("OpenAI API request failed: {}", e))?;

    let status = response.status();
    let response_text = response.text().await.map_err(|e| format!("Failed to read response: {}", e))?;

    if !status.is_success() {
        return Err(format!("OpenAI API error ({}): {}", status, response_text));
    }

    let data: serde_json::Value = serde_json::from_str(&response_text)
        .map_err(|e| format!("Failed to parse OpenAI response: {}", e))?;

    data["choices"][0]["message"]["content"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| format!("No content in OpenAI response: {}", response_text))
}

// ---------------------------------------------------------------------------
// OpenAPI document — generated from the #[utoipa::path] annotations on every
// handler. Native clients (Swift/Kotlin) can be generated from this spec
// instead of hand-writing API layers.
// ---------------------------------------------------------------------------
struct SecurityAddon;

impl utoipa::Modify for SecurityAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme};
        let components = openapi.components.get_or_insert_with(Default::default);
        components.add_security_scheme(
            "bearer_token",
            SecurityScheme::Http(HttpBuilder::new().scheme(HttpAuthScheme::Bearer).build()),
        );
        openapi.security = Some(vec![utoipa::openapi::security::SecurityRequirement::new(
            "bearer_token",
            Vec::<String>::new(),
        )]);
    }
}

#[derive(utoipa::OpenApi)]
#[openapi(
    info(
        title = "Stocks API",
        version = "1.0.0",
        description = "Portfolio, watchlist and market-data API for the Stocks app. \
            All endpoints are served under /api/v1 (an alias of /api). \
            Every derived number — FIFO P/L, dividends attribution, FX conversion, \
            technical indicators — is computed server-side so all clients render \
            identical values. Errors use a consistent envelope: \
            {\"error\": {\"code\", \"message\"}}. Authentication is an optional \
            Bearer token (enabled when the server sets API_TOKEN); /api/v1/health \
            and /api/v1/openapi.json are exempt. Amounts are numeric (AUD unless \
            stated otherwise) and dates are ISO 8601 — formatting is left to clients."
    ),
    paths(
        health, get_meta, get_sync_state, openapi_spec,
        get_portfolio_holdings, get_portfolio_overview, get_portfolio_lots,
        get_portfolio_sold, get_portfolio_risk,
        get_watchlist, get_watchlist_lists, get_watchlist_enriched,
        add_watchlist_symbol, update_watchlist_symbol, delete_watchlist_symbol,
        rename_watchlist_list, update_watchlist_symbol_lists,
        get_watchlist_prices, get_watchlist_cached_prices,
        get_holdings, add_holding_transaction, update_holding_transaction,
        delete_holding_transaction, rename_holding_symbol,
        add_holding_from_watchlist, update_holdings_symbol_fields,
        get_holdings_symbol_fields, get_transactions_ledger,
        get_cached_prices, get_current_prices, get_price_history,
        get_symbol_info, get_fx_rate_for_date, get_fx_rates,
        get_dividends, refresh_dividends, refresh_sold_dividends, refresh_all,
        get_analysis_history, post_stock_analysis, delete_analysis_history,
        get_config, update_config, get_events,
    ),
    modifiers(&SecurityAddon)
)]
struct ApiDoc;

#[utoipa::path(get, path = "/api/v1/openapi.json", tag = "system", responses((status = 200, description = "This OpenAPI document")))]
#[get("/api/openapi.json")]
async fn openapi_spec() -> impl Responder {
    use utoipa::OpenApi as _;
    HttpResponse::Ok().json(ApiDoc::openapi())
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    let database_path = env::var("DATABASE_PATH").unwrap_or_else(|_| "stocks.db".to_string());
    let db_path = PathBuf::from(database_path);
    init_db(&db_path).map_err(|err| {
        eprintln!("Failed to initialize database: {err}");
        std::io::Error::new(std::io::ErrorKind::Other, err)
    })?;

    let host = env::var("API_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
    let port = env::var("API_PORT").ok().and_then(|value| value.parse::<u16>().ok()).unwrap_or(3001);
    let bind = format!("{host}:{port}");
    // Comma-separated list of origins allowed to call the API from a browser.
    let cors_origins = env::var("CORS_ALLOWED_ORIGINS")
        .unwrap_or_else(|_| "http://localhost:5173,http://127.0.0.1:5173".to_string());
    // Optional bearer-token authentication. CORS only protects browsers; any
    // native client (or curl) on the network can reach the API, so set
    // API_TOKEN before exposing the server beyond localhost.
    let api_token = env::var("API_TOKEN").ok().filter(|t| !t.is_empty());
    if api_token.is_some() {
        println!("API token authentication enabled");
    } else {
        println!("API token authentication disabled (set API_TOKEN to enable)");
    }

    println!("Starting stock API server at http://{bind}");

    HttpServer::new(move || {
        use actix_web::dev::Service as _;
        let mut cors = Cors::default()
            .allow_any_method()
            .allow_any_header()
            .max_age(3600);
        for origin in cors_origins.split(',').map(str::trim).filter(|o| !o.is_empty()) {
            cors = cors.allowed_origin(origin);
        }
        let token = api_token.clone();
        App::new()
            // Auth runs inside CORS so 401 responses still carry CORS headers.
            .wrap_fn(move |req, srv| {
                let authorized = match token.as_deref() {
                    None => true,
                    Some(expected) => {
                        req.method() == actix_web::http::Method::OPTIONS
                            || req.path() == "/api/health"
                            || req.path() == "/api/openapi.json"
                            || req
                                .headers()
                                .get("authorization")
                                .and_then(|h| h.to_str().ok())
                                .and_then(|h| h.strip_prefix("Bearer "))
                                .map(|t| t == expected)
                                .unwrap_or(false)
                    }
                };
                let fut = if authorized { Some(srv.call(req)) } else { None };
                async move {
                    match fut {
                        Some(f) => f.await,
                        None => Err(actix_web::error::InternalError::from_response(
                            "unauthorized",
                            api_error(actix_web::http::StatusCode::UNAUTHORIZED, "unauthorized", "Missing or invalid API token"),
                        )
                        .into()),
                    }
                }
            })
            // Versioned surface: /api/v1/* is an alias of /api/*. Native
            // clients pin the stable v1 prefix; breaking changes get a new
            // version. Runs before auth so exemptions see normalized paths.
            .wrap_fn(|mut req, srv| {
                if let Some(rest) = req.path().strip_prefix("/api/v1/").map(str::to_owned) {
                    let path_and_query = if req.query_string().is_empty() {
                        format!("/api/{}", rest)
                    } else {
                        format!("/api/{}?{}", rest, req.query_string())
                    };
                    if let Ok(uri) = path_and_query.parse::<actix_web::http::Uri>() {
                        req.head_mut().uri = uri.clone();
                        // The router matches against the cached path object,
                        // not head.uri — update both (as NormalizePath does).
                        req.match_info_mut().get_mut().update(&uri);
                    }
                }
                srv.call(req)
            })
            .wrap(cors)
            .app_data(web::Data::new(db_path.clone()))
            .service(health)
            .service(get_watchlist)
            .service(get_watchlist_lists)
            .service(rename_watchlist_list)
            .service(add_watchlist_symbol)
            .service(update_watchlist_symbol)
            .service(delete_watchlist_symbol)
            .service(get_config)
            .service(update_config)
            .service(get_watchlist_cached_prices)
            .service(get_watchlist_prices)
            .service(get_cached_prices)
            .service(get_current_prices)
            .service(rename_holding_symbol)
            .service(add_holding_from_watchlist)
            .service(update_watchlist_symbol_lists)
            .service(get_watchlist_enriched)
            .service(get_transactions_ledger)
            .service(refresh_all)
            .service(update_holdings_symbol_fields)
            .service(get_holdings_symbol_fields)
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
            .service(get_analysis_history)
            .service(post_stock_analysis)
            .service(delete_analysis_history)
            .service(get_portfolio_holdings)
            .service(get_portfolio_overview)
            .service(get_portfolio_lots)
            .service(get_portfolio_sold)
            .service(get_portfolio_risk)
            .service(get_meta)
            .service(get_sync_state)
            .service(openapi_spec)
    })
    .bind(bind)?
    .run()
    .await
}

fn init_db(path: &PathBuf) -> Result<(), String> {
    let conn = open_db(path).map_err(|err| err.to_string())?;
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

    // cached_current_prices: stores the most recent fetched price per symbol
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS cached_current_prices (
            symbol TEXT PRIMARY KEY,
            price REAL,
            change REAL,
            change_percent REAL,
            volume INTEGER,
            last_updated TEXT NOT NULL,
            price_date TEXT
        );",
    )
    .map_err(|err| err.to_string())?;

    // Seed cache from historical prices for any symbol not yet cached
    conn.execute_batch(
        "INSERT OR IGNORE INTO cached_current_prices (symbol, price, change, change_percent, volume, last_updated, price_date)
         SELECT p.symbol, p.close, NULL, NULL, p.volume, p.fetched_at, p.date
         FROM prices p
         INNER JOIN (SELECT symbol, MAX(date) as max_date FROM prices WHERE close IS NOT NULL GROUP BY symbol) latest
         ON p.symbol = latest.symbol AND p.date = latest.max_date
         WHERE p.symbol NOT IN (SELECT symbol FROM cached_current_prices);"
    ).map_err(|err| err.to_string())?;

    // Seed the sector list used by /api/meta and the sector dropdowns
    conn.execute(
        "INSERT OR IGNORE INTO app_config (key, value) VALUES ('sectors', ?1)",
        params![DEFAULT_SECTORS_JSON],
    )
    .map_err(|err| err.to_string())?;

    // holdings_custom_fields: per-transaction values for user-defined custom fields
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS holdings_custom_fields (
            transaction_id INTEGER NOT NULL,
            field_key TEXT NOT NULL,
            value TEXT NOT NULL,
            PRIMARY KEY (transaction_id, field_key)
        );",
    )
    .map_err(|err| err.to_string())?;

    // holdings_symbol_fields: per-symbol master values for user-defined custom fields
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS holdings_symbol_fields (
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
    add_column_if_missing(&conn, "watchlist_symbols", "breakthrough_price", "REAL")?;
    add_column_if_missing(&conn, "watchlist_symbols", "stop_loss_price", "REAL")?;

    // Migrate breakthrough_price and stop_loss_price from watchlist_symbol_fields to dedicated columns
    conn.execute_batch(
        "UPDATE watchlist_symbols SET breakthrough_price = CAST((SELECT value FROM watchlist_symbol_fields WHERE watchlist_symbol_fields.symbol = watchlist_symbols.symbol AND field_key = 'breakthrough_price') AS REAL) WHERE breakthrough_price IS NULL AND EXISTS (SELECT 1 FROM watchlist_symbol_fields WHERE watchlist_symbol_fields.symbol = watchlist_symbols.symbol AND field_key = 'breakthrough_price');
         UPDATE watchlist_symbols SET stop_loss_price = CAST((SELECT value FROM watchlist_symbol_fields WHERE watchlist_symbol_fields.symbol = watchlist_symbols.symbol AND field_key = 'stop_loss_price') AS REAL) WHERE stop_loss_price IS NULL AND EXISTS (SELECT 1 FROM watchlist_symbol_fields WHERE watchlist_symbol_fields.symbol = watchlist_symbols.symbol AND field_key = 'stop_loss_price');
         DELETE FROM watchlist_symbol_fields WHERE field_key IN ('breakthrough_price', 'stop_loss_price');"
    ).map_err(|err| err.to_string())?;

    // Audit table: records every change to any tracked table
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS audit_log (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            timestamp TEXT NOT NULL,
            table_name TEXT NOT NULL,
            action TEXT NOT NULL,
            row_id TEXT,
            old_values TEXT,
            new_values TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_audit_log_timestamp ON audit_log(timestamp);
        CREATE INDEX IF NOT EXISTS idx_audit_log_table ON audit_log(table_name, timestamp);",
    )
    .map_err(|err| err.to_string())?;

    // Create triggers for all tracked tables.
    // Each trigger captures old/new values as JSON.
    let trigger_sql = "
        -- watchlist_symbols
        CREATE TRIGGER IF NOT EXISTS audit_watchlist_symbols_insert AFTER INSERT ON watchlist_symbols
        BEGIN
            INSERT INTO audit_log (timestamp, table_name, action, row_id, old_values, new_values)
            VALUES (strftime('%Y-%m-%dT%H:%M:%fZ','now'), 'watchlist_symbols', 'INSERT', NEW.symbol, NULL,
                json_object('symbol', NEW.symbol, 'notes', NEW.notes, 'updated_at', NEW.updated_at));
        END;
        CREATE TRIGGER IF NOT EXISTS audit_watchlist_symbols_update AFTER UPDATE ON watchlist_symbols
        BEGIN
            INSERT INTO audit_log (timestamp, table_name, action, row_id, old_values, new_values)
            VALUES (strftime('%Y-%m-%dT%H:%M:%fZ','now'), 'watchlist_symbols', 'UPDATE', NEW.symbol,
                json_object('symbol', OLD.symbol, 'notes', OLD.notes, 'updated_at', OLD.updated_at),
                json_object('symbol', NEW.symbol, 'notes', NEW.notes, 'updated_at', NEW.updated_at));
        END;
        CREATE TRIGGER IF NOT EXISTS audit_watchlist_symbols_delete AFTER DELETE ON watchlist_symbols
        BEGIN
            INSERT INTO audit_log (timestamp, table_name, action, row_id, old_values, new_values)
            VALUES (strftime('%Y-%m-%dT%H:%M:%fZ','now'), 'watchlist_symbols', 'DELETE', OLD.symbol,
                json_object('symbol', OLD.symbol, 'notes', OLD.notes, 'updated_at', OLD.updated_at), NULL);
        END;

        -- watchlist_memberships
        CREATE TRIGGER IF NOT EXISTS audit_watchlist_memberships_insert AFTER INSERT ON watchlist_memberships
        BEGIN
            INSERT INTO audit_log (timestamp, table_name, action, row_id, old_values, new_values)
            VALUES (strftime('%Y-%m-%dT%H:%M:%fZ','now'), 'watchlist_memberships', 'INSERT', CAST(NEW.id AS TEXT), NULL,
                json_object('id', NEW.id, 'symbol', NEW.symbol, 'list_name', NEW.list_name, 'added_at', NEW.added_at));
        END;
        CREATE TRIGGER IF NOT EXISTS audit_watchlist_memberships_update AFTER UPDATE ON watchlist_memberships
        BEGIN
            INSERT INTO audit_log (timestamp, table_name, action, row_id, old_values, new_values)
            VALUES (strftime('%Y-%m-%dT%H:%M:%fZ','now'), 'watchlist_memberships', 'UPDATE', CAST(NEW.id AS TEXT),
                json_object('id', OLD.id, 'symbol', OLD.symbol, 'list_name', OLD.list_name, 'added_at', OLD.added_at),
                json_object('id', NEW.id, 'symbol', NEW.symbol, 'list_name', NEW.list_name, 'added_at', NEW.added_at));
        END;
        CREATE TRIGGER IF NOT EXISTS audit_watchlist_memberships_delete AFTER DELETE ON watchlist_memberships
        BEGIN
            INSERT INTO audit_log (timestamp, table_name, action, row_id, old_values, new_values)
            VALUES (strftime('%Y-%m-%dT%H:%M:%fZ','now'), 'watchlist_memberships', 'DELETE', CAST(OLD.id AS TEXT),
                json_object('id', OLD.id, 'symbol', OLD.symbol, 'list_name', OLD.list_name, 'added_at', OLD.added_at), NULL);
        END;

        -- watchlist_symbol_fields
        CREATE TRIGGER IF NOT EXISTS audit_watchlist_symbol_fields_insert AFTER INSERT ON watchlist_symbol_fields
        BEGIN
            INSERT INTO audit_log (timestamp, table_name, action, row_id, old_values, new_values)
            VALUES (strftime('%Y-%m-%dT%H:%M:%fZ','now'), 'watchlist_symbol_fields', 'INSERT', NEW.symbol || ':' || NEW.field_key, NULL,
                json_object('symbol', NEW.symbol, 'field_key', NEW.field_key, 'value', NEW.value));
        END;
        CREATE TRIGGER IF NOT EXISTS audit_watchlist_symbol_fields_update AFTER UPDATE ON watchlist_symbol_fields
        BEGIN
            INSERT INTO audit_log (timestamp, table_name, action, row_id, old_values, new_values)
            VALUES (strftime('%Y-%m-%dT%H:%M:%fZ','now'), 'watchlist_symbol_fields', 'UPDATE', NEW.symbol || ':' || NEW.field_key,
                json_object('symbol', OLD.symbol, 'field_key', OLD.field_key, 'value', OLD.value),
                json_object('symbol', NEW.symbol, 'field_key', NEW.field_key, 'value', NEW.value));
        END;
        CREATE TRIGGER IF NOT EXISTS audit_watchlist_symbol_fields_delete AFTER DELETE ON watchlist_symbol_fields
        BEGIN
            INSERT INTO audit_log (timestamp, table_name, action, row_id, old_values, new_values)
            VALUES (strftime('%Y-%m-%dT%H:%M:%fZ','now'), 'watchlist_symbol_fields', 'DELETE', OLD.symbol || ':' || OLD.field_key,
                json_object('symbol', OLD.symbol, 'field_key', OLD.field_key, 'value', OLD.value), NULL);
        END;

        -- holdings_transactions
        CREATE TRIGGER IF NOT EXISTS audit_holdings_transactions_insert AFTER INSERT ON holdings_transactions
        BEGIN
            INSERT INTO audit_log (timestamp, table_name, action, row_id, old_values, new_values)
            VALUES (strftime('%Y-%m-%dT%H:%M:%fZ','now'), 'holdings_transactions', 'INSERT', CAST(NEW.id AS TEXT), NULL,
                json_object('id', NEW.id, 'symbol', NEW.symbol, 'transaction_type', NEW.transaction_type, 'date', NEW.date,
                    'quantity', NEW.quantity, 'price', NEW.price, 'amount', NEW.amount, 'brokerage', NEW.brokerage,
                    'notes', NEW.notes, 'currency', NEW.currency, 'original_price', NEW.original_price, 'fx_rate', NEW.fx_rate));
        END;
        CREATE TRIGGER IF NOT EXISTS audit_holdings_transactions_update AFTER UPDATE ON holdings_transactions
        BEGIN
            INSERT INTO audit_log (timestamp, table_name, action, row_id, old_values, new_values)
            VALUES (strftime('%Y-%m-%dT%H:%M:%fZ','now'), 'holdings_transactions', 'UPDATE', CAST(NEW.id AS TEXT),
                json_object('id', OLD.id, 'symbol', OLD.symbol, 'transaction_type', OLD.transaction_type, 'date', OLD.date,
                    'quantity', OLD.quantity, 'price', OLD.price, 'amount', OLD.amount, 'brokerage', OLD.brokerage,
                    'notes', OLD.notes, 'currency', OLD.currency, 'original_price', OLD.original_price, 'fx_rate', OLD.fx_rate),
                json_object('id', NEW.id, 'symbol', NEW.symbol, 'transaction_type', NEW.transaction_type, 'date', NEW.date,
                    'quantity', NEW.quantity, 'price', NEW.price, 'amount', NEW.amount, 'brokerage', NEW.brokerage,
                    'notes', NEW.notes, 'currency', NEW.currency, 'original_price', NEW.original_price, 'fx_rate', NEW.fx_rate));
        END;
        CREATE TRIGGER IF NOT EXISTS audit_holdings_transactions_delete AFTER DELETE ON holdings_transactions
        BEGIN
            INSERT INTO audit_log (timestamp, table_name, action, row_id, old_values, new_values)
            VALUES (strftime('%Y-%m-%dT%H:%M:%fZ','now'), 'holdings_transactions', 'DELETE', CAST(OLD.id AS TEXT),
                json_object('id', OLD.id, 'symbol', OLD.symbol, 'transaction_type', OLD.transaction_type, 'date', OLD.date,
                    'quantity', OLD.quantity, 'price', OLD.price, 'amount', OLD.amount, 'brokerage', OLD.brokerage,
                    'notes', OLD.notes, 'currency', OLD.currency, 'original_price', OLD.original_price, 'fx_rate', OLD.fx_rate), NULL);
        END;

        -- holdings_custom_fields
        CREATE TRIGGER IF NOT EXISTS audit_holdings_custom_fields_insert AFTER INSERT ON holdings_custom_fields
        BEGIN
            INSERT INTO audit_log (timestamp, table_name, action, row_id, old_values, new_values)
            VALUES (strftime('%Y-%m-%dT%H:%M:%fZ','now'), 'holdings_custom_fields', 'INSERT', CAST(NEW.transaction_id AS TEXT) || ':' || NEW.field_key, NULL,
                json_object('transaction_id', NEW.transaction_id, 'field_key', NEW.field_key, 'value', NEW.value));
        END;
        CREATE TRIGGER IF NOT EXISTS audit_holdings_custom_fields_update AFTER UPDATE ON holdings_custom_fields
        BEGIN
            INSERT INTO audit_log (timestamp, table_name, action, row_id, old_values, new_values)
            VALUES (strftime('%Y-%m-%dT%H:%M:%fZ','now'), 'holdings_custom_fields', 'UPDATE', CAST(NEW.transaction_id AS TEXT) || ':' || NEW.field_key,
                json_object('transaction_id', OLD.transaction_id, 'field_key', OLD.field_key, 'value', OLD.value),
                json_object('transaction_id', NEW.transaction_id, 'field_key', NEW.field_key, 'value', NEW.value));
        END;
        CREATE TRIGGER IF NOT EXISTS audit_holdings_custom_fields_delete AFTER DELETE ON holdings_custom_fields
        BEGIN
            INSERT INTO audit_log (timestamp, table_name, action, row_id, old_values, new_values)
            VALUES (strftime('%Y-%m-%dT%H:%M:%fZ','now'), 'holdings_custom_fields', 'DELETE', CAST(OLD.transaction_id AS TEXT) || ':' || OLD.field_key,
                json_object('transaction_id', OLD.transaction_id, 'field_key', OLD.field_key, 'value', OLD.value), NULL);
        END;

        -- holdings_symbol_fields
        CREATE TRIGGER IF NOT EXISTS audit_holdings_symbol_fields_insert AFTER INSERT ON holdings_symbol_fields
        BEGIN
            INSERT INTO audit_log (timestamp, table_name, action, row_id, old_values, new_values)
            VALUES (strftime('%Y-%m-%dT%H:%M:%fZ','now'), 'holdings_symbol_fields', 'INSERT', NEW.symbol || ':' || NEW.field_key, NULL,
                json_object('symbol', NEW.symbol, 'field_key', NEW.field_key, 'value', NEW.value));
        END;
        CREATE TRIGGER IF NOT EXISTS audit_holdings_symbol_fields_update AFTER UPDATE ON holdings_symbol_fields
        BEGIN
            INSERT INTO audit_log (timestamp, table_name, action, row_id, old_values, new_values)
            VALUES (strftime('%Y-%m-%dT%H:%M:%fZ','now'), 'holdings_symbol_fields', 'UPDATE', NEW.symbol || ':' || NEW.field_key,
                json_object('symbol', OLD.symbol, 'field_key', OLD.field_key, 'value', OLD.value),
                json_object('symbol', NEW.symbol, 'field_key', NEW.field_key, 'value', NEW.value));
        END;
        CREATE TRIGGER IF NOT EXISTS audit_holdings_symbol_fields_delete AFTER DELETE ON holdings_symbol_fields
        BEGIN
            INSERT INTO audit_log (timestamp, table_name, action, row_id, old_values, new_values)
            VALUES (strftime('%Y-%m-%dT%H:%M:%fZ','now'), 'holdings_symbol_fields', 'DELETE', OLD.symbol || ':' || OLD.field_key,
                json_object('symbol', OLD.symbol, 'field_key', OLD.field_key, 'value', OLD.value), NULL);
        END;

        -- app_config
        CREATE TRIGGER IF NOT EXISTS audit_app_config_insert AFTER INSERT ON app_config
        BEGIN
            INSERT INTO audit_log (timestamp, table_name, action, row_id, old_values, new_values)
            VALUES (strftime('%Y-%m-%dT%H:%M:%fZ','now'), 'app_config', 'INSERT', NEW.key, NULL,
                json_object('key', NEW.key, 'value', NEW.value));
        END;
        CREATE TRIGGER IF NOT EXISTS audit_app_config_update AFTER UPDATE ON app_config
        BEGIN
            INSERT INTO audit_log (timestamp, table_name, action, row_id, old_values, new_values)
            VALUES (strftime('%Y-%m-%dT%H:%M:%fZ','now'), 'app_config', 'UPDATE', NEW.key,
                json_object('key', OLD.key, 'value', OLD.value),
                json_object('key', NEW.key, 'value', NEW.value));
        END;
        CREATE TRIGGER IF NOT EXISTS audit_app_config_delete AFTER DELETE ON app_config
        BEGIN
            INSERT INTO audit_log (timestamp, table_name, action, row_id, old_values, new_values)
            VALUES (strftime('%Y-%m-%dT%H:%M:%fZ','now'), 'app_config', 'DELETE', OLD.key,
                json_object('key', OLD.key, 'value', OLD.value), NULL);
        END;

        -- dividend_events
        CREATE TRIGGER IF NOT EXISTS audit_dividend_events_insert AFTER INSERT ON dividend_events
        BEGIN
            INSERT INTO audit_log (timestamp, table_name, action, row_id, old_values, new_values)
            VALUES (strftime('%Y-%m-%dT%H:%M:%fZ','now'), 'dividend_events', 'INSERT', CAST(NEW.id AS TEXT), NULL,
                json_object('id', NEW.id, 'symbol', NEW.symbol, 'ex_date', NEW.ex_date, 'amount', NEW.amount));
        END;
        CREATE TRIGGER IF NOT EXISTS audit_dividend_events_update AFTER UPDATE ON dividend_events
        BEGIN
            INSERT INTO audit_log (timestamp, table_name, action, row_id, old_values, new_values)
            VALUES (strftime('%Y-%m-%dT%H:%M:%fZ','now'), 'dividend_events', 'UPDATE', CAST(NEW.id AS TEXT),
                json_object('id', OLD.id, 'symbol', OLD.symbol, 'ex_date', OLD.ex_date, 'amount', OLD.amount),
                json_object('id', NEW.id, 'symbol', NEW.symbol, 'ex_date', NEW.ex_date, 'amount', NEW.amount));
        END;
        CREATE TRIGGER IF NOT EXISTS audit_dividend_events_delete AFTER DELETE ON dividend_events
        BEGIN
            INSERT INTO audit_log (timestamp, table_name, action, row_id, old_values, new_values)
            VALUES (strftime('%Y-%m-%dT%H:%M:%fZ','now'), 'dividend_events', 'DELETE', CAST(OLD.id AS TEXT),
                json_object('id', OLD.id, 'symbol', OLD.symbol, 'ex_date', OLD.ex_date, 'amount', OLD.amount), NULL);
        END;

        -- symbol_info
        CREATE TRIGGER IF NOT EXISTS audit_symbol_info_insert AFTER INSERT ON symbol_info
        BEGIN
            INSERT INTO audit_log (timestamp, table_name, action, row_id, old_values, new_values)
            VALUES (strftime('%Y-%m-%dT%H:%M:%fZ','now'), 'symbol_info', 'INSERT', NEW.symbol, NULL,
                json_object('symbol', NEW.symbol, 'instrument_type', NEW.instrument_type, 'long_name', NEW.long_name, 'currency', NEW.currency));
        END;
        CREATE TRIGGER IF NOT EXISTS audit_symbol_info_update AFTER UPDATE ON symbol_info
        BEGIN
            INSERT INTO audit_log (timestamp, table_name, action, row_id, old_values, new_values)
            VALUES (strftime('%Y-%m-%dT%H:%M:%fZ','now'), 'symbol_info', 'UPDATE', NEW.symbol,
                json_object('symbol', OLD.symbol, 'instrument_type', OLD.instrument_type, 'long_name', OLD.long_name, 'currency', OLD.currency),
                json_object('symbol', NEW.symbol, 'instrument_type', NEW.instrument_type, 'long_name', NEW.long_name, 'currency', NEW.currency));
        END;
        CREATE TRIGGER IF NOT EXISTS audit_symbol_info_delete AFTER DELETE ON symbol_info
        BEGIN
            INSERT INTO audit_log (timestamp, table_name, action, row_id, old_values, new_values)
            VALUES (strftime('%Y-%m-%dT%H:%M:%fZ','now'), 'symbol_info', 'DELETE', OLD.symbol,
                json_object('symbol', OLD.symbol, 'instrument_type', OLD.instrument_type, 'long_name', OLD.long_name, 'currency', OLD.currency), NULL);
        END;
    ";
    conn.execute_batch(trigger_sql).map_err(|err| err.to_string())?;

    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS stock_analysis_messages (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            symbol TEXT NOT NULL,
            role TEXT NOT NULL,
            content TEXT NOT NULL,
            model_used TEXT,
            created_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_analysis_messages_symbol ON stock_analysis_messages(symbol, created_at);",
    ).map_err(|err| err.to_string())?;

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

/// Merge semantics: an empty value deletes that key, a non-empty value upserts it,
/// and keys not present in `fields` are left untouched. This prevents callers that
/// send a partial map (e.g. adding an existing symbol to a second list) from
/// wiping fields they didn't mention.
fn save_custom_fields(conn: &Connection, symbol: &str, fields: &std::collections::HashMap<String, String>) -> Result<(), String> {
    for (key, value) in fields {
        if value.trim().is_empty() {
            conn.execute(
                "DELETE FROM watchlist_symbol_fields WHERE symbol = ?1 AND field_key = ?2",
                params![symbol, key],
            ).map_err(|e| e.to_string())?;
        } else {
            conn.execute(
                "INSERT OR REPLACE INTO watchlist_symbol_fields (symbol, field_key, value) VALUES (?1, ?2, ?3)",
                params![symbol, key, value.trim()],
            ).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

fn load_watchlist_symbols(db_path: &PathBuf, list: Option<&str>) -> Result<Vec<WatchlistSymbol>, String> {
    let conn = open_db(db_path).map_err(|err| err.to_string())?;
    let mut rows: Vec<WatchlistSymbol> = if let Some(list_name) = list {
        let mut stmt = conn
            .prepare(
                "SELECT wm.id, ws.symbol, wm.list_name, wm.added_at, ws.notes, ws.breakthrough_price, ws.stop_loss_price
                 FROM watchlist_memberships wm
                 JOIN watchlist_symbols ws ON wm.symbol = ws.symbol
                 WHERE wm.list_name = ?1 ORDER BY ws.symbol",
            )
            .map_err(|err| err.to_string())?;
        stmt.query_map(params![list_name], |row| {
            Ok(WatchlistSymbol { id: row.get(0)?, symbol: row.get(1)?, list_name: row.get(2)?, added_at: row.get(3)?, notes: row.get(4)?, breakthrough_price: row.get(5)?, stop_loss_price: row.get(6)?, custom_fields: Default::default() })
        })
        .map_err(|err| err.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| err.to_string())?
    } else {
        let mut stmt = conn
            .prepare(
                "SELECT wm.id, ws.symbol, wm.list_name, wm.added_at, ws.notes, ws.breakthrough_price, ws.stop_loss_price
                 FROM watchlist_memberships wm
                 JOIN watchlist_symbols ws ON wm.symbol = ws.symbol
                 ORDER BY wm.list_name, ws.symbol",
            )
            .map_err(|err| err.to_string())?;
        stmt.query_map([], |row| {
            Ok(WatchlistSymbol { id: row.get(0)?, symbol: row.get(1)?, list_name: row.get(2)?, added_at: row.get(3)?, notes: row.get(4)?, breakthrough_price: row.get(5)?, stop_loss_price: row.get(6)?, custom_fields: Default::default() })
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
    let conn = open_db(db_path).map_err(|err| err.to_string())?;
    let mut stmt = conn
        .prepare("SELECT DISTINCT list_name FROM watchlist_memberships ORDER BY list_name")
        .map_err(|err| err.to_string())?;
    let lists: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|err| err.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| err.to_string())?;
    Ok(lists)
}

fn insert_watchlist_symbol(db_path: &PathBuf, symbol: &str, list_name: &str, notes: Option<&str>, breakthrough_price: Option<f64>, stop_loss_price: Option<f64>, custom_fields: Option<&std::collections::HashMap<String, String>>) -> Result<WatchlistSymbol, String> {
    let conn = open_db(db_path).map_err(|err| err.to_string())?;
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO watchlist_symbols (symbol, notes, breakthrough_price, stop_loss_price, updated_at) VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(symbol) DO UPDATE SET notes = COALESCE(excluded.notes, notes), breakthrough_price = COALESCE(excluded.breakthrough_price, breakthrough_price), stop_loss_price = COALESCE(excluded.stop_loss_price, stop_loss_price), updated_at = excluded.updated_at",
        params![symbol, notes, breakthrough_price, stop_loss_price, now],
    ).map_err(|err| err.to_string())?;
    if let Some(fields) = custom_fields {
        save_custom_fields(&conn, symbol, fields)?;
    }
    conn.execute(
        "INSERT OR IGNORE INTO watchlist_memberships (symbol, list_name, added_at) VALUES (?1, ?2, ?3)",
        params![symbol, list_name, now],
    ).map_err(|err| err.to_string())?;

    let mut stmt = conn
        .prepare(
            "SELECT wm.id, ws.symbol, wm.list_name, wm.added_at, ws.notes, ws.breakthrough_price, ws.stop_loss_price
             FROM watchlist_memberships wm
             JOIN watchlist_symbols ws ON wm.symbol = ws.symbol
             WHERE wm.symbol = ?1 AND wm.list_name = ?2",
        )
        .map_err(|err| err.to_string())?;
    let mut rows = stmt.query_map(params![symbol, list_name], |row| {
        Ok(WatchlistSymbol { id: row.get(0)?, symbol: row.get(1)?, list_name: row.get(2)?, added_at: row.get(3)?, notes: row.get(4)?, breakthrough_price: row.get(5)?, stop_loss_price: row.get(6)?, custom_fields: Default::default() })
    }).map_err(|err| err.to_string())?;
    let mut result = rows.next()
        .transpose()
        .map_err(|err| err.to_string())?
        .ok_or_else(|| "Failed to load inserted symbol".to_string())?;
    result.custom_fields = load_custom_fields(&conn, &result.symbol);
    Ok(result)
}

fn update_watchlist_symbol_notes(db_path: &PathBuf, id: i64, notes: Option<Option<String>>, breakthrough_price: Option<Option<f64>>, stop_loss_price: Option<Option<f64>>, custom_fields: Option<&std::collections::HashMap<String, String>>) -> Result<WatchlistSymbol, String> {
    let conn = open_db(db_path).map_err(|err| err.to_string())?;
    let symbol: String = conn
        .query_row("SELECT symbol FROM watchlist_memberships WHERE id = ?1", params![id], |row| row.get(0))
        .map_err(|_| format!("Membership id {} not found", id))?;
    // Fields absent from the payload keep their current value; explicit nulls clear it.
    let (cur_notes, cur_bp, cur_sl): (Option<String>, Option<f64>, Option<f64>) = conn
        .query_row(
            "SELECT notes, breakthrough_price, stop_loss_price FROM watchlist_symbols WHERE symbol = ?1",
            params![symbol],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(|err| err.to_string())?;
    let new_notes = notes.unwrap_or(cur_notes);
    let new_bp = breakthrough_price.unwrap_or(cur_bp);
    let new_sl = stop_loss_price.unwrap_or(cur_sl);
    conn.execute(
        "UPDATE watchlist_symbols SET notes = ?1, breakthrough_price = ?2, stop_loss_price = ?3 WHERE symbol = ?4",
        params![new_notes, new_bp, new_sl, symbol],
    ).map_err(|err| err.to_string())?;
    if let Some(fields) = custom_fields {
        save_custom_fields(&conn, &symbol, fields)?;
    }
    let mut stmt = conn
        .prepare(
            "SELECT wm.id, ws.symbol, wm.list_name, wm.added_at, ws.notes, ws.breakthrough_price, ws.stop_loss_price
             FROM watchlist_memberships wm
             JOIN watchlist_symbols ws ON wm.symbol = ws.symbol
             WHERE wm.id = ?1",
        )
        .map_err(|err| err.to_string())?;
    let mut rows = stmt.query_map(params![id], |row| {
        Ok(WatchlistSymbol { id: row.get(0)?, symbol: row.get(1)?, list_name: row.get(2)?, added_at: row.get(3)?, notes: row.get(4)?, breakthrough_price: row.get(5)?, stop_loss_price: row.get(6)?, custom_fields: Default::default() })
    }).map_err(|err| err.to_string())?;
    let mut result = rows.next()
        .transpose()
        .map_err(|err| err.to_string())?
        .ok_or_else(|| "Symbol not found after update".to_string())?;
    result.custom_fields = load_custom_fields(&conn, &result.symbol);
    Ok(result)
}

fn remove_watchlist_symbol(db_path: &PathBuf, id: i64) -> Result<bool, String> {
    let conn = open_db(db_path).map_err(|err| err.to_string())?;
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
    let conn = open_db(db_path).map_err(|err| err.to_string())?;
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
                custom_fields: std::collections::HashMap::new(),
            })
        })
        .map_err(|err| err.to_string())?;

    let mut transactions = rows.collect::<Result<Vec<_>, _>>()
        .map_err(|err| err.to_string())?;

    // Load custom fields for all transactions
    {
        let mut cf_stmt = conn
            .prepare("SELECT transaction_id, field_key, value FROM holdings_custom_fields")
            .map_err(|err| err.to_string())?;
        let cf_rows = cf_stmt
            .query_map([], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?))
            })
            .map_err(|err| err.to_string())?;
        let mut cf_map: std::collections::HashMap<i64, std::collections::HashMap<String, String>> = std::collections::HashMap::new();
        for row in cf_rows {
            let (tid, key, val) = row.map_err(|err| err.to_string())?;
            cf_map.entry(tid).or_default().insert(key, val);
        }
        for tx in &mut transactions {
            if let Some(fields) = cf_map.remove(&tx.id) {
                tx.custom_fields = fields;
            }
        }
    }

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
    let conn = open_db(db_path).map_err(|err| err.to_string())?;
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

    let conn = open_db(db_path).map_err(|err| err.to_string())?;
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

    // Save custom fields (per-transaction and per-symbol)
    if let Some(ref fields) = transaction.custom_fields {
        for (key, value) in fields {
            if !value.is_empty() {
                conn.execute(
                    "INSERT OR REPLACE INTO holdings_custom_fields (transaction_id, field_key, value) VALUES (?1, ?2, ?3)",
                    params![id, key, value],
                ).map_err(|err| err.to_string())?;
            }
        }
        upsert_holdings_symbol_fields(&conn, symbol, fields)?;
    }

    let mut custom_fields = std::collections::HashMap::new();
    if let Some(fields) = transaction.custom_fields {
        for (k, v) in fields {
            if !v.is_empty() { custom_fields.insert(k, v); }
        }
    }

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
                custom_fields: std::collections::HashMap::new(),
            })
        })
        .map_err(|err| err.to_string())?;

    let mut result = rows.next()
        .transpose()
        .map_err(|err| err.to_string())?
        .ok_or_else(|| "Failed to retrieve holding transaction".to_string())?;
    result.custom_fields = custom_fields;
    Ok(result)
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

    let conn = open_db(db_path).map_err(|err| err.to_string())?;
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

    // Update custom fields: delete all then re-insert (per-transaction and per-symbol)
    conn.execute("DELETE FROM holdings_custom_fields WHERE transaction_id = ?1", params![id])
        .map_err(|err| err.to_string())?;
    let mut custom_fields = std::collections::HashMap::new();
    if let Some(ref fields) = transaction.custom_fields {
        for (key, value) in fields {
            if !value.is_empty() {
                conn.execute(
                    "INSERT INTO holdings_custom_fields (transaction_id, field_key, value) VALUES (?1, ?2, ?3)",
                    params![id, key, value],
                ).map_err(|err| err.to_string())?;
                custom_fields.insert(key.clone(), value.clone());
            }
        }
        upsert_holdings_symbol_fields(&conn, symbol, fields)?;
    }

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
                custom_fields: std::collections::HashMap::new(),
            })
        })
        .map_err(|err| err.to_string())?;

    let mut result = rows.next()
        .transpose()
        .map_err(|err| err.to_string())?
        .ok_or_else(|| "Failed to retrieve updated holding transaction".to_string())?;
    result.custom_fields = custom_fields;
    Ok(result)
}

fn remove_holding_transaction(db_path: &PathBuf, id: i64) -> Result<bool, String> {
    let conn = open_db(db_path).map_err(|err| err.to_string())?;
    conn.execute("DELETE FROM holdings_custom_fields WHERE transaction_id = ?1", params![id])
        .map_err(|err| err.to_string())?;
    let affected = conn
        .execute("DELETE FROM holdings_transactions WHERE id = ?1", params![id])
        .map_err(|err| err.to_string())?;
    Ok(affected > 0)
}

fn cache_current_price(conn: &Connection, price: &CurrentPrice) -> Result<(), String> {
    conn.execute(
        "INSERT OR REPLACE INTO cached_current_prices (symbol, price, change, change_percent, volume, last_updated, price_date)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            price.symbol, price.price, price.change, price.change_percent,
            price.volume, price.last_updated, price.price_date,
        ],
    ).map_err(|err| err.to_string())?;
    Ok(())
}

fn load_cached_prices(db_path: &PathBuf, symbols: &[String]) -> Result<Vec<CurrentPrice>, String> {
    if symbols.is_empty() { return Ok(Vec::new()); }
    let conn = open_db(db_path).map_err(|err| err.to_string())?;
    let placeholders: Vec<String> = symbols.iter().enumerate().map(|(i, _)| format!("?{}", i + 1)).collect();
    let sql = format!(
        "SELECT symbol, price, change, change_percent, volume, last_updated, price_date FROM cached_current_prices WHERE symbol IN ({})",
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
            error: None,
        })
    }).map_err(|err| err.to_string())?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|err| err.to_string())
}

fn load_cached_prices_with_fallback(db_path: &PathBuf, symbols: &[String]) -> Result<Vec<CurrentPrice>, String> {
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
                last_updated: String::new(),
                price_date: None,
                error: None,
            });
        }
    }
    Ok(results)
}

fn load_holdings_symbol_fields(db_path: &PathBuf) -> Result<std::collections::HashMap<String, std::collections::HashMap<String, String>>, String> {
    let conn = open_db(db_path).map_err(|err| err.to_string())?;
    let mut stmt = conn
        .prepare("SELECT symbol, field_key, value FROM holdings_symbol_fields")
        .map_err(|err| err.to_string())?;
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?))
        })
        .map_err(|err| err.to_string())?;
    let mut result: std::collections::HashMap<String, std::collections::HashMap<String, String>> = std::collections::HashMap::new();
    for row in rows {
        let (symbol, key, val) = row.map_err(|err| err.to_string())?;
        result.entry(symbol).or_default().insert(key, val);
    }
    Ok(result)
}

fn upsert_holdings_symbol_fields(conn: &Connection, symbol: &str, fields: &std::collections::HashMap<String, String>) -> Result<(), String> {
    for (key, value) in fields {
        if value.is_empty() {
            conn.execute(
                "DELETE FROM holdings_symbol_fields WHERE symbol = ?1 AND field_key = ?2",
                params![symbol, key],
            ).map_err(|err| err.to_string())?;
        } else {
            conn.execute(
                "INSERT OR REPLACE INTO holdings_symbol_fields (symbol, field_key, value) VALUES (?1, ?2, ?3)",
                params![symbol, key, value],
            ).map_err(|err| err.to_string())?;
        }
    }
    Ok(())
}

fn load_config(db_path: &PathBuf) -> Result<Vec<ConfigItem>, String> {
    let conn = open_db(db_path).map_err(|err| err.to_string())?;
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
    let conn = open_db(db_path).map_err(|err| err.to_string())?;
    conn.execute(
        "INSERT INTO app_config (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )
    .map_err(|err| err.to_string())?;
    Ok(())
}

fn persist_price_history(conn: &Connection, symbol: &str, records: &[PriceHistoryPoint]) {
    let now = Utc::now().to_rfc3339();
    for r in records {
        if let Some(close) = r.close {
            if let Err(err) = conn.execute(
                "INSERT INTO prices (symbol, date, close, volume, fetched_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(symbol, date) DO UPDATE SET close = excluded.close, volume = excluded.volume, fetched_at = excluded.fetched_at",
                params![symbol, r.date, close, r.volume, now],
            ) {
                eprintln!("Failed to persist price history for {}: {}", symbol, err);
            }
        }
    }
}

fn persist_price_to_history(conn: &Connection, symbol: &str, price: &CurrentPrice, fetched_at: &str) {
    if let (Some(close), Some(date)) = (price.price, &price.price_date) {
        if let Err(err) = conn.execute(
            "INSERT INTO prices (symbol, date, close, volume, fetched_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(symbol, date) DO UPDATE SET close = excluded.close, volume = excluded.volume, fetched_at = excluded.fetched_at",
            params![symbol, date, close, price.volume, fetched_at],
        ) {
            eprintln!("Failed to persist price for {}: {}", symbol, err);
        }
    }
}

async fn fetch_watchlist_current_prices(db_path: &PathBuf, list: Option<&str>) -> Result<Vec<CurrentPrice>, String> {
    let symbols: Vec<String> = load_watchlist_symbols(db_path, list)?
        .into_iter()
        .map(|s| s.symbol)
        .collect();
    if symbols.is_empty() {
        return Ok(Vec::new());
    }
    fetch_and_cache_current_prices(db_path, &symbols, "watchlist_prices_updated_at").await
}

/// Fetch live prices for `symbols` from Yahoo in small concurrent batches,
/// falling back to the latest stored close on failure, then persist results
/// to the price cache/history and stamp `updated_at_key` in app_config.
async fn fetch_and_cache_current_prices(
    db_path: &PathBuf,
    symbols: &[String],
    updated_at_key: &str,
) -> Result<Vec<CurrentPrice>, String> {
    let client = Client::builder()
        .user_agent("stocks-api/1.0")
        .build()
        .map_err(|err| err.to_string())?;

    // Dedupe while preserving order (a watchlist symbol can be in several lists)
    let mut unique: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for s in symbols {
        if seen.insert(s.clone()) {
            unique.push(s.clone());
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
                if p.price.is_some() {
                    if let Err(err) = cache_current_price(&conn, p) {
                        let _ = insert_event_log(db_path, "error", "price_cache", "api", Some(&p.symbol), &format!("Failed to cache price: {}", err));
                    }
                    persist_price_to_history(&conn, &p.symbol, p, &now);
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

async fn fetch_price_history(db_path: &PathBuf, symbol: &str, days: i64) -> Result<Vec<PriceHistoryPoint>, String> {
    // Scope the connection so this future stays Send — it can then be run
    // concurrently for many symbols via JoinSet.
    let mut history = {
        let conn = open_db(db_path).map_err(|err| err.to_string())?;
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

        rows.collect::<Result<Vec<_>, _>>().map_err(|err| err.to_string())?
    };
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
    let has_enough_data = history.len() as i64 >= days / 2;

    let client = Client::builder()
        .user_agent("stocks-api/1.0")
        .build()
        .map_err(|err| err.to_string())?;

    if history.is_empty() || !has_enough_data {
        match fetch_price_history_from_yahoo(&client, symbol, days).await {
            Ok(records) => {
                if records.len() > history.len() {
                    if let Ok(conn) = open_db(db_path) {
                        persist_price_history(&conn, symbol, &records);
                    }
                    return Ok(records);
                }
            }
            Err(err) => {
                let _ = insert_event_log(db_path, "warn", "price_history_fetch", "api", Some(symbol), &format!("Yahoo history fetch failed, serving stored data: {}", err));
            }
        }
    } else if needs_supplement {
        // Stored data is behind the last trading day — fetch from Yahoo and append missing records
        match fetch_price_history_from_yahoo(&client, symbol, days).await {
            Ok(yahoo) => {
                let new_records: Vec<_> = yahoo.into_iter().filter(|r| r.date > last_stored).collect();
                if let Ok(conn) = open_db(db_path) {
                    persist_price_history(&conn, symbol, &new_records);
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
async fn fetch_histories(db_path: &PathBuf, symbols: &[String], days: i64) -> HashMap<String, Vec<PriceHistoryPoint>> {
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

fn indicator_points(history: &[PriceHistoryPoint]) -> Vec<stocks::indicators::PricePoint> {
    history
        .iter()
        .map(|p| stocks::indicators::PricePoint { close: p.close, volume: p.volume })
        .collect()
}

/// Full indicator block for one symbol — the server-side equivalent of the
/// watchlist enrichment previously computed in the browser.
fn compute_symbol_indicators(history: &[PriceHistoryPoint], price: Option<f64>, volume: Option<i64>) -> serde_json::Value {
    use stocks::indicators as ind;
    let points = indicator_points(history);
    let sma50_arr = ind::calculate_sma(&points, 50);
    let sma150_arr = ind::calculate_sma(&points, 150);
    let sma50 = ind::latest_sma(&sma50_arr);
    let sma150 = ind::latest_sma(&sma150_arr);

    let mut days_50 = None;
    let mut vol_50 = None;
    if let (Some(p), Some(s)) = (price, sma50) {
        if p > s {
            let stats = ind::crossover_stats(&points, &sma50_arr, volume);
            days_50 = Some(stats.days);
            vol_50 = stats.volume_pct;
        }
    }
    let mut days_150 = None;
    let mut vol_150 = None;
    if let (Some(p), Some(s)) = (price, sma150) {
        if p > s {
            let stats = ind::crossover_stats(&points, &sma150_arr, volume);
            days_150 = Some(stats.days);
            vol_150 = stats.volume_pct;
        }
    }

    serde_json::json!({
        "sma50": sma50,
        "sma150": sma150,
        "sma50_trend": ind::sma_trend(&sma50_arr, 5),
        "sma150_trend": ind::sma_trend(&sma150_arr, 5),
        "days_since_50sma": days_50,
        "volume_pct_50sma": vol_50,
        "days_since_150sma": days_150,
        "volume_pct_150sma": vol_150,
        "volume_change_pct": ind::volume_change_pct(&points),
    })
}

/// Watchlist rows with prices and server-computed indicators — one call
/// replaces the N price-history requests the browser used to make.
#[utoipa::path(get, path = "/api/v1/watchlist/enriched", tag = "watchlist", responses((status = 200, description = "Get watchlist enriched")))]
#[get("/api/watchlist/enriched")]
async fn get_watchlist_enriched(db_path: web::Data<PathBuf>, query: web::Query<WatchlistQuery>) -> impl Responder {
    let rows = match load_watchlist_symbols(&db_path, query.list.as_deref()) {
        Ok(r) => r,
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "watchlist_fetch", "api", None, &err);
            return err_internal(err);
        }
    };
    let mut unique: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for r in &rows {
        if seen.insert(r.symbol.clone()) {
            unique.push(r.symbol.clone());
        }
    }
    let prices = match load_cached_prices_with_fallback(&db_path, &unique) {
        Ok(p) => p,
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "cached_prices_fetch", "api", None, &err);
            return err_internal(err);
        }
    };
    let price_map: HashMap<String, CurrentPrice> = prices.into_iter().map(|p| (p.symbol.clone(), p)).collect();

    let mut info: HashMap<String, (Option<String>, Option<String>, Option<String>)> = HashMap::new();
    if let Ok(conn) = open_db(db_path.as_ref()) {
        if let Ok(mut stmt) = conn.prepare("SELECT symbol, instrument_type, long_name, currency FROM symbol_info") {
            if let Ok(info_rows) = stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?, row.get::<_, Option<String>>(2)?, row.get::<_, Option<String>>(3)?))
            }) {
                for r in info_rows.flatten() {
                    info.insert(r.0, (r.1, r.2, r.3));
                }
            }
        }
    }

    let histories = fetch_histories(&db_path, &unique, 300).await;
    let empty: Vec<PriceHistoryPoint> = Vec::new();
    let indicator_map: HashMap<&String, serde_json::Value> = unique
        .iter()
        .map(|sym| {
            let p = price_map.get(sym);
            let hist = histories.get(sym).unwrap_or(&empty);
            (sym, compute_symbol_indicators(hist, p.and_then(|x| x.price), p.and_then(|x| x.volume)))
        })
        .collect();

    let prices_updated_at = load_config(&db_path)
        .ok()
        .and_then(|c| c.into_iter().find(|i| i.key == "watchlist_prices_updated_at").map(|i| i.value));

    let items: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            let p = price_map.get(&r.symbol);
            let i = info.get(&r.symbol);
            serde_json::json!({
                "id": r.id,
                "symbol": r.symbol,
                "list_name": r.list_name,
                "added_at": r.added_at,
                "notes": r.notes,
                "breakthrough_price": r.breakthrough_price,
                "stop_loss_price": r.stop_loss_price,
                "custom_fields": r.custom_fields,
                "instrument_type": i.and_then(|x| x.0.clone()),
                "long_name": i.and_then(|x| x.1.clone()),
                "currency": i.and_then(|x| x.2.clone()),
                "price": p.and_then(|x| x.price),
                "change": p.and_then(|x| x.change),
                "change_percent": p.and_then(|x| x.change_percent),
                "volume": p.and_then(|x| x.volume),
                "price_date": p.and_then(|x| x.price_date.clone()),
                "last_updated": p.map(|x| x.last_updated.clone()),
                "indicators": indicator_map.get(&r.symbol).cloned().unwrap_or(serde_json::Value::Null),
            })
        })
        .collect();

    HttpResponse::Ok().json(serde_json::json!({ "items": items, "prices_updated_at": prices_updated_at }))
}

fn fetch_latest_close_price(db_path: &PathBuf, symbol: &str) -> Result<Option<f64>, String> {
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


fn store_symbol_info(db_path: &PathBuf, symbol: &str, instrument_type: Option<&str>, long_name: Option<&str>, currency: Option<&str>) -> Result<(), String> {
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

fn insert_event_log(
    db_path: &PathBuf,
    level: &str,
    event_type: &str,
    source: &str,
    symbol: Option<&str>,
    details: &str,
) -> Result<(), String> {
    let conn = open_db(db_path).map_err(|err| err.to_string())?;
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO event_log (timestamp, level, source, event_type, symbol, details) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![now, level, source, event_type, symbol, details],
    )
    .map_err(|err| err.to_string())?;
    Ok(())
}

fn fetch_event_log(db_path: &PathBuf, q: &EventQuery) -> Result<(Vec<EventLogEntry>, i64), String> {
    let conn = open_db(db_path).map_err(|err| err.to_string())?;
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

// ---------------------------------------------------------------------------
// Thin-client portfolio endpoints — expose the shared portfolio engine
// (stocks::portfolio) with server-side FX conversion, manual-price and
// instrument-type overrides so every client renders the same numbers.
// ---------------------------------------------------------------------------

const DEFAULT_SECTORS_JSON: &str = r#"["Energy","Materials","Industrials","Consumer Discretionary","Consumer Staples","Health Care","Financials","Information Technology","Communication Services","Utilities","Real Estate","Others"]"#;
const SUPPORTED_CURRENCIES: [&str; 9] = ["AUD", "USD", "GBP", "EUR", "JPY", "CAD", "HKD", "SGD", "NZD"];

fn to_portfolio_txs(rows: &[HoldingTransaction]) -> Vec<PortfolioTx> {
    rows.iter()
        .map(|t| PortfolioTx {
            id: t.id,
            symbol: t.symbol.clone(),
            tx_type: TxType::parse(&t.transaction_type),
            date: t.date.clone(),
            quantity: t.quantity,
            price: t.price,
            native_price: t.original_price,
            amount: t.amount,
            brokerage: t.brokerage,
            dividends_total: t.dividends_total,
        })
        .collect()
}

/// AUD rate per currency, served from the price cache when fresh (<1h),
/// refreshed from Yahoo and re-cached otherwise, falling back to a stale
/// cached value when Yahoo is unreachable.
async fn resolve_fx_rates(db_path: &PathBuf, currencies: &[String]) -> HashMap<String, Option<f64>> {
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
                    live = meta.regular_market_price;
                    if live.is_some() {
                        if let Ok(conn) = open_db(db_path) {
                            let _ = cache_current_price(&conn, &CurrentPrice {
                                symbol: pair.clone(),
                                price: live,
                                change: None,
                                change_percent: None,
                                volume: None,
                                last_updated: Utc::now().to_rfc3339(),
                                price_date: None,
                                error: None,
                            });
                        }
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

/// Latest N-day simple moving average from stored daily closes (no network).
fn stored_sma(conn: &Connection, symbol: &str, period: usize) -> Option<f64> {
    let mut stmt = conn
        .prepare("SELECT close FROM prices WHERE symbol = ?1 AND close IS NOT NULL ORDER BY date DESC LIMIT ?2")
        .ok()?;
    let closes: Vec<f64> = stmt
        .query_map(params![symbol, period as i64], |row| row.get::<_, f64>(0))
        .ok()?
        .flatten()
        .collect();
    if closes.len() < period {
        return None;
    }
    Some(closes.iter().sum::<f64>() / period as f64)
}

struct EffectivePrice {
    native: Option<f64>,
    aud: Option<f64>,
    source: &'static str, // "cache" | "manual" | "none"
    price_date: Option<String>,
    change: Option<f64>,
    change_percent: Option<f64>,
    volume: Option<i64>,
}

struct PortfolioContext {
    groups: Vec<(String, Vec<PortfolioTx>)>,
    prices: HashMap<String, EffectivePrice>,
    /// symbol -> (instrument_type, long_name, currency)
    info: HashMap<String, (Option<String>, Option<String>, Option<String>)>,
    fields: HashMap<String, HashMap<String, String>>,
    intl: HashMap<String, bool>,
    etf: HashMap<String, bool>,
    /// true when every purchase for the symbol was recorded in AUD — such
    /// stocks are displayed in AUD even if they trade in a foreign currency
    all_aud: HashMap<String, bool>,
    fx_rates: HashMap<String, Option<f64>>,
}

impl PortfolioContext {
    fn currency_of(&self, symbol: &str) -> String {
        self.info
            .get(symbol)
            .and_then(|i| i.2.clone())
            .map(|c| c.to_uppercase())
            .unwrap_or_else(|| "AUD".to_string())
    }

    fn to_aud(&self, symbol: &str, value: f64) -> f64 {
        let currency = self.currency_of(symbol);
        if currency == "AUD" {
            return value;
        }
        match self.fx_rates.get(&currency).copied().flatten() {
            Some(rate) if rate != 0.0 => value * rate,
            _ => value,
        }
    }

    fn sector_of(&self, symbol: &str) -> Option<String> {
        self.fields
            .get(symbol)
            .and_then(|f| f.get("sector").cloned())
            .filter(|s| !s.is_empty())
    }
}

async fn build_portfolio_context(db_path: &PathBuf) -> Result<PortfolioContext, String> {
    let rows = fetch_holdings(db_path)?;
    let txs = to_portfolio_txs(&rows);
    let groups = portfolio::group_by_symbol(&txs);
    let symbols: Vec<String> = groups.iter().map(|(s, _)| s.clone()).collect();

    let config: HashMap<String, String> = load_config(db_path)?.into_iter().map(|c| (c.key, c.value)).collect();
    let fields = load_holdings_symbol_fields(db_path)?;

    let conn = open_db(db_path).map_err(|e| e.to_string())?;
    let mut info: HashMap<String, (Option<String>, Option<String>, Option<String>)> = HashMap::new();
    {
        let mut stmt = conn
            .prepare("SELECT symbol, instrument_type, long_name, currency FROM symbol_info")
            .map_err(|e| e.to_string())?;
        let info_rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        for r in info_rows.flatten() {
            info.insert(r.0, (r.1, r.2, r.3));
        }
    }

    // Effective classification per symbol: instrument_type_* config override,
    // and the "purchased entirely in AUD ⇒ domestic" rule from the UI.
    let mut intl = HashMap::new();
    let mut etf = HashMap::new();
    let mut all_aud_map = HashMap::new();
    for symbol in &symbols {
        let purchases: Vec<&HoldingTransaction> = rows
            .iter()
            .filter(|t| &t.symbol == symbol && t.transaction_type == "purchase")
            .collect();
        let all_aud = !purchases.is_empty() && purchases.iter().all(|t| t.currency == "AUD");
        all_aud_map.insert(symbol.clone(), all_aud);
        let yahoo_currency = info.get(symbol).and_then(|i| i.2.clone()).map(|c| c.to_uppercase());
        let is_intl = if all_aud { false } else { matches!(&yahoo_currency, Some(c) if c != "AUD") };
        intl.insert(symbol.clone(), is_intl);
        let itype = config
            .get(&format!("instrument_type_{}", symbol))
            .cloned()
            .filter(|v| !v.is_empty())
            .or_else(|| info.get(symbol).and_then(|i| i.0.clone()))
            .unwrap_or_default();
        etf.insert(symbol.clone(), itype == "ETF" || itype == "MUTUALFUND");
    }

    let currencies: Vec<String> = symbols
        .iter()
        .filter_map(|s| info.get(s).and_then(|i| i.2.clone()))
        .map(|c| c.to_uppercase())
        .filter(|c| c != "AUD")
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    let fx_rates = resolve_fx_rates(db_path, &currencies).await;

    let cached = load_cached_prices(db_path, &symbols)?;
    let cached_map: HashMap<String, CurrentPrice> = cached.into_iter().map(|p| (p.symbol.clone(), p)).collect();
    let mut prices: HashMap<String, EffectivePrice> = HashMap::new();
    for symbol in &symbols {
        let c = cached_map.get(symbol);
        let mut native = c.and_then(|p| p.price);
        let mut source = if native.is_some() { "cache" } else { "none" };
        if native.is_none() || native == Some(0.0) {
            if let Some(manual) = config.get(&format!("manual_price_{}", symbol)) {
                if let Ok(v) = manual.parse::<f64>() {
                    native = Some(v);
                    source = "manual";
                }
            }
        }
        let currency = info.get(symbol).and_then(|i| i.2.clone()).map(|c| c.to_uppercase());
        let aud = match (&native, &currency) {
            (Some(n), Some(cur)) if cur != "AUD" => match fx_rates.get(cur).copied().flatten() {
                Some(rate) if rate != 0.0 => Some(n * rate),
                _ => Some(*n),
            },
            (Some(n), _) => Some(*n),
            _ => None,
        };
        prices.insert(symbol.clone(), EffectivePrice {
            native,
            aud,
            source,
            price_date: c.and_then(|p| p.price_date.clone()),
            change: c.and_then(|p| p.change),
            change_percent: c.and_then(|p| p.change_percent),
            volume: c.and_then(|p| p.volume),
        });
    }

    Ok(PortfolioContext { groups, prices, info, fields, intl, etf, all_aud: all_aud_map, fx_rates })
}

#[utoipa::path(get, path = "/api/v1/portfolio/holdings", tag = "portfolio", responses((status = 200, description = "Get portfolio holdings")))]
#[get("/api/portfolio/holdings")]
async fn get_portfolio_holdings(db_path: web::Data<PathBuf>) -> impl Responder {
    let ctx = match build_portfolio_context(&db_path).await {
        Ok(c) => c,
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "portfolio_fetch", "api", None, &err);
            return err_internal(err);
        }
    };
    let conn = match open_db(db_path.as_ref()) {
        Ok(c) => c,
        Err(err) => return err_internal(err.to_string()),
    };

    let mut holdings = Vec::new();
    for (symbol, txs) in &ctx.groups {
        let summary = portfolio::calc_symbol_summary(txs);
        if summary.remaining_shares <= 0.0 {
            continue;
        }
        let dividends = portfolio::symbol_dividends(&portfolio::sort_transactions(txs));
        let ep = ctx.prices.get(symbol);
        let price_aud = ep.and_then(|p| p.aud);
        let current_value = price_aud.filter(|p| *p != 0.0).map(|p| summary.remaining_shares * p).unwrap_or(0.0);
        let invested = summary.remaining_cost;
        let pl = current_value - invested + dividends;
        let sym_fields = ctx.fields.get(symbol);
        let fields: HashMap<&String, &String> = sym_fields
            .map(|f| f.iter().filter(|(k, _)| k.as_str() != "_notes").collect())
            .unwrap_or_default();
        let sma150 = stored_sma(&conn, symbol, 150).map(|v| ctx.to_aud(symbol, v));
        let itype = ctx.info.get(symbol).and_then(|i| i.0.clone());
        holdings.push(serde_json::json!({
            "symbol": symbol,
            "long_name": ctx.info.get(symbol).and_then(|i| i.1.clone()),
            "instrument_type": itype,
            "is_etf": ctx.etf.get(symbol).copied().unwrap_or(false),
            "is_international": ctx.intl.get(symbol).copied().unwrap_or(false),
            "currency": ctx.currency_of(symbol),
            "sector": ctx.sector_of(symbol),
            "notes": sym_fields.and_then(|f| f.get("_notes").cloned()),
            "fields": fields,
            "shares": summary.remaining_shares,
            "invested": invested,
            "avg_cost": if summary.remaining_shares > 0.0 { Some(invested / summary.remaining_shares) } else { None },
            "native_avg_cost": if summary.remaining_shares > 0.0 { Some(summary.native_remaining_cost / summary.remaining_shares) } else { None },
            "current_price": price_aud,
            "native_current_price": ep.and_then(|p| p.native),
            "price_source": ep.map(|p| p.source).unwrap_or("none"),
            "price_date": ep.and_then(|p| p.price_date.clone()),
            "change": ep.and_then(|p| p.change),
            "change_percent": ep.and_then(|p| p.change_percent),
            "volume": ep.and_then(|p| p.volume),
            "current_value": current_value,
            "dividends": dividends,
            "pl": pl,
            "pl_pct": if invested > 0.0 { Some(pl / invested * 100.0) } else { None },
            "sma150": sma150,
        }));
    }

    HttpResponse::Ok().json(serde_json::json!({ "holdings": holdings, "fx_rates": ctx.fx_rates }))
}

#[utoipa::path(get, path = "/api/v1/portfolio/overview", tag = "portfolio", responses((status = 200, description = "Get portfolio overview")))]
#[get("/api/portfolio/overview")]
async fn get_portfolio_overview(db_path: web::Data<PathBuf>) -> impl Responder {
    let ctx = match build_portfolio_context(&db_path).await {
        Ok(c) => c,
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "portfolio_fetch", "api", None, &err);
            return err_internal(err);
        }
    };

    #[derive(Default)]
    struct Agg {
        count: usize,
        value: f64,
        dividends: f64,
        pl: f64,
        cost: f64,
    }
    impl Agg {
        fn add(&mut self, value: f64, dividends: f64, pl: f64, cost: f64) {
            self.count += 1;
            self.value += value;
            self.dividends += dividends;
            self.pl += pl;
            self.cost += cost;
        }
        fn json(&self) -> serde_json::Value {
            serde_json::json!({ "count": self.count, "value": self.value, "dividends": self.dividends, "pl": self.pl, "cost": self.cost })
        }
    }

    let mut holdings_agg = Agg::default();
    let mut equity_agg = Agg::default();
    let mut etf_agg = Agg::default();
    let mut sector_aggs: HashMap<String, Agg> = HashMap::new();
    let mut sold_agg = Agg::default();
    let mut sold_pl = 0.0;

    for (symbol, txs) in &ctx.groups {
        let pos = portfolio::calc_symbol_position(txs);

        if pos.remaining_shares > 0.0 {
            let price = ctx.prices.get(symbol).and_then(|p| p.aud).filter(|p| *p != 0.0);
            let current_value = price.map(|p| pos.remaining_shares * p).unwrap_or(0.0);
            let sym_pl = current_value - pos.remaining_cost + pos.dividends;
            holdings_agg.add(current_value, pos.dividends, sym_pl, pos.remaining_cost);
            if ctx.etf.get(symbol).copied().unwrap_or(false) {
                etf_agg.add(current_value, pos.dividends, sym_pl, pos.remaining_cost);
            } else {
                equity_agg.add(current_value, pos.dividends, sym_pl, pos.remaining_cost);
            }
            let sector = ctx.sector_of(symbol).unwrap_or_else(|| "Unallocated".to_string());
            sector_aggs.entry(sector).or_default().add(current_value, pos.dividends, sym_pl, pos.remaining_cost);
        }

        let sym_sold_pl = pos.sold_pl();
        if pos.sold_proceeds > 0.0 {
            sold_agg.add(
                pos.sold_proceeds,
                pos.sold_dividends,
                sym_sold_pl,
                pos.sold_proceeds - sym_sold_pl + pos.sold_dividends,
            );
        }
        sold_pl += sym_sold_pl;
    }

    let mut sectors: Vec<(String, Agg)> = sector_aggs.into_iter().collect();
    sectors.sort_by(|a, b| b.1.value.partial_cmp(&a.1.value).unwrap_or(std::cmp::Ordering::Equal));
    let sectors_json: Vec<serde_json::Value> = sectors
        .into_iter()
        .map(|(name, agg)| {
            let mut v = agg.json();
            v["name"] = serde_json::json!(name);
            v
        })
        .collect();

    // ------------------------------------------------------------------
    // Dashboard lists — previously computed in the browser from N price
    // history requests; now derived server-side.
    // ------------------------------------------------------------------
    let config: HashMap<String, String> = load_config(&db_path)
        .map(|c| c.into_iter().map(|i| (i.key, i.value)).collect())
        .unwrap_or_default();

    // Worst holdings vs their 150-day SMA
    let mut worst_holdings: Vec<serde_json::Value> = Vec::new()
;
    if let Ok(conn) = open_db(db_path.as_ref()) {
        let mut scored: Vec<(f64, serde_json::Value)> = Vec::new();
        for (symbol, txs) in &ctx.groups {
            let pos = portfolio::calc_symbol_position(txs);
            if pos.remaining_shares <= 0.0 {
                continue;
            }
            let price = ctx.prices.get(symbol).and_then(|p| p.aud).filter(|p| *p != 0.0);
            let sma150 = stored_sma(&conn, symbol, 150).map(|v| ctx.to_aud(symbol, v));
            if let (Some(p), Some(s)) = (price, sma150) {
                if s != 0.0 {
                    let pct = (p - s) / s * 100.0;
                    scored.push((pct, serde_json::json!({ "symbol": symbol, "price": p, "sma150": s, "pct_diff": pct })));
                }
            }
        }
        scored.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        worst_holdings = scored.into_iter().take(15).map(|(_, v)| v).collect();
    }

    // Watchlist rows are needed for both best-watchlist and custom lists
    let watchlist_rows = load_watchlist_symbols(&db_path, None).unwrap_or_default();
    let mut watch_unique: Vec<String> = Vec::new();
    let mut watch_seen = std::collections::HashSet::new();
    for r in &watchlist_rows {
        if watch_seen.insert(r.symbol.clone()) {
            watch_unique.push(r.symbol.clone());
        }
    }
    let watch_prices: HashMap<String, CurrentPrice> = load_cached_prices(&db_path, &watch_unique)
        .unwrap_or_default()
        .into_iter()
        .map(|p| (p.symbol.clone(), p))
        .collect();

    // Best watchlist — most recently crossed above their 50-day SMA
    let histories = fetch_histories(&db_path, &watch_unique, 300).await;
    let mut best: Vec<(i64, serde_json::Value)> = Vec::new();
    {
        use stocks::indicators as ind;
        for sym in &watch_unique {
            let Some(p) = watch_prices.get(sym).and_then(|x| x.price) else { continue };
            let Some(hist) = histories.get(sym) else { continue };
            let points = indicator_points(hist);
            let sma50_arr = ind::calculate_sma(&points, 50);
            let Some(sma50) = ind::latest_sma(&sma50_arr) else { continue };
            if p <= sma50 {
                continue;
            }
            let stats = ind::crossover_stats(&points, &sma50_arr, watch_prices.get(sym).and_then(|x| x.volume));
            best.push((stats.days, serde_json::json!({
                "symbol": sym,
                "price": p,
                "sma50": sma50,
                "sma50_trend": ind::sma_trend(&sma50_arr, 5),
                "days_since_50sma": stats.days,
                "volume_pct_50sma": stats.volume_pct,
            })));
        }
    }
    best.sort_by_key(|(days, _)| *days);
    let best_watchlist: Vec<serde_json::Value> = best.into_iter().take(15).map(|(_, v)| v).collect();

    // Custom dashboard lists: price vs a user-defined field
    #[derive(Deserialize)]
    struct DashboardListDef {
        key: String,
        label: String,
        source: String,
        field_key: String,
        operator: String,
        #[serde(default)]
        limit: Option<usize>,
        #[serde(default)]
        sort: Option<String>,
    }
    #[derive(Deserialize)]
    struct FieldDef {
        key: String,
        label: String,
    }
    let list_defs: Vec<DashboardListDef> = config
        .get("dashboard_custom_lists")
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();
    let holdings_field_defs: Vec<FieldDef> = config
        .get("holdings_custom_fields")
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();
    let watchlist_field_defs: Vec<FieldDef> = config
        .get("watchlist_custom_fields")
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();

    let custom_lists: Vec<serde_json::Value> = list_defs
        .iter()
        .map(|def| {
            let (field_source, field_key) = def.field_key.split_once(':').unwrap_or(("", ""));
            struct Entry {
                symbol: String,
                price: f64,
                field_value: f64,
                diff: f64,
                pct_diff: f64,
                currency: Option<String>,
            }
            let matches_op = |diff: f64| match def.operator.as_str() {
                "above" | "pct_below" => diff > 0.0,
                "below" | "pct_above" => diff < 0.0,
                _ => false,
            };
            let mut entries: Vec<Entry> = Vec::new();

            if (def.source == "holdings" || def.source == "both") && field_source == "holdings" {
                for (symbol, txs) in &ctx.groups {
                    let pos = portfolio::calc_symbol_position(txs);
                    if pos.remaining_shares <= 0.0 {
                        continue;
                    }
                    let Some(price) = ctx.prices.get(symbol).and_then(|p| p.native) else { continue };
                    let Some(fv) = ctx.fields.get(symbol).and_then(|f| f.get(field_key)).and_then(|v| v.parse::<f64>().ok()).filter(|v| *v > 0.0) else { continue };
                    let diff = price - fv;
                    if matches_op(diff) {
                        entries.push(Entry {
                            symbol: symbol.clone(),
                            price,
                            field_value: fv,
                            diff,
                            pct_diff: diff / fv * 100.0,
                            currency: ctx.info.get(symbol).and_then(|i| i.2.clone()),
                        });
                    }
                }
            }

            if (def.source == "watchlist" || def.source == "both") && field_source == "watchlist" {
                for row in &watchlist_rows {
                    if entries.iter().any(|e| e.symbol == row.symbol) {
                        continue;
                    }
                    let Some(price) = watch_prices.get(&row.symbol).and_then(|p| p.price) else { continue };
                    let fv = match field_key {
                        "breakthrough_price" => row.breakthrough_price,
                        "stop_loss_price" => row.stop_loss_price,
                        _ => row.custom_fields.get(field_key).and_then(|v| v.parse::<f64>().ok()),
                    };
                    let Some(fv) = fv.filter(|v| *v > 0.0) else { continue };
                    let diff = price - fv;
                    if matches_op(diff) {
                        entries.push(Entry {
                            symbol: row.symbol.clone(),
                            price,
                            field_value: fv,
                            diff,
                            pct_diff: diff / fv * 100.0,
                            currency: None,
                        });
                    }
                }
            }

            let pct_op = def.operator == "pct_above" || def.operator == "pct_below";
            entries.sort_by(|a, b| {
                let cmp = if pct_op {
                    a.pct_diff.abs().partial_cmp(&b.pct_diff.abs())
                } else {
                    a.pct_diff.partial_cmp(&b.pct_diff)
                }
                .unwrap_or(std::cmp::Ordering::Equal);
                if def.sort.as_deref() == Some("desc") { cmp.reverse() } else { cmp }
            });
            entries.truncate(def.limit.unwrap_or(15));

            let builtin_labels: HashMap<&str, &str> = HashMap::from([
                ("breakthrough_price", "Breakthrough Price"),
                ("stop_loss_price", "Stop Loss Price"),
                ("stop_loss", "Stop Loss Price"),
                ("trailing_sell_pct", "Trailing Sell %"),
            ]);
            let field_defs = if field_source == "holdings" { &holdings_field_defs } else { &watchlist_field_defs };
            let field_label = field_defs
                .iter()
                .find(|f| f.key == field_key)
                .map(|f| f.label.clone())
                .or_else(|| builtin_labels.get(field_key).map(|s| s.to_string()))
                .unwrap_or_else(|| field_key.to_string());

            serde_json::json!({
                "key": def.key,
                "label": def.label,
                "source": def.source,
                // Where the entry symbols actually live (holdings vs watchlist),
                // derived from the field_key prefix. Drives click navigation.
                "field_source": field_source,
                "operator": def.operator,
                "field_label": field_label,
                "entries": entries.iter().map(|e| serde_json::json!({
                    "symbol": e.symbol,
                    "price": e.price,
                    "field_value": e.field_value,
                    "diff": e.diff,
                    "pct_diff": e.pct_diff,
                    "currency": e.currency,
                })).collect::<Vec<_>>(),
            })
        })
        .collect();

    HttpResponse::Ok().json(serde_json::json!({
        "totals": {
            "stock_count": holdings_agg.count,
            "total_value": holdings_agg.value,
            "total_pl": holdings_agg.pl + sold_pl,
            "holdings_pl": holdings_agg.pl,
            "sold_pl": sold_pl,
        },
        "breakdowns": {
            "equities": equity_agg.json(),
            "etfs": etf_agg.json(),
            "holdings": holdings_agg.json(),
            "sold": {
                "count": sold_agg.count,
                "value": sold_agg.value,
                "dividends": sold_agg.dividends,
                "pl": sold_pl,
                "cost": sold_agg.cost,
            },
        },
        "sectors": sectors_json,
        "worst_holdings": worst_holdings,
        "best_watchlist": best_watchlist,
        "custom_lists": custom_lists,
    }))
}

#[utoipa::path(get, path = "/api/v1/portfolio/lots", tag = "portfolio", responses((status = 200, description = "Get portfolio lots")))]
#[get("/api/portfolio/lots")]
async fn get_portfolio_lots(db_path: web::Data<PathBuf>) -> impl Responder {
    let ctx = match build_portfolio_context(&db_path).await {
        Ok(c) => c,
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "portfolio_fetch", "api", None, &err);
            return err_internal(err);
        }
    };

    let all_txs: Vec<PortfolioTx> = ctx.groups.iter().flat_map(|(_, txs)| txs.clone()).collect();
    let remaining = portfolio::calc_remaining_by_lot(&all_txs);

    let mut lots = Vec::new();
    for (symbol, txs) in &ctx.groups {
        let price_aud = ctx.prices.get(symbol).and_then(|p| p.aud);
        for tx in txs {
            if tx.tx_type != TxType::Purchase {
                continue;
            }
            let rem = remaining.get(&tx.id).copied().unwrap_or(0.0);
            let (current_value, unrealised_pl) = match (price_aud, tx.price) {
                (Some(p), Some(cost)) if rem > 0.0 => {
                    let value = rem * p;
                    (Some(value), Some(value - (rem * cost + tx.brokerage.unwrap_or(0.0))))
                }
                _ => (None, None),
            };
            lots.push(serde_json::json!({
                "transaction_id": tx.id,
                "symbol": symbol,
                "date": tx.date,
                "remaining": rem,
                "current_value": current_value,
                "unrealised_pl": unrealised_pl,
            }));
        }
    }

    HttpResponse::Ok().json(serde_json::json!({ "lots": lots }))
}

#[utoipa::path(get, path = "/api/v1/portfolio/sold", tag = "portfolio", responses((status = 200, description = "Get portfolio sold")))]
#[get("/api/portfolio/sold")]
async fn get_portfolio_sold(db_path: web::Data<PathBuf>) -> impl Responder {
    let rows = match fetch_holdings(&db_path) {
        Ok(r) => r,
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "portfolio_fetch", "api", None, &err);
            return err_internal(err);
        }
    };
    let txs = to_portfolio_txs(&rows);
    let mut entries: Vec<portfolio::SoldEntry> = Vec::new();
    for (_, group) in portfolio::group_by_symbol(&txs) {
        entries.extend(portfolio::calc_sold_entries(&group));
    }
    entries.sort_by(|a, b| b.date.cmp(&a.date));

    let total_realised_pl: f64 = entries.iter().map(|e| e.realised_pl).sum();
    let total_cost: f64 = entries.iter().map(|e| e.avg_purchase_price * e.quantity).sum();
    let entries_json: Vec<serde_json::Value> = entries
        .iter()
        .map(|e| serde_json::json!({
            "symbol": e.symbol,
            "date": e.date,
            "quantity": e.quantity,
            "avg_purchase_price": e.avg_purchase_price,
            "sale_price": e.sale_price,
            "brokerage": e.brokerage,
            "dividends": e.dividends,
            "days_held": e.days_held,
            "realised_pl": e.realised_pl,
        }))
        .collect();

    HttpResponse::Ok().json(serde_json::json!({
        "entries": entries_json,
        "total_realised_pl": total_realised_pl,
        "total_cost": total_cost,
    }))
}

/// Risk / stop-loss analysis per active holding — the server-side port of the
/// Analysis screen's row computation. Display-currency rule: a stock purchased
/// entirely in AUD is shown in AUD (market prices converted); otherwise it is
/// shown in its native trading currency.
#[utoipa::path(get, path = "/api/v1/portfolio/risk", tag = "portfolio", responses((status = 200, description = "Get portfolio risk")))]
#[get("/api/portfolio/risk")]
async fn get_portfolio_risk(db_path: web::Data<PathBuf>) -> impl Responder {
    let ctx = match build_portfolio_context(&db_path).await {
        Ok(c) => c,
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "portfolio_fetch", "api", None, &err);
            return err_internal(err);
        }
    };
    let conn = match open_db(db_path.as_ref()) {
        Ok(c) => c,
        Err(err) => return err_internal(err.to_string()),
    };

    let mut rows = Vec::new();
    let mut total_invested = 0.0;
    let mut total_sl_dollar = 0.0;

    for (symbol, txs) in &ctx.groups {
        let summary = portfolio::calc_symbol_summary(txs);
        if summary.remaining_shares <= 0.0 {
            continue;
        }
        let shares = summary.remaining_shares;
        let symbol_currency = ctx.currency_of(symbol);
        let all_aud = ctx.all_aud.get(symbol).copied().unwrap_or(true);
        let display_currency = if all_aud { "AUD".to_string() } else { symbol_currency.clone() };
        let is_foreign = display_currency != "AUD";
        let rate = ctx.fx_rates.get(&symbol_currency).copied().flatten().filter(|r| *r != 0.0);
        let needs_conversion = all_aud && symbol_currency != "AUD" && rate.is_some();
        let to_display = |p: f64| if needs_conversion { p * rate.unwrap() } else { p };

        // Average purchase price in the display currency: AUD lots when
        // purchased in AUD, native-currency lots otherwise.
        let purchase_price = if is_foreign {
            summary.native_remaining_cost / shares
        } else {
            summary.remaining_cost / shares
        };

        let current_price = ctx.prices.get(symbol).and_then(|p| p.native).map(&to_display);
        let pl_pct = match current_price {
            Some(c) if purchase_price != 0.0 && c != 0.0 => Some((c - purchase_price) / purchase_price * 100.0),
            _ => None,
        };

        let sym_fields = ctx.fields.get(symbol);
        let stop_loss_manual = sym_fields
            .and_then(|f| f.get("stop_loss"))
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|v| *v != 0.0);
        let trailing_pct = sym_fields
            .and_then(|f| f.get("trailing_sell_pct"))
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|v| *v > 0.0);

        let mut stop_loss: Option<f64> = stop_loss_manual;
        let mut is_trailing = false;
        if stop_loss.is_none() {
            if let Some(pct) = trailing_pct {
                // Trailing trigger: highest close since placement date (plus
                // the live price) minus the trailing percentage.
                let mut reference = current_price;
                if let Some(since) = sym_fields.and_then(|f| f.get("trailing_sell_date")).filter(|d| !d.is_empty()) {
                    let mut closes: Vec<f64> = conn
                        .prepare("SELECT close FROM prices WHERE symbol = ?1 AND close IS NOT NULL AND date >= ?2")
                        .ok()
                        .and_then(|mut stmt| {
                            stmt.query_map(params![symbol, since], |row| row.get::<_, f64>(0))
                                .ok()
                                .map(|r| r.flatten().map(&to_display).collect())
                        })
                        .unwrap_or_default();
                    if let Some(c) = current_price {
                        closes.push(c);
                    }
                    if !closes.is_empty() {
                        reference = closes.into_iter().reduce(f64::max);
                    }
                }
                if let Some(r) = reference.filter(|r| *r != 0.0) {
                    stop_loss = Some(r * (1.0 - pct / 100.0));
                    is_trailing = true;
                }
            }
        }

        let stop_loss_pct = stop_loss
            .filter(|_| purchase_price != 0.0)
            .map(|sl| (sl - purchase_price) / purchase_price * 100.0);
        let sl_dollar_native = stop_loss
            .filter(|_| purchase_price != 0.0 && shares > 0.0)
            .map(|sl| (sl - purchase_price) * shares);
        let stop_loss_dollar = sl_dollar_native.map(|v| match (is_foreign, rate) {
            (true, Some(r)) => v * r,
            _ => v,
        });

        let sma50 = stored_sma(&conn, symbol, 50).map(&to_display);
        let sma150 = stored_sma(&conn, symbol, 150).map(&to_display);
        let high30d: Option<f64> = conn
            .prepare("SELECT close FROM prices WHERE symbol = ?1 AND close IS NOT NULL ORDER BY date DESC LIMIT 30")
            .ok()
            .and_then(|mut stmt| {
                stmt.query_map(params![symbol], |row| row.get::<_, f64>(0))
                    .ok()
                    .and_then(|r| r.flatten().map(&to_display).reduce(f64::max))
            });

        let invested = if purchase_price != 0.0 && shares > 0.0 { purchase_price * shares } else { 0.0 };
        total_invested += invested;
        total_sl_dollar += stop_loss_dollar.unwrap_or(0.0);

        rows.push(serde_json::json!({
            "symbol": symbol,
            "currency": display_currency,
            "current_price": current_price,
            "purchase_price": if purchase_price != 0.0 { Some(purchase_price) } else { None },
            "pl_pct": pl_pct,
            "stop_loss": stop_loss,
            "is_trailing_sell": is_trailing,
            "stop_loss_pct": stop_loss_pct,
            "stop_loss_dollar": stop_loss_dollar,
            "sma50": sma50,
            "sma150": sma150,
            "high30d": high30d,
            "total_invested": invested,
        }));
    }

    let total_sl_pct = if total_invested > 0.0 { Some(total_sl_dollar / total_invested * 100.0) } else { None };
    HttpResponse::Ok().json(serde_json::json!({
        "rows": rows,
        "totals": {
            "total_invested": total_invested,
            "total_sl_dollar": total_sl_dollar,
            "total_sl_pct": total_sl_pct,
        },
    }))
}

/// Cheap change-detection for polling clients: last-modified stamps per data
/// domain, sourced from the audit log (every tracked table has triggers) and
/// the price-refresh timestamps. A mobile app polls this one tiny endpoint
/// and refetches a domain's payload only when its stamp moves.
#[utoipa::path(get, path = "/api/v1/sync-state", tag = "system", responses((status = 200, description = "Get sync state")))]
#[get("/api/sync-state")]
async fn get_sync_state(db_path: web::Data<PathBuf>) -> impl Responder {
    let conn = match open_db(db_path.as_ref()) {
        Ok(c) => c,
        Err(err) => return err_internal(err.to_string()),
    };
    let mut latest: HashMap<String, String> = HashMap::new();
    if let Ok(mut stmt) = conn.prepare("SELECT table_name, MAX(timestamp) FROM audit_log GROUP BY table_name") {
        if let Ok(rows) = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
        }) {
            for (table, ts) in rows.flatten() {
                if let Some(ts) = ts {
                    latest.insert(table, ts);
                }
            }
        }
    }
    let max_of = |tables: &[&str]| -> Option<String> {
        tables.iter().filter_map(|t| latest.get(*t)).max().cloned()
    };
    let config: HashMap<String, String> = load_config(&db_path)
        .map(|c| c.into_iter().map(|i| (i.key, i.value)).collect())
        .unwrap_or_default();

    HttpResponse::Ok().json(serde_json::json!({
        "holdings": max_of(&["holdings_transactions", "holdings_symbol_fields", "holdings_custom_fields"]),
        "watchlist": max_of(&["watchlist_symbols", "watchlist_memberships", "watchlist_symbol_fields"]),
        "dividends": max_of(&["dividend_events"]),
        "symbol_info": max_of(&["symbol_info"]),
        "config": max_of(&["app_config"]),
        "watchlist_prices_updated_at": config.get("watchlist_prices_updated_at"),
        "holdings_prices_updated_at": config.get("holdings_prices_updated_at"),
        "last_full_refresh_at": config.get("last_full_refresh_at"),
        "server_time": Utc::now().to_rfc3339(),
    }))
}

#[utoipa::path(get, path = "/api/v1/meta", tag = "meta", responses((status = 200, description = "Get meta")))]
#[get("/api/meta")]
async fn get_meta(db_path: web::Data<PathBuf>) -> impl Responder {
    let config: HashMap<String, String> = match load_config(&db_path) {
        Ok(c) => c.into_iter().map(|c| (c.key, c.value)).collect(),
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "meta_fetch", "api", None, &err);
            return err_internal(err);
        }
    };
    let sectors: serde_json::Value = config
        .get("sectors")
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_else(|| serde_json::from_str(DEFAULT_SECTORS_JSON).unwrap());
    let parse_defs = |key: &str| -> serde_json::Value {
        config
            .get(key)
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_else(|| serde_json::json!([]))
    };
    HttpResponse::Ok().json(serde_json::json!({
        "sectors": sectors,
        "currencies": SUPPORTED_CURRENCIES,
        "holdings_custom_fields": parse_defs("holdings_custom_fields"),
        "watchlist_custom_fields": parse_defs("watchlist_custom_fields"),
        "dashboard_custom_lists": parse_defs("dashboard_custom_lists"),
        "reserved_holdings_keys": ["stop_loss", "trailing_sell_pct", "trailing_sell_date", "sector"],
        "reserved_watchlist_keys": ["breakthrough_price", "stop_loss_price", "sector"],
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
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
            currency: "AUD".to_string(),
            original_price: None,
            fx_rate: None,
            custom_fields: Default::default(),
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
        let conn = open_db(db_path).unwrap();
        conn.execute(
            "INSERT INTO holdings_transactions (id, symbol, transaction_type, date, quantity, price, brokerage, created_at)
             VALUES (?1, 'TST.AX', ?2, ?3, ?4, ?5, ?6, '2024-01-01T00:00:00Z')",
            rusqlite::params![id, tx_type, date, qty, price, brokerage],
        ).unwrap();
    }

    fn insert_dividend_event(db_path: &PathBuf, ex_date: &str, amount: f64) {
        let conn = open_db(db_path).unwrap();
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

        let conn = open_db(&db_path).unwrap();
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
            currency: None,
            original_price: None,
            fx_rate: None,
            custom_fields: None,
            confirm: None,
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
            currency: None,
            original_price: None,
            fx_rate: None,
            custom_fields: None,
            confirm: None,
        };

        let result = insert_holding_transaction(&db_path, "TST.AX", payload);
        assert!(result.is_err(), "should reject zero quantity");
    }
}

use actix_cors::Cors;
use actix_web::{delete, get, patch, post, put, web, App, HttpResponse, HttpServer, Responder};
use chrono::{Datelike, NaiveDate, TimeZone, Timelike, Utc};
use reqwest::Client;
use rusqlite::{params, types::Type, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, env, path::PathBuf};
use stocks::hindsight;
use stocks::portfolio::{self, PortfolioTx, TxType};

mod market;
mod schema;
use market::{
    fetch_current_price, fetch_histories, fetch_price_history, fx_pair_symbol, fx_rate_on,
    fetch_price_history_from_yahoo, persist_price_history, resolve_fx_rates, yahoo_local_date,
    YahooHistoryResponse, YahooMeta,
};
#[cfg(test)]
use market::{
    history_recently_checked, mark_history_checked, session_range, HISTORY_CHECKED,
    HISTORY_CHECK_TTL_SECS,
};
use schema::init_db;

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
    /// Session OHLC for the day `price_date` refers to. `price` is the running
    /// close, so these three complete today's candle while the market is open.
    day_open: Option<f64>,
    day_high: Option<f64>,
    day_low: Option<f64>,
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
    /// Cash account this trade settles against, when it has one.
    cash_account_id: Option<i64>,
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
    /// Cash account this trade settles against. Omit to record the trade
    /// without touching the cash ledger, as every pre-ledger trade does.
    cash_account_id: Option<i64>,
    /// Tax withheld from this payment, overriding the symbol's standing rate.
    withholding_amount: Option<f64>,
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
        if let Some(qty) = payload.quantity
            && qty > held + 1e-9 {
                return Err(HttpResponse::Conflict().json(serde_json::json!({
                    "error": {
                        "code": "oversell_confirmation_required",
                        "message": format!("Selling {} shares but only {:.2} held for {}. Re-submit with confirm=true to record anyway.", qty, held, symbol),
                        "held": held,
                    }
                })));
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

/// The vocabulary a dashboard list definition may use. Kept beside the
/// validator rather than inline in the engine so the two cannot drift: a value
/// the engine would silently treat as "matches nothing" is rejected on write.
const LIST_SOURCES: [&str; 3] = ["holdings", "watchlist", "both"];
const LIST_OPERATORS: [&str; 7] = [
    "above", "below", "pct_above", "pct_below", "days_above", "days_below", "volume_cross_pct",
];
const LIST_COMPARES: [&str; 2] = ["price", "volume"];
const LIST_SORTS: [&str; 2] = ["asc", "desc"];
const LIST_INDICATORS: [&str; 3] = ["sma50", "sma150", "ema40w"];

/// Marks a symbol the market no longer trades. One key per symbol, holding the
/// last date it traded; `manual_price_<symbol>` holds what it was worth.
///
/// Two keys rather than one because they answer different questions and are
/// consumed in different places: the price feeds the existing valuation chain
/// untouched, while the date bounds how far forward any "since" window can
/// honestly be read.
const DEAD_SYMBOL_PREFIX: &str = "dead_symbol_";

/// Reject a config value the server would later fail to parse.
///
/// These keys hold JSON that other endpoints read back. Without this a typo is
/// accepted silently and only shows up as an empty dashboard, with nothing
/// pointing at the cause.
fn validate_config_value(key: &str, value: &str) -> Result<(), String> {
    match key {
        "dashboard_custom_lists" => validate_dashboard_custom_lists(value),
        "holdings_custom_fields" | "watchlist_custom_fields" => validate_custom_fields(value),
        PORTFOLIO_HISTORY_START => validate_optional_date(value),
        // A delisting date is compared against bar dates as text, so a value in
        // any other shape would order wrongly rather than fail.
        key if key.starts_with(DEAD_SYMBOL_PREFIX) => validate_optional_date(value),
        _ => Ok(()),
    }
}

/// A date setting that may be cleared. Stored as text and compared as text, so
/// anything but YYYY-MM-DD would silently order wrongly rather than fail.
fn validate_optional_date(value: &str) -> Result<(), String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(());
    }
    NaiveDate::parse_from_str(trimmed, "%Y-%m-%d")
        .map(|_| ())
        .map_err(|_| format!("'{trimmed}' is not a date in YYYY-MM-DD form"))
}

fn validate_dashboard_custom_lists(value: &str) -> Result<(), String> {
    let parsed: serde_json::Value =
        serde_json::from_str(value).map_err(|err| format!("not valid JSON: {err}"))?;
    let items = parsed.as_array().ok_or("must be a JSON array of list definitions")?;
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();

    for (index, item) in items.iter().enumerate() {
        let at = |message: String| format!("list {}: {}", index + 1, message);
        let obj = item.as_object().ok_or_else(|| at("must be an object".into()))?;
        let text = |field: &str| {
            obj.get(field)
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
        };
        // An absent optional field and an explicit null mean the same thing.
        let optional = |field: &str| obj.get(field).filter(|v| !v.is_null());
        let one_of = |field: &str, allowed: &[&str], found: &str| -> Result<(), String> {
            if allowed.contains(&found) {
                Ok(())
            } else {
                Err(at(format!("{field} '{found}' is not one of: {}", allowed.join(", "))))
            }
        };

        let key = text("key").ok_or_else(|| at("key is required".into()))?;
        if !seen.insert(key) {
            return Err(at(format!("duplicate key '{key}' — keys identify a list to the client")));
        }
        text("label").ok_or_else(|| at("label is required".into()))?;

        one_of("source", &LIST_SOURCES, text("source").ok_or_else(|| at("source is required".into()))?)?;
        one_of("operator", &LIST_OPERATORS, text("operator").ok_or_else(|| at("operator is required".into()))?)?;

        let field_key = text("field_key").ok_or_else(|| at("field_key is required".into()))?;
        let (prefix, name) = field_key
            .split_once(':')
            .ok_or_else(|| at("field_key must look like holdings:key, watchlist:key or indicator:key".into()))?;
        match prefix {
            // Custom field names are user-defined and a list may be configured
            // before the field it points at exists, so only the shape is checked.
            "holdings" | "watchlist" => {
                if name.trim().is_empty() {
                    return Err(at(format!("field_key '{field_key}' names no field")));
                }
            }
            "indicator" => one_of("indicator", &LIST_INDICATORS, name)?,
            other => {
                return Err(at(format!(
                    "field_key prefix '{other}' is not holdings, watchlist or indicator"
                )))
            }
        }

        if let Some(compare) = optional("compare") {
            one_of("compare", &LIST_COMPARES, compare.as_str().ok_or_else(|| at("compare must be a string".into()))?)?;
        }
        if let Some(sort) = optional("sort") {
            one_of("sort", &LIST_SORTS, sort.as_str().ok_or_else(|| at("sort must be a string".into()))?)?;
        }
        if let Some(limit) = optional("limit") {
            let n = limit.as_u64().ok_or_else(|| at("limit must be a whole number".into()))?;
            if n == 0 || n > 100 {
                return Err(at(format!("limit {n} is outside 1-100")));
            }
        }
    }
    Ok(())
}

/// Custom field definitions are simpler — a key and a label each — but they
/// fail the same way, so they get the same guard.
fn validate_custom_fields(value: &str) -> Result<(), String> {
    let parsed: serde_json::Value =
        serde_json::from_str(value).map_err(|err| format!("not valid JSON: {err}"))?;
    let items = parsed.as_array().ok_or("must be a JSON array of field definitions")?;
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for (index, item) in items.iter().enumerate() {
        let at = |message: String| format!("field {}: {}", index + 1, message);
        let obj = item.as_object().ok_or_else(|| at("must be an object".into()))?;
        let text = |field: &str| {
            obj.get(field)
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
        };
        let key = text("key").ok_or_else(|| at("key is required".into()))?;
        if !seen.insert(key) {
            return Err(at(format!("duplicate key '{key}'")));
        }
        text("label").ok_or_else(|| at("label is required".into()))?;
    }
    Ok(())
}

/// Read a JSON config value, logging when it cannot be parsed.
///
/// The fallback is still the default — a broken blob must not take an endpoint
/// down — but it leaves a trail in the event log instead of an empty screen
/// with no explanation.
fn config_json<T: serde::de::DeserializeOwned + Default>(
    db_path: &PathBuf,
    config: &HashMap<String, String>,
    key: &str,
) -> T {
    let Some(raw) = config.get(key) else { return T::default() };
    match serde_json::from_str(raw) {
        Ok(parsed) => parsed,
        Err(err) => {
            let _ = insert_event_log(
                db_path,
                "error",
                "config_parse",
                "api",
                Some(key),
                &format!("Could not parse {key}, falling back to empty: {err}"),
            );
            T::default()
        }
    }
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

/// Constant-time token comparison: XOR-folds every byte instead of
/// returning at the first mismatch, so response timing doesn't reveal
/// how much of a guessed token was correct. The length check leaks only
/// the token's length.
fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// The bearer-token gate. No configured token means auth is disabled.
/// /api/health and /api/openapi.json stay reachable without a token so
/// clients can probe and discover the API; OPTIONS passes so CORS
/// preflights (which cannot carry credentials) succeed.
fn is_request_authorized(req: &actix_web::dev::ServiceRequest, expected: Option<&str>) -> bool {
    let Some(expected) = expected else { return true };
    req.method() == actix_web::http::Method::OPTIONS
        || req.path() == "/api/health"
        || req.path() == "/api/openapi.json"
        || req
            .headers()
            .get("authorization")
            .and_then(|h| h.to_str().ok())
            .and_then(|h| h.strip_prefix("Bearer "))
            .map(|t| constant_time_eq(t, expected))
            .unwrap_or(false)
}

/// Rewrite /api/v1/* to /api/* (query string preserved) so the versioned
/// surface is an alias of the unversioned one. Native clients pin the
/// stable v1 prefix; breaking changes get a new version.
fn rewrite_v1_alias(req: &mut actix_web::dev::ServiceRequest) {
    if let Some(rest) = req.path().strip_prefix("/api/v1/").map(str::to_owned) {
        let path_and_query = if req.query_string().is_empty() {
            format!("/api/{}", rest)
        } else {
            format!("/api/{}?{}", rest, req.query_string())
        };
        if let Ok(uri) = path_and_query.parse::<actix_web::http::Uri>() {
            req.head_mut().uri = uri.clone();
            // The router matches against the cached path object, not
            // head.uri — update both (as NormalizePath does).
            req.match_info_mut().get_mut().update(&uri);
        }
    }
}

/// Per-symbol metadata from the symbol_info table:
/// (instrument_type, long_name, currency)
type SymbolInfo = (Option<String>, Option<String>, Option<String>);

/// A dividend_events row: (symbol, ex_date, payment_date, amount)
type DividendEventRow = (String, String, Option<String>, f64);




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
                    && let Ok(meta) = fetch_current_price(&client, &sym_clone).await {
                        let _ = store_symbol_info(
                            &db_path_clone,
                            &sym_clone,
                            meta.instrument_type.as_deref(),
                            meta.long_name.as_deref(),
                            meta.currency.as_deref(),
                        );
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
    if let Err(reason) = validate_config_value(key, value) {
        let message = format!("Invalid {key}: {reason}");
        let _ = insert_event_log(&db_path, "warn", "config_update", "api", Some(key), &message);
        return err_bad_request(message);
    }

    match upsert_config(&db_path, key, value) {
        Ok(()) => {
            let _ = insert_event_log(&db_path, "info", "config_update", "api", Some(key), &format!("Updated config {}", key));
            if let Some(symbol) = key.strip_prefix(DEAD_SYMBOL_PREFIX).filter(|_| !value.is_empty()) {
                drop_cached_quote(&db_path, symbol);
            }
            HttpResponse::NoContent().finish()
        }
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "config_update", "api", Some(key), &err);
            err_internal(err)
        }
    }
}

/// Forget the last live quote for a symbol just marked dead.
///
/// Marking it dead stops the refresh fetching it, which also means nothing will
/// ever overwrite the quote already cached. Left in place it would be served as
/// the current price indefinitely — a delisted stock frozen at its final
/// trading day but presented as though it were still quoted today. Deleting it
/// drops the symbol onto the manual-price/last-close chain, which reports where
/// the figure came from.
fn drop_cached_quote(db_path: &PathBuf, symbol: &str) {
    let removed = open_db(db_path).and_then(|conn| {
        conn.execute("DELETE FROM cached_current_prices WHERE symbol = ?1", params![symbol])
    });
    match removed {
        Ok(n) if n > 0 => {
            let _ = insert_event_log(db_path, "info", "config_update", "api", Some(symbol),
                "Marked dead; discarded the cached quote so it is not served as current");
        }
        Ok(_) => {}
        Err(err) => {
            let _ = insert_event_log(db_path, "warn", "config_update", "api", Some(symbol),
                &format!("Marked dead but its cached quote could not be discarded: {err}"));
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
    let gmtoffset = result.meta.as_ref().and_then(|m| m.gmtoffset);
    for (i, ts) in timestamps.iter().enumerate() {
        let date_str = yahoo_local_date(*ts, gmtoffset)
            .map(|d| d.format("%Y-%m-%d").to_string())
            .unwrap_or_default();
        if date_str <= target_str
            && let Some(Some(rate)) = closes.get(i) {
                best = Some((*rate, date_str));
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
    // Prefer the stored rate history: it covers every past date, avoids a Yahoo
    // round trip per lookup, and returns the same rate the valuation code will
    // use. Only fall through to the network when the date is not yet stored.
    if let Ok(conn) = open_db(db_path.as_ref())
        && let Some(rate) = fx_rate_on(&conn, &currency, &query.date)
    {
        return HttpResponse::Ok().json(serde_json::json!({ "rate": rate, "date": query.date, "source": "stored" }));
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
/// Every table that keys rows by ticker, and so has to follow a symbol when it
/// is renamed.
///
/// `event_log` is deliberately absent. It records what happened at the time,
/// under the name in use then; rewriting it would falsify the history it exists
/// to preserve. That is the same reasoning that keeps the log tables out of the
/// audit triggers — see the Audit Logging section of CLAUDE.md.
const SYMBOL_KEYED_TABLES: &[&str] = &[
    "holdings_transactions",
    "holdings_symbol_fields",
    "symbol_info",
    "prices",
    "dividend_events",
    "dividend_exclusions",
    "chart_drawings",
    "cached_current_prices",
    "watchlist_symbols",
    "watchlist_memberships",
    "watchlist_prices",
    "watchlist_symbol_fields",
    "stock_analysis_messages",
];

/// Move every row keyed to `old_symbol` across to `new_symbol`, returning how
/// many `holdings_transactions` moved.
///
/// One rule covers every table: `UPDATE OR IGNORE` moves what it can, and where
/// the target already holds that key its own row wins, leaving the stale
/// duplicate to be deleted. Being column-blind is the point — copying row by
/// row meant naming each column, and a column added later was silently left
/// behind. That is exactly how a rename used to drop
/// `symbol_info.dividend_withholding_pct` and start crediting US distributions
/// gross again.
///
/// Table names come from the constant above, never from input, so interpolating
/// them into the statement is safe.
fn migrate_symbol_rows(tx: &Connection, old_symbol: &str, new_symbol: &str) -> Result<usize, String> {
    let mut holdings_moved = 0usize;
    for table in SYMBOL_KEYED_TABLES {
        // Some of these are legacy: `watchlist_prices` still holds rows in
        // databases created by older versions but is no longer built by
        // `init_db`. Touching a table that isn't there would abort the whole
        // rename mid-transaction, so absence is simply skipped.
        let present: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                params![table],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        if present == 0 {
            continue;
        }

        let moved = tx
            .execute(
                &format!("UPDATE OR IGNORE {} SET symbol = ?1 WHERE symbol = ?2", table),
                params![new_symbol, old_symbol],
            )
            .map_err(|e| format!("Renaming {} in {}: {}", old_symbol, table, e))?;
        if *table == "holdings_transactions" {
            holdings_moved = moved;
        }
        // Whatever could not move is a row the target already has under that
        // key; the old copy is stale either way.
        tx.execute(
            &format!("DELETE FROM {} WHERE symbol = ?1", table),
            params![old_symbol],
        )
        .map_err(|e| format!("Clearing {} from {}: {}", old_symbol, table, e))?;
    }
    Ok(holdings_moved)
}

fn rename_holdings_symbol(db_path: &PathBuf, old_symbol: &str, new_symbol: &str) -> Result<usize, String> {
    let mut conn = open_db(db_path).map_err(|e| e.to_string())?;
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    let affected = migrate_symbol_rows(&tx, old_symbol, new_symbol)?;
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
    if let Some(ref notes) = payload.notes
        && let Err(err) = conn.execute(
            "INSERT OR REPLACE INTO holdings_symbol_fields (symbol, field_key, value) VALUES (?1, '_notes', ?2)",
            params![symbol, notes],
        ) {
            let _ = insert_event_log(&db_path, "error", "holdings_symbol_fields_update", "api", Some(&symbol), &format!("Failed to save notes: {}", err));
            return err_internal(err.to_string());
        }
    if let Some(ref fields) = payload.custom_fields {
        if let Err(err) = upsert_holdings_symbol_fields(&conn, &symbol, fields) {
            return err_internal(err);
        }
        // The baseline price is resolved once, here, rather than derived on
        // every read: stored history is finite and gets trimmed, and a basis
        // that silently moves is not a record of anything. Clearing the date
        // clears the price with it.
        if let Some(date) = fields.get(PL_BASIS_DATE) {
            let resolved = if date.trim().is_empty() {
                String::new()
            } else {
                match close_on_or_after(&conn, &symbol, date.trim()) {
                    Some(close) => close.to_string(),
                    None => {
                        let message = format!("No stored price for {} on or after {}", symbol, date.trim());
                        let _ = insert_event_log(&db_path, "warn", "holdings_symbol_fields_update", "api", Some(&symbol), &message);
                        return err_unprocessable(message);
                    }
                }
            };
            let mut resolved_field = std::collections::HashMap::new();
            resolved_field.insert(PL_BASIS_PRICE.to_string(), resolved);
            if let Err(err) = upsert_holdings_symbol_fields(&conn, &symbol, &resolved_field) {
                return err_internal(err);
            }
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
    /// OHLC is absent for rows written before OHLC ingest existed, and for
    /// any bar Yahoo returns without it — clients must tolerate nulls.
    open: Option<f64>,
    high: Option<f64>,
    low: Option<f64>,
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
            let date = bar.price_date.clone().unwrap_or_else(|| Utc::now().format("%Y-%m-%d").to_string());
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
    meta: Option<YahooMeta>,
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

    if let Some(ev) = result.events
        && let Some(dividends) = ev.dividends {
            for entry in dividends.values() {
                let amount = entry.amount.unwrap_or(0.0);
                if amount <= 0.0 {
                    continue;
                }
                let ts = entry.ex_date.or(entry.date).ok_or_else(|| {
                    format!("Dividend entry missing date for {}", symbol)
                })?;
                let gmtoffset = result.meta.as_ref().and_then(|m| m.gmtoffset);
                let ex_date = yahoo_local_date(ts, gmtoffset)
                    .ok_or_else(|| format!("Invalid timestamp {} for {}", ts, symbol))?;
                let payment_date = entry.payment_date.and_then(|t| yahoo_local_date(t, gmtoffset));
                let record_date = entry.record_date.and_then(|t| yahoo_local_date(t, gmtoffset));
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

    events.sort_by_key(|e| e.ex_date);
    Ok(events)
}

/// Days within which two identical amounts are taken to be one distribution.
/// Yahoo has been seen listing a Vanguard quarterly three days apart.
const DIVIDEND_DUPLICATE_WINDOW_DAYS: i64 = 4;

/// Collapse the same distribution reported more than once.
///
/// Yahoo's feed genuinely repeats some events: VAE.AX comes back with
/// timestamps exactly 86400 apart carrying an identical 0.677876, which is one
/// distribution described twice, not two payments. Left alone it is paid twice
/// into the cash ledger. Duplicates are matched on an identical amount within a
/// few days, and the earliest is kept — that is the real ex-date, the later
/// copy being the artefact.
fn dedupe_dividend_events(events: &[DividendEvent]) -> Vec<DividendEvent> {
    let mut sorted: Vec<DividendEvent> = events.to_vec();
    sorted.sort_by(|a, b| a.ex_date.cmp(&b.ex_date));

    let mut kept: Vec<DividendEvent> = Vec::with_capacity(sorted.len());
    for event in sorted {
        let is_repeat = kept.iter().any(|k| {
            (k.amount - event.amount).abs() < 1e-6
                && (event.ex_date - k.ex_date).num_days() <= DIVIDEND_DUPLICATE_WINDOW_DAYS
        });
        if !is_repeat {
            kept.push(event);
        }
    }
    kept
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

    // An empty fetch is a failure to learn anything, not news that the symbol
    // never paid a dividend, so it must not be allowed to erase real history.
    if events.is_empty() {
        return Ok(());
    }
    let events = dedupe_dividend_events(events);
    let events = &events[..];

    let tx = conn.transaction().map_err(|e| e.to_string())?;
    {
        // Yahoo returns the symbol's complete dividend history, so the fetch is
        // the authority: a stored row it no longer reports is stale, not merely
        // unrefreshed. Upserting alone made this table grow-only — when a
        // date-handling change moved ex-dates by a day, every refresh added a
        // second copy of every dividend rather than correcting the first,
        // because the key is (symbol, ex_date). Replacing the symbol's whole
        // set is what makes a refresh converge.
        tx.execute("DELETE FROM dividend_events WHERE symbol = ?1", params![symbol])
            .map_err(|e| e.to_string())?;

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

/// Symbols sold out of entirely — nothing held, but at least one sale.
///
/// Price collection otherwise follows what is held and what is watched, so an
/// exited position stops being priced the day it leaves the watchlist. The
/// Hindsight screen's whole question is what the stock did *after* the sale, so
/// these have to keep being fetched: the quote answers "current price", and the
/// daily bar each fetch also writes is what lets the +1 week, +6 week, +3 month
/// and peak-since windows fill in as time passes.
fn load_exited_symbols(db_path: &PathBuf) -> Result<Vec<String>, String> {
    let conn = open_db(db_path).map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare(
            "SELECT symbol,
                    SUM(CASE WHEN transaction_type='purchase' THEN quantity ELSE -quantity END) AS net_qty,
                    SUM(CASE WHEN transaction_type='sale' THEN 1 ELSE 0 END) AS sales
             FROM holdings_transactions
             WHERE transaction_type IN ('purchase', 'sale')
             GROUP BY symbol
             HAVING net_qty <= 0 AND sales > 0
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

    let total = symbols.len();
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
                Ok(events) => match store_dividend_events_for_symbol(db_path, &symbol, &events) {
                    Ok(()) => {
                        updated += 1;
                    }
                    Err(err) => {
                        let _ = insert_event_log(db_path, "error", "dividend_fetch", "api", Some(&symbol), &err);
                        errors.push(format!("{}: {}", symbol, err));
                    }
                },
                Err(err) => {
                    let _ = insert_event_log(db_path, "error", "dividend_fetch", "api", Some(&symbol), &err);
                    errors.push(format!("{}: {}", symbol, err));
                }
            }
        }
    }

    // One summary row instead of one info row per symbol — keeps event_log
    // growth proportional to refreshes, not portfolio size. Errors still log
    // per symbol above.
    let _ = insert_event_log(
        db_path,
        "info",
        "dividend_fetch",
        "api",
        None,
        &format!("Dividend refresh complete: {}/{} symbols updated, {} error(s)", updated, total, errors.len()),
    );

    DividendRefreshResult { updated, errors }
}

/// Outcome of materialising fetched dividend events as transactions.
#[derive(Serialize, Default)]
struct DividendRecordResult {
    recorded: usize,
    /// Already had a transaction.
    already_present: usize,
    /// Declined by the user; see `dividend_exclusions`.
    excluded: usize,
    /// Currencies with no configured destination account, so nothing was
    /// recorded for them.
    unconfigured_currencies: Vec<String>,
    errors: Vec<String>,
}

/// Where dividends in `currency` are paid, from `app_config`.
///
/// Keyed by currency because that is how the accounts actually divide: AUD
/// distributions land in the everyday investment account, USD ones in the
/// foreign broker. A currency with no key configured is left alone rather than
/// guessed at — inventing a destination would move real money to the wrong
/// place.
fn dividend_account_for_currency(conn: &Connection, currency: &str) -> Option<i64> {
    conn.query_row(
        "SELECT value FROM app_config WHERE key = ?1",
        params![format!("dividend_account_{}", currency.to_uppercase())],
        |r| r.get::<_, String>(0),
    )
    .optional()
    .ok()
    .flatten()
    .and_then(|v| v.trim().parse::<i64>().ok())
}

/// Record every fetched dividend event the holder was entitled to and has not
/// already recorded or declined.
///
/// Runs after each dividend refresh so a newly fetched event reaches the cash
/// ledger on its own. Previously this was a one-off script, and anything
/// fetched afterwards sat in the Transactions screen as a derived row with no
/// account, invisible in the cash balance until someone remembered to re-run
/// it.
fn record_new_dividends(db_path: &PathBuf) -> Result<DividendRecordResult, String> {
    let mut result = DividendRecordResult::default();
    let conn = open_db(db_path).map_err(|e| e.to_string())?;

    let events: Vec<(String, String, f64)> = {
        let mut stmt = conn
            .prepare(
                "SELECT e.symbol, e.ex_date, e.amount
                   FROM dividend_events e
                  WHERE NOT EXISTS (SELECT 1 FROM holdings_transactions h
                                     WHERE h.symbol = e.symbol AND h.date = e.ex_date
                                       AND h.transaction_type = 'dividend')
                  ORDER BY e.ex_date, e.symbol",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .map_err(|e| e.to_string())?;
        rows.filter_map(|r| r.ok()).collect()
    };

    // One read of the ledger, grouped by symbol — the entitlement check runs
    // per event and there can be hundreds.
    let all_txs = fetch_holdings(db_path)?;
    let mut by_symbol: HashMap<String, Vec<HoldingTransaction>> = HashMap::new();
    for tx in all_txs {
        by_symbol.entry(tx.symbol.clone()).or_default().push(tx);
    }

    let mut unconfigured: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (symbol, ex_date, per_share) in events {
        let excluded: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM dividend_exclusions WHERE symbol = ?1 AND ex_date = ?2",
                params![symbol, ex_date],
                |r| r.get(0),
            )
            .unwrap_or(0);
        if excluded > 0 {
            result.excluded += 1;
            continue;
        }

        // Entitlement follows the shares held at the ex-date, using the same
        // convention as the rest of the engine.
        let Some(txs) = by_symbol.get(&symbol) else { continue };
        let shares = portfolio::shares_on_date(&to_portfolio_txs(txs), &ex_date);
        if shares <= 0.0 {
            continue;
        }

        // The currency the symbol trades in, taken from its own transactions
        // so it matches how the trades were recorded.
        let currency = txs
            .iter()
            .find(|t| !t.currency.is_empty())
            .map(|t| t.currency.to_uppercase())
            .unwrap_or_else(|| "AUD".to_string());
        let Some(account_id) = dividend_account_for_currency(&conn, &currency) else {
            unconfigured.insert(currency);
            continue;
        };

        let mut payload = NewHoldingTransaction {
            symbol: symbol.clone(),
            transaction_type: "dividend".to_string(),
            date: ex_date.clone(),
            quantity: Some(shares),
            price: Some(per_share),
            amount: Some(shares * per_share),
            brokerage: None,
            notes: None,
            currency: Some(currency.clone()),
            original_price: None,
            fx_rate: None,
            custom_fields: None,
            cash_account_id: Some(account_id),
            withholding_amount: None,
            confirm: None,
        };
        if currency != "AUD" {
            // `amount` and `price` are stored in AUD, as they are for a trade;
            // the native figures ride alongside so a foreign-currency account
            // can be credited in its own currency.
            let Some(rate) = fx_rate_on(&conn, &currency, &ex_date) else {
                result.errors.push(format!("{} {}: no {}/AUD rate", symbol, ex_date, currency));
                continue;
            };
            payload.original_price = Some(per_share);
            payload.price = Some(per_share * rate);
            payload.fx_rate = Some(rate);
            payload.amount = Some(shares * per_share * rate);
        }

        match insert_holding_transaction(db_path, &symbol, payload) {
            Ok(_) => result.recorded += 1,
            Err(err) => result.errors.push(format!("{} {}: {}", symbol, ex_date, err)),
        }
    }

    result.unconfigured_currencies = {
        let mut v: Vec<String> = unconfigured.into_iter().collect();
        v.sort();
        v
    };
    for ccy in &result.unconfigured_currencies {
        let _ = insert_event_log(
            db_path,
            "warn",
            "dividend_record",
            "api",
            None,
            &format!(
                "Dividends in {} were not recorded: set app_config key 'dividend_account_{}' to the destination account id",
                ccy, ccy
            ),
        );
    }
    if result.recorded > 0 {
        let _ = insert_event_log(db_path, "info", "dividend_record", "api", None,
            &format!("Recorded {} newly fetched dividend(s)", result.recorded));
    }
    Ok(result)
}

/// Fetch result plus what recording those events produced.
///
/// Recording runs on the same request as the fetch: an event that arrives and
/// is not immediately recorded shows in the Transactions screen as a derived
/// row with no cash account, which reads as a bug rather than as pending work.
fn with_recorded_dividends(db_path: &PathBuf, fetched: DividendRefreshResult) -> serde_json::Value {
    let recorded = match record_new_dividends(db_path) {
        Ok(r) => r,
        Err(err) => {
            let _ = insert_event_log(db_path, "error", "dividend_record", "api", None, &err);
            DividendRecordResult { errors: vec![err], ..Default::default() }
        }
    };
    serde_json::json!({
        "updated": fetched.updated,
        "errors": fetched.errors,
        "recorded": recorded,
    })
}

#[utoipa::path(post, path = "/api/v1/dividends/refresh", tag = "dividends", responses((status = 200, description = "Refresh dividends")))]
#[post("/api/dividends/refresh")]
async fn refresh_dividends(db_path: web::Data<PathBuf>) -> impl Responder {
    match load_holding_symbols(&db_path) {
        Ok(symbols) => {
            let fetched = refresh_dividends_for_symbols(&db_path, symbols).await;
            HttpResponse::Ok().json(with_recorded_dividends(&db_path, fetched))
        }
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
        Ok(symbols) => {
            let fetched = refresh_dividends_for_symbols(&db_path, symbols).await;
            HttpResponse::Ok().json(with_recorded_dividends(&db_path, fetched))
        }
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
    if let Some(fields) = payload.custom_fields.as_ref()
        && let Ok(conn) = open_db(db_path.as_ref())
            && let Err(err) = save_custom_fields(&conn, &symbol, fields) {
                let _ = insert_event_log(&db_path, "error", "watchlist_update", "api", Some(&symbol), &err);
                return err_internal(err);
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

    // Grouped once, to test entitlement per event below.
    let mut by_symbol: HashMap<String, Vec<HoldingTransaction>> = HashMap::new();
    let mut manual_dividend_keys: std::collections::HashSet<String> = std::collections::HashSet::new();
    for tx in &txs {
        by_symbol.entry(tx.symbol.clone()).or_default().push(tx.clone());
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
            "cash_account_id": tx.cash_account_id,
            "per_share": false,
            "payment_date": serde_json::Value::Null,
            "custom_fields": tx.custom_fields,
        }))
        .collect();

    let events_result = (|| -> Result<Vec<DividendEventRow>, String> {
        let conn = open_db(db_path.as_ref()).map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare("SELECT symbol, ex_date, payment_date, amount FROM dividend_events ORDER BY ex_date DESC")
            .map_err(|e| e.to_string())?;
        let event_rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, Option<String>>(2)?, row.get::<_, f64>(3)?))
            })
            .map_err(|e| e.to_string())?;
        event_rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
    })();
    match events_result {
        Ok(events) => {
            for (symbol, ex_date, payment_date, amount) in events {
                // Show an event only where the shares were actually held on
                // the ex-date. Testing only "on or after the first purchase"
                // guarded the wrong end: a dividend declared after the holding
                // was sold still appeared, as a row that could never be
                // recorded because there was no entitlement to record.
                let Some(symbol_txs) = by_symbol.get(&symbol) else { continue };
                if portfolio::shares_on_date(&to_portfolio_txs(symbol_txs), &ex_date) <= 0.0 {
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
                    // Synthetic rows from dividend_events, not stored trades, so
                    // they have no settlement account to carry.
                    "cash_account_id": serde_json::Value::Null,
                    "per_share": true,
                    "payment_date": payment_date,
                    "custom_fields": serde_json::json!({}),
                }));
            }
        }
        Err(err) => {
            // The ledger degrades to transactions-only — record why
            let _ = insert_event_log(&db_path, "warn", "ledger_fetch", "api", None, &format!("Dividend events unavailable for ledger: {}", err));
        }
    }

    rows.sort_by(|a, b| b["date"].as_str().unwrap_or("").cmp(a["date"].as_str().unwrap_or("")));
    HttpResponse::Ok().json(serde_json::json!({ "rows": rows }))
}

#[derive(Deserialize)]
struct RefreshQuery {
    force: Option<bool>,
}

/// Only one refresh may run at a time — a second caller gets a skip
/// response instead of doubling the Yahoo traffic.
static REFRESH_IN_FLIGHT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether a completed refresh should stamp `last_full_refresh_at`.
/// Stamp when the refresh achieved something, or when there was nothing to
/// do — but a total failure (e.g. Yahoo down) must leave the stamp unset so
/// the next attempt isn't debounced into a stale-data window.
fn refresh_should_stamp(attempted_any: bool, did_any_work: bool) -> bool {
    did_any_work || !attempted_any
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
        if let Some(last) = last
            && let Ok(t) = chrono::DateTime::parse_from_rfc3339(&last)
                && (Utc::now() - t.with_timezone(&Utc)).num_seconds() < DEBOUNCE_SECS {
                    return HttpResponse::Ok().json(serde_json::json!({ "skipped": true, "last_refreshed_at": last }));
                }
    }
    if REFRESH_IN_FLIGHT
        .compare_exchange(false, true, std::sync::atomic::Ordering::SeqCst, std::sync::atomic::Ordering::SeqCst)
        .is_err()
    {
        return HttpResponse::Ok().json(serde_json::json!({ "skipped": true, "reason": "refresh_in_progress" }));
    }

    let mut errors: Vec<String> = Vec::new();
    let mut watchlist_ok = 0;
    let mut holdings_ok = 0;

    let watchlist_count = match fetch_watchlist_current_prices(&db_path, None).await {
        Ok(prices) => {
            watchlist_ok = prices.iter().filter(|p| p.error.is_none()).count();
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
                holdings_ok = prices.iter().filter(|p| p.error.is_none()).count();
                errors.extend(prices.iter().filter_map(|p| p.error.clone()));
                prices.len()
            }
            Err(err) => {
                errors.push(err);
                0
            }
        }
    };

    // Sold positions, so the Hindsight windows keep filling. Their own stamp:
    // a stale quote on something sold last year is far less urgent than one on
    // a holding, and separating them makes that visible rather than assumed.
    let mut exited_ok = 0;
    let exited_symbols = load_exited_symbols(&db_path).unwrap_or_default();
    let exited_count = if exited_symbols.is_empty() {
        0
    } else {
        match fetch_and_cache_current_prices(&db_path, &exited_symbols, "sold_prices_updated_at").await {
            Ok(prices) => {
                exited_ok = prices.iter().filter(|p| p.error.is_none()).count();
                // A delisted symbol errors on every run for good reason, so its
                // failure does not belong in the banner the user sees.
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

    let attempted_any = watchlist_count > 0 || holdings_count > 0;
    let did_any_work = watchlist_ok > 0 || holdings_ok > 0 || dividends.updated > 0;
    if refresh_should_stamp(attempted_any, did_any_work)
        && let Err(err) = upsert_config(&db_path, "last_full_refresh_at", &Utc::now().to_rfc3339()) {
            let _ = insert_event_log(&db_path, "error", "refresh_all", "api", None, &err);
        }
    REFRESH_IN_FLIGHT.store(false, std::sync::atomic::Ordering::SeqCst);

    let _ = insert_event_log(&db_path, "info", "refresh_all", "api", None, &format!("Refreshed {} watchlist prices, {} holdings prices, {} sold prices, dividends for {} symbols ({} error(s))", watchlist_ok, holdings_ok, exited_ok, dividends.updated, errors.len()));

    HttpResponse::Ok().json(serde_json::json!({
        "skipped": false,
        "watchlist_prices": watchlist_count,
        "holdings_prices": holdings_count,
        "sold_prices": exited_count,
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
        )
            && let Some(price) = row.0 {
                let mut line = format!("Current price: ${:.2}", price);
                if let Some(chg) = row.1 { line.push_str(&format!(", change: {:.2}", chg)); }
                if let Some(pct) = row.2 { line.push_str(&format!(" ({:.2}%)", pct)); }
                if let Some(vol) = row.3 { line.push_str(&format!(", volume: {}", vol)); }
                if let Some(ref date) = row.4 { line.push_str(&format!(", as of {}", date)); }
                context_parts.push(line);
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
        if let Some(ref mut stmt) = fields_stmt
            && let Ok(rows) = stmt.query_map(params![symbol], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))) {
                let fields: Vec<String> = rows.filter_map(|r| r.ok()).map(|(k, v)| format!("{}: {}", k, v)).collect();
                if !fields.is_empty() { context_parts.push(format!("Watchlist fields: {}", fields.join(", "))); }
            }
        let mut hf_stmt = conn.prepare("SELECT field_key, value FROM holdings_symbol_fields WHERE symbol = ?1").ok();
        if let Some(ref mut stmt) = hf_stmt
            && let Ok(rows) = stmt.query_map(params![symbol], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))) {
                let fields: Vec<String> = rows.filter_map(|r| r.ok()).map(|(k, v)| format!("{}: {}", k, v)).collect();
                if !fields.is_empty() { context_parts.push(format!("Holdings fields: {}", fields.join(", "))); }
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
    if let Some(last_msg) = payload.messages.last()
        && let Ok(conn) = open_db(db_path.as_ref()) {
            let _ = conn.execute(
                "INSERT INTO stock_analysis_messages (symbol, role, content, created_at) VALUES (?1, ?2, ?3, ?4)",
                params![symbol, last_msg.role, last_msg.content, now],
            );
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
            if block["type"] == "text"
                && let Some(text) = block["text"].as_str() {
                    result_text.push_str(text);
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
        get_portfolio_sold, get_portfolio_risk, get_hindsight,
        get_watchlist, get_watchlist_lists, get_watchlist_enriched,
        add_watchlist_symbol, update_watchlist_symbol, delete_watchlist_symbol,
        rename_watchlist_list, update_watchlist_symbol_lists,
        get_watchlist_prices, get_watchlist_cached_prices,
        get_holdings, add_holding_transaction, update_holding_transaction,
        delete_holding_transaction, rename_holding_symbol,
        add_holding_from_watchlist, update_holdings_symbol_fields,
        get_holdings_symbol_fields, get_transactions_ledger,
        get_cached_prices, get_current_prices, get_price_history,
        get_symbol_info, get_fx_rate_for_date, get_fx_rates, post_fx_sync,
        get_cash_accounts, add_cash_account, update_cash_account, delete_cash_account,
        get_cash_transactions, add_cash_transaction, update_cash_transaction,
        delete_cash_transaction, add_cash_transfer, get_portfolio_history,
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
        std::io::Error::other(err)
    })?;

    // Keep stored FX rate history current. Spawned rather than awaited so a
    // slow or unreachable Yahoo cannot delay the server binding its port; a
    // failure is logged to event_log and retried on the next start.
    {
        let db_path = db_path.clone();
        tokio::spawn(async move {
            let report = sync_fx_history(&db_path, 600).await;
            if report.fetched > 0 || !report.errors.is_empty() {
                log::info!(
                    "FX sync: {} pair(s) updated, {} already current, {} bar(s) written, {} error(s)",
                    report.fetched, report.skipped, report.bars_written, report.errors.len()
                );
            }
        });
    }

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
                let authorized = is_request_authorized(&req, token.as_deref());
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
                rewrite_v1_alias(&mut req);
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
            .service(post_fx_sync)
            .service(get_cash_accounts)
            .service(export_cash_account_csv)
            .service(get_chart_drawings)
            .service(add_chart_drawing)
            .service(delete_chart_drawing)
            .service(move_chart_drawing)
            .service(add_cash_account)
            .service(update_cash_account)
            .service(delete_cash_account)
            .service(get_cash_transactions)
            .service(add_cash_transaction)
            .service(update_cash_transaction)
            .service(delete_cash_transaction)
            .service(add_cash_transfer)
            .service(get_portfolio_history)
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
            .service(get_hindsight)
            .service(get_meta)
            .service(get_sync_state)
            .service(openapi_spec)
    })
    .bind(bind)?
    .run()
    .await
}



fn normalize_symbol(symbol: &str) -> String {
    symbol.trim().to_uppercase()
}

fn load_custom_fields(conn: &Connection, symbol: &str) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    if let Ok(mut stmt) = conn.prepare("SELECT field_key, value FROM watchlist_symbol_fields WHERE symbol = ?1")
        && let Ok(rows) = stmt.query_map(params![symbol], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))) {
            for row in rows.flatten() { map.insert(row.0, row.1); }
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
    // One grouped query for all custom fields instead of one query per row
    // (same pattern as load_holdings_symbol_fields).
    let mut fields_stmt = conn
        .prepare("SELECT symbol, field_key, value FROM watchlist_symbol_fields")
        .map_err(|err| err.to_string())?;
    let field_rows = fields_stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?))
        })
        .map_err(|err| err.to_string())?;
    let mut fields_by_symbol: std::collections::HashMap<String, std::collections::HashMap<String, String>> = std::collections::HashMap::new();
    for row in field_rows {
        let (symbol, key, val) = row.map_err(|err| err.to_string())?;
        fields_by_symbol.entry(symbol).or_default().insert(key, val);
    }
    for row in &mut rows {
        if let Some(fields) = fields_by_symbol.get(&row.symbol) {
            row.custom_fields = fields.clone();
        }
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
        // Both of these used to swallow their errors, and the count defaulted to
        // zero — so a failed read looked exactly like "no memberships left" and
        // took the delete branch, destroying the symbol's notes, breakthrough
        // price and stop loss. Those are the columns this project has already
        // lost once; a count that cannot be read is not a count of nothing.
        let remaining: i64 = conn
            .query_row("SELECT COUNT(*) FROM watchlist_memberships WHERE symbol = ?1", params![sym], |row| row.get(0))
            .map_err(|err| format!("Counting remaining memberships for {}: {}", sym, err))?;
        if remaining == 0 {
            conn.execute("DELETE FROM watchlist_symbols WHERE symbol = ?1", params![sym])
                .map_err(|err| format!("Removing symbol row for {}: {}", sym, err))?;
        }
    }
    Ok(affected > 0)
}

fn fetch_holdings(db_path: &PathBuf) -> Result<Vec<HoldingTransaction>, String> {
    let conn = open_db(db_path).map_err(|err| err.to_string())?;
    let mut stmt = conn
        .prepare(
            "SELECT id, symbol, transaction_type, date, quantity, price, amount, brokerage, notes, created_at, currency, original_price, fx_rate, cash_account_id
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
                cash_account_id: row.get(13)?,
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
                    .and_then(|date| NaiveDate::parse_from_str(&date, "%Y-%m-%d").ok()),
                record_date: record_date_str
                    .and_then(|date| NaiveDate::parse_from_str(&date, "%Y-%m-%d").ok()),
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

// The shares-held ledger walk lives in stocks::portfolio (shared with the
// dividends daemon); these wrappers only adapt the API's row types.
fn calculate_dividend_payments(
    transactions: &[HoldingTransaction],
    events: &[DividendEvent],
) -> Vec<DividendPayment> {
    let txs = to_portfolio_txs(transactions);
    events
        .iter()
        .filter_map(|event| {
            let shares_held = portfolio::shares_on_date(&txs, &event.ex_date.format("%Y-%m-%d").to_string());
            (shares_held > 0.0).then(|| DividendPayment {
                symbol: event.symbol.clone(),
                ex_date: event.ex_date,
                payment_date: event.payment_date,
                amount_per_share: event.amount,
                shares_held,
                total_payment: shares_held * event.amount,
            })
        })
        .collect()
}

/// Test-only adapter: production callers go through
/// calculate_dividend_payments; the shares-on-date tests exercise the
/// engine through the same row conversion.
#[cfg(test)]
fn calculate_shares_on_date(transactions: &[HoldingTransaction], date: NaiveDate) -> f64 {
    portfolio::shares_on_date(&to_portfolio_txs(transactions), &date.format("%Y-%m-%d").to_string())
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
        "INSERT INTO holdings_transactions (symbol, transaction_type, date, quantity, price, amount, brokerage, notes, created_at, currency, original_price, fx_rate, cash_account_id, withholding_amount)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
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
            transaction.cash_account_id,
            transaction.withholding_amount,
        ],
    )
    .map_err(|err| err.to_string())?;

    let id = conn.last_insert_rowid();
    sync_trade_cash_leg(&conn, id)?;

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
            "SELECT id, symbol, transaction_type, date, quantity, price, amount, brokerage, notes, created_at, currency, original_price, fx_rate, cash_account_id
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
                cash_account_id: row.get(13)?,
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
        "UPDATE holdings_transactions SET symbol = ?1, transaction_type = ?2, date = ?3, quantity = ?4, price = ?5, amount = ?6, brokerage = ?7, notes = ?8, currency = ?9, original_price = ?10, fx_rate = ?11, cash_account_id = ?13, withholding_amount = COALESCE(?14, withholding_amount) WHERE id = ?12",
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
            transaction.cash_account_id,
            transaction.withholding_amount,
        ],
    )
    .map_err(|err| err.to_string())?;

    // Rewrite the settlement leg from the row as just stored: an edited price,
    // quantity or account has to flow through, and clearing the account removes
    // the leg entirely.
    sync_trade_cash_leg(&conn, id)?;

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
            "SELECT id, symbol, transaction_type, date, quantity, price, amount, brokerage, notes, created_at, currency, original_price, fx_rate, cash_account_id
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
                cash_account_id: row.get(13)?,
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
    // Captured before the delete so the exclusion below knows what went.
    let doomed: Option<(String, String)> = conn
        .query_row(
            "SELECT symbol, date FROM holdings_transactions WHERE id = ?1 AND transaction_type = 'dividend'",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(|err| err.to_string())?;
    conn.execute("DELETE FROM holdings_custom_fields WHERE transaction_id = ?1", params![id])
        .map_err(|err| err.to_string())?;
    // The settlement leg belongs to the trade; leaving it would strand cash
    // movements against a trade that no longer exists.
    conn.execute("DELETE FROM cash_transactions WHERE holding_tx_id = ?1", params![id])
        .map_err(|err| err.to_string())?;
    let affected = conn
        .execute("DELETE FROM holdings_transactions WHERE id = ?1", params![id])
        .map_err(|err| err.to_string())?;

    // Deleting a dividend that came from a fetched event has to mean "not this
    // one". Without recording that, the next refresh would helpfully record it
    // again, and the deletion would look like it silently failed.
    if let Some((symbol, date)) = doomed {
        let is_event: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM dividend_events WHERE symbol = ?1 AND ex_date = ?2",
                params![symbol, date],
                |r| r.get(0),
            )
            .unwrap_or(0);
        if is_event > 0 {
            conn.execute(
                "INSERT OR IGNORE INTO dividend_exclusions (symbol, ex_date, reason, created_at)
                 VALUES (?1, ?2, 'deleted by user', ?3)",
                params![symbol, date, Utc::now().to_rfc3339()],
            )
            .map_err(|err| err.to_string())?;
        }
    }
    Ok(affected > 0)
}

// ---------------------------------------------------------------------------
// Portfolio value over time
// ---------------------------------------------------------------------------

/// A symbol's or currency's closes, oldest first, with a cursor for the sweep.
struct SeriesCursor {
    points: Vec<(String, f64)>,
    index: usize,
    current: Option<f64>,
}

impl SeriesCursor {
    fn new(points: Vec<(String, f64)>) -> Self {
        Self { points, index: 0, current: None }
    }

    /// Advance to `date`, returning the latest close at or before it. Values
    /// carry forward, so weekends, holidays and missing bars hold the last
    /// traded price rather than dropping the holding out of the valuation.
    fn value_on(&mut self, date: &str) -> Option<f64> {
        while self.index < self.points.len() && self.points[self.index].0.as_str() <= date {
            self.current = Some(self.points[self.index].1);
            self.index += 1;
        }
        self.current
    }

    /// True once `date` is past the last stored bar — where a delisted or
    /// suspended holding stops having real prices and a manual valuation, if
    /// one is set, takes over.
    fn is_past_end(&self, date: &str) -> bool {
        match self.points.last() {
            Some((last, _)) => date > last.as_str(),
            None => true,
        }
    }
}

/// Symbols the market no longer trades, keyed by symbol, to the last date each
/// one traded.
///
/// A delisted ticker is not a transient fetch failure: Yahoo answers 404 for it
/// on every run, forever. Left unmarked it burns a request per refresh and
/// writes an error to the event log each time — JLG.AX and CCLD.AX between them
/// account for 639 such rows — which buries the failures that do mean something.
fn dead_symbols(conn: &Connection) -> HashMap<String, String> {
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
fn manual_prices(conn: &Connection) -> HashMap<String, f64> {
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
fn cached_quote_prices(conn: &Connection) -> HashMap<String, f64> {
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
fn latest_closes(db_path: &PathBuf, symbols: &[String]) -> HashMap<String, (f64, String)> {
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

fn load_close_series(conn: &Connection, symbol: &str) -> Vec<(String, f64)> {
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

#[derive(Serialize)]
struct HistoryPoint {
    date: String,
    stocks: f64,
    cash: f64,
    total: f64,
    flow: f64,
}

#[derive(Deserialize)]
struct HistoryQuery {
    from: Option<String>,
    to: Option<String>,
}

/// Daily portfolio value in AUD, with the external flows needed to measure
/// return.
///
/// A trade that names no cash account is treated as externally funded: the
/// shares appear with no matching cash movement, so without this the purchase
/// would look like value materialising from nowhere and inflate the return.
/// Every transaction recorded before the cash ledger existed is in that state,
/// which is what makes the full history usable rather than only the part after
/// the ledger starts.
///
/// The first element is an **anchor**: the day before the requested window,
/// carrying the value the window opens with. Without it the first day inside
/// the window would have nothing to be compared against, so its movement would
/// drop out of the return, and its own contributions would be double counted —
/// once in the opening value and again in the contribution total.
fn build_portfolio_history(
    conn: &Connection,
    from: Option<&str>,
    to: Option<&str>,
) -> Result<Vec<portfolio::DailyValue>, String> {
    #[derive(Clone)]
    struct Trade {
        date: String,
        symbol: String,
        tx_type: String,
        quantity: f64,
        price_aud: f64,
        brokerage_aud: f64,
        has_cash_leg: bool,
    }

    let mut stmt = conn
        .prepare(
            "SELECT date, symbol, transaction_type, COALESCE(quantity, 0), COALESCE(price, 0),
                    COALESCE(brokerage, 0), cash_account_id
               FROM holdings_transactions ORDER BY date, id",
        )
        .map_err(|e| e.to_string())?;
    let trades: Vec<Trade> = stmt
        .query_map([], |row| {
            Ok(Trade {
                date: row.get(0)?,
                symbol: row.get(1)?,
                tx_type: row.get(2)?,
                quantity: row.get(3)?,
                price_aud: row.get(4)?,
                brokerage_aud: row.get(5)?,
                has_cash_leg: row.get::<_, Option<i64>>(6)?.is_some(),
            })
        })
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();

    // Only accounts counted toward the portfolio; an everyday savings account
    // can be tracked without being treated as invested capital.
    let mut account_stmt = conn
        .prepare("SELECT id, currency FROM cash_accounts WHERE include_in_portfolio = 1")
        .map_err(|e| e.to_string())?;
    let accounts: HashMap<i64, String> = account_stmt
        .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)))
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();

    let mut cash_stmt = conn
        .prepare("SELECT date, account_id, amount, kind FROM cash_transactions ORDER BY date, id")
        .map_err(|e| e.to_string())?;
    let cash_txs: Vec<(String, i64, f64, String)> = cash_stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)))
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .filter(|(_, account_id, _, _)| accounts.contains_key(account_id))
        .collect();

    if trades.is_empty() && cash_txs.is_empty() {
        return Ok(Vec::new());
    }

    let earliest = trades
        .iter()
        .map(|t| t.date.clone())
        .chain(cash_txs.iter().map(|c| c.0.clone()))
        .min()
        .unwrap_or_default();
    let start = from.map(str::to_string).unwrap_or(earliest);
    let end = to.map(str::to_string).unwrap_or_else(|| Utc::now().format("%Y-%m-%d").to_string());
    let (Ok(start_date), Ok(end_date)) = (
        NaiveDate::parse_from_str(&start, "%Y-%m-%d"),
        NaiveDate::parse_from_str(&end, "%Y-%m-%d"),
    ) else {
        return Err("Invalid date range. Use YYYY-MM-DD.".to_string());
    };
    if end_date < start_date {
        return Err("`to` must not be before `from`".to_string());
    }

    // Price and rate cursors, one per symbol and per currency.
    let symbols: Vec<String> = {
        let mut seen: Vec<String> = trades.iter().map(|t| t.symbol.clone()).collect();
        seen.sort();
        seen.dedup();
        seen
    };
    let symbol_currency: HashMap<String, String> = {
        let mut stmt = conn
            .prepare("SELECT symbol, COALESCE(currency, 'AUD') FROM symbol_info")
            .map_err(|e| e.to_string())?;
        stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
            .map_err(|e| e.to_string())?
            .filter_map(|r| r.ok())
            .collect()
    };
    let manual = manual_prices(conn);
    let quotes = cached_quote_prices(conn);
    // A stored bar exists for today as soon as the fetcher has run, but it is
    // the day's close-so-far, not the live quote the rest of the app shows. On
    // today's point only, the quote wins; every earlier day stays on its bar.
    let today = Utc::now().format("%Y-%m-%d").to_string();
    let mut prices: HashMap<String, SeriesCursor> = symbols
        .iter()
        .map(|s| (s.clone(), SeriesCursor::new(load_close_series(conn, s))))
        .collect();

    let mut currencies: Vec<String> = symbol_currency.values().cloned().collect();
    currencies.extend(accounts.values().cloned());
    currencies.sort();
    currencies.dedup();
    let mut rates: HashMap<String, SeriesCursor> = currencies
        .iter()
        .filter(|c| c.as_str() != "AUD")
        .map(|c| (c.clone(), SeriesCursor::new(load_close_series(conn, &fx_pair_symbol(c)))))
        .collect();

    let mut shares: HashMap<String, f64> = HashMap::new();
    let mut balances: HashMap<i64, f64> = HashMap::new();
    let mut trade_index = 0usize;
    let mut cash_index = 0usize;
    let mut series = Vec::new();

    // Start a day early to produce the anchor described above.
    let mut date = start_date.pred_opt().unwrap_or(start_date);
    while date <= end_date {
        let day = date.format("%Y-%m-%d").to_string();
        let mut flow = 0.0;

        // Apply everything dated on or before today that has not been applied
        // yet, so transactions before `from` are folded into the opening state.
        while trade_index < trades.len() && trades[trade_index].date <= day {
            let trade = &trades[trade_index];
            let signed = match trade.tx_type.as_str() {
                "purchase" => trade.quantity,
                "sale" => -trade.quantity,
                _ => 0.0,
            };
            *shares.entry(trade.symbol.clone()).or_insert(0.0) += signed;

            // Externally funded trades count as money in or out on their day.
            if !trade.has_cash_leg && trade.date == day {
                match trade.tx_type.as_str() {
                    "purchase" => flow += trade.quantity * trade.price_aud + trade.brokerage_aud,
                    "sale" => flow -= trade.quantity * trade.price_aud - trade.brokerage_aud,
                    _ => {}
                }
            }
            trade_index += 1;
        }

        while cash_index < cash_txs.len() && cash_txs[cash_index].0 <= day {
            let (tx_date, account_id, amount, kind) = &cash_txs[cash_index];
            *balances.entry(*account_id).or_insert(0.0) += amount;
            if tx_date == &day && matches!(kind.as_str(), "deposit" | "withdrawal" | "opening_balance") {
                let currency = accounts.get(account_id).cloned().unwrap_or_else(|| "AUD".to_string());
                let rate = if currency == "AUD" {
                    Some(1.0)
                } else {
                    rates.get_mut(&currency).and_then(|c| c.value_on(&day))
                };
                flow += amount * rate.unwrap_or(0.0);
            }
            cash_index += 1;
        }

        let mut stocks = 0.0;
        for (symbol, held) in &shares {
            if *held <= 0.0 {
                continue;
            }
            // Past the last real bar there is no close for the day, so the
            // live quote stands in, then a manual valuation, then the last
            // stored close. That is the order the holdings endpoint uses, and
            // matching it is what keeps the chart's final point equal to the
            // Stock Value shown above it.
            let latest = || quotes.get(symbol).copied().or_else(|| manual.get(symbol).copied());
            let close = match prices.get_mut(symbol) {
                Some(cursor) => {
                    let stored = cursor.value_on(&day);
                    if day == today || cursor.is_past_end(&day) { latest().or(stored) } else { stored }
                }
                None => latest(),
            };
            let Some(close) = close else { continue };
            let currency = symbol_currency.get(symbol).cloned().unwrap_or_else(|| "AUD".to_string());
            // The FX bars stop with the price bars, and past that point the
            // holdings endpoint converts at the live rate. Following it here is
            // what makes the two agree: with a stale daily bar instead, every
            // foreign holding lands a fraction out and the chart's last point
            // drifts from the Stock Value beside it.
            let rate = if currency == "AUD" {
                Some(1.0)
            } else {
                let live = || quotes.get(&fx_pair_symbol(&currency)).copied();
                match rates.get_mut(&currency) {
                    Some(cursor) => {
                        let stored = cursor.value_on(&day);
                        if day == today || cursor.is_past_end(&day) { live().or(stored) } else { stored }
                    }
                    None => live(),
                }
            };
            let Some(rate) = rate else { continue };
            stocks += held * close * rate;
        }

        let mut cash = 0.0;
        for (account_id, balance) in &balances {
            let currency = accounts.get(account_id).cloned().unwrap_or_else(|| "AUD".to_string());
            let rate = if currency == "AUD" {
                Some(1.0)
            } else {
                rates.get_mut(&currency).and_then(|c| c.value_on(&day))
            };
            let Some(rate) = rate else { continue };
            cash += balance * rate;
        }

        series.push(portfolio::DailyValue { date: day, stocks, cash, flow });
        date = match date.succ_opt() {
            Some(d) => d,
            None => break,
        };
    }

    Ok(series)
}

#[utoipa::path(get, path = "/api/v1/portfolio/history", tag = "portfolio", responses((status = 200, description = "Daily portfolio value and time-weighted return")))]
#[get("/api/portfolio/history")]
async fn get_portfolio_history(db_path: web::Data<PathBuf>, query: web::Query<HistoryQuery>) -> impl Responder {
    let conn = match open_db(db_path.as_ref()) {
        Ok(c) => c,
        Err(err) => return err_internal(err.to_string()),
    };
    // The floor applies to every range, not just "All": a 5-year window reaches
    // just as far back into the unpriced years as an unbounded one.
    let floor: Option<String> = load_config(&db_path)
        .ok()
        .and_then(|items| {
            items
                .into_iter()
                .find(|item| item.key == PORTFOLIO_HISTORY_START)
                .map(|item| item.value.trim().to_string())
        })
        .filter(|value| !value.is_empty());
    // ISO dates order lexicographically, so the later of the two is the max.
    let from = match (query.from.as_deref(), floor.as_deref()) {
        (Some(requested), Some(floor)) => Some(requested.max(floor)),
        (None, floor) => floor,
        (requested, None) => requested,
    };

    let series = match build_portfolio_history(&conn, from, query.to.as_deref()) {
        Ok(s) => s,
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "portfolio_history", "api", None, &err);
            return err_bad_request(err);
        }
    };

    // Element 0 is the anchor: the value carried into the window, which is not
    // part of it. Returns chain across the whole vector so the first real day
    // still counts; contributions and the reported range cover the window only.
    let twr = portfolio::time_weighted_return(&series);
    let opening_value = series.first().map(|p| p.total()).unwrap_or(0.0);
    let window = if series.is_empty() { &series[..] } else { &series[1..] };
    let contributions = portfolio::net_contributions(window);
    let end_value = window.last().map(|p| p.total()).unwrap_or(opening_value);

    HttpResponse::Ok().json(serde_json::json!({
        "series": window.iter().map(|p| HistoryPoint {
            date: p.date.clone(),
            stocks: p.stocks,
            cash: p.cash,
            total: p.total(),
            flow: p.flow,
        }).collect::<Vec<_>>(),
        "summary": {
            "start_date": window.first().map(|p| p.date.clone()),
            "end_date": window.last().map(|p| p.date.clone()),
            "opening_value": opening_value,
            "end_value": end_value,
            "net_contributions": contributions,
            // What the portfolio earned: the change in value that contributions
            // do not account for. opening + contributions + gain = end.
            "gain": end_value - opening_value - contributions,
            "twr_pct": twr.map(|r| r * 100.0),
        },
        // Published so the client can hide range buttons the floor makes
        // identical to each other, rather than offering three ways to ask for
        // the same window.
        "start_floor": floor,
    }))
}

// ---------------------------------------------------------------------------
// Cash ledger
// ---------------------------------------------------------------------------

fn is_valid_cash_tx_kind(kind: &str) -> bool {
    CASH_TX_KINDS.contains(&kind)
}

/// Kinds that only ever originate from a transaction, so the manual endpoints
/// refuse to create them directly.
///
/// Note this is about *entry*, not ownership: `dividend` is absent because a
/// hand-entered dividend is legitimate (an account can be credited without the
/// app having fetched the event). Ownership is decided by `holding_tx_id`
/// instead — see `is_transaction_owned`.
fn is_trade_owned_kind(kind: &str) -> bool {
    kind == "trade_buy" || kind == "trade_sell"
}

/// Whether a cash row was written by `sync_trade_cash_leg` on behalf of a
/// transaction. Such a row is regenerated whenever that transaction is saved,
/// so editing it by hand is silently undone — the manual endpoints refuse
/// instead. Keyed on the link rather than the kind, because a dividend leg and
/// a hand-entered dividend share a kind but not an owner.
fn is_transaction_owned(holding_tx_id: Option<i64>) -> bool {
    holding_tx_id.is_some()
}

fn cash_account_currency(conn: &Connection, account_id: i64) -> Option<String> {
    conn.query_row(
        "SELECT currency FROM cash_accounts WHERE id = ?1",
        params![account_id],
        |row| row.get::<_, String>(0),
    )
    .optional()
    .ok()
    .flatten()
}

/// Balance of an account in its own currency, optionally as at a date.
/// Derived from the ledger every time — never stored — so a back-dated entry
/// is reflected immediately.
/// Ledger balance for one account, in the account's own currency.
///
/// Fallible on purpose. The SQL already `COALESCE`s an empty ledger to zero, so
/// the only thing a swallowed error could add is a *wrong* zero — reported as a
/// balance, summed into the portfolio total, and used as a guard against
/// changing an account's currency. Each caller decides what to do instead.
fn cash_balance(conn: &Connection, account_id: i64, as_of: Option<&str>) -> Result<f64, String> {
    let (sql, bind_date) = match as_of {
        Some(_) => (
            "SELECT COALESCE(SUM(amount), 0) FROM cash_transactions WHERE account_id = ?1 AND date <= ?2",
            true,
        ),
        None => ("SELECT COALESCE(SUM(amount), 0) FROM cash_transactions WHERE account_id = ?1", false),
    };
    let result = if bind_date {
        conn.query_row(sql, params![account_id, as_of.unwrap()], |row| row.get::<_, f64>(0))
    } else {
        conn.query_row(sql, params![account_id], |row| row.get::<_, f64>(0))
    };
    result.map_err(|err| format!("Reading balance for cash account {}: {}", account_id, err))
}

#[derive(Deserialize)]
struct CashAccountPayload {
    name: String,
    currency: String,
    interest_rate: Option<f64>,
    include_in_portfolio: Option<bool>,
    notes: Option<String>,
}

#[derive(Serialize)]
struct CashAccountRow {
    id: i64,
    name: String,
    currency: String,
    interest_rate: Option<f64>,
    include_in_portfolio: bool,
    notes: Option<String>,
    created_at: String,
    /// Balance in the account's own currency.
    balance: f64,
    /// The same balance in AUD at today's stored rate, or null when no rate is
    /// available yet for that currency.
    balance_aud: Option<f64>,
    transaction_count: i64,
}

fn load_cash_accounts(conn: &Connection) -> Result<Vec<CashAccountRow>, String> {
    let today = Utc::now().format("%Y-%m-%d").to_string();
    let mut stmt = conn
        .prepare(
            "SELECT id, name, currency, interest_rate, include_in_portfolio, notes, created_at
               FROM cash_accounts ORDER BY name",
        )
        .map_err(|e| e.to_string())?;
    let rows: Vec<(i64, String, String, Option<f64>, i64, Option<String>, String)> = stmt
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?))
        })
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();

    // Collected through `Result` so a balance that cannot be read fails the whole
    // listing rather than reporting one account as empty among the rest.
    rows.into_iter()
        .map(|(id, name, currency, interest_rate, include, notes, created_at)| {
            let balance = cash_balance(conn, id, None)?;
            let balance_aud = fx_rate_on(conn, &currency, &today).map(|rate| balance * rate);
            let transaction_count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM cash_transactions WHERE account_id = ?1",
                    params![id],
                    |row| row.get(0),
                )
                .map_err(|err| format!("Counting transactions for cash account {}: {}", id, err))?;
            Ok(CashAccountRow {
                id,
                name,
                currency,
                interest_rate,
                include_in_portfolio: include != 0,
                notes,
                created_at,
                balance,
                balance_aud,
                transaction_count,
            })
        })
        .collect()
}

#[utoipa::path(get, path = "/api/v1/cash/accounts", tag = "cash", responses((status = 200, description = "List cash accounts with balances")))]
#[get("/api/cash/accounts")]
async fn get_cash_accounts(db_path: web::Data<PathBuf>) -> impl Responder {
    let conn = match open_db(db_path.as_ref()) {
        Ok(c) => c,
        Err(err) => return err_internal(err.to_string()),
    };
    match load_cash_accounts(&conn) {
        Ok(rows) => HttpResponse::Ok().json(rows),
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "cash_accounts_fetch", "api", None, &err);
            err_internal(err)
        }
    }
}

#[utoipa::path(post, path = "/api/v1/cash/accounts", tag = "cash", responses((status = 200, description = "Create a cash account")))]
#[post("/api/cash/accounts")]
async fn add_cash_account(db_path: web::Data<PathBuf>, payload: web::Json<CashAccountPayload>) -> impl Responder {
    let payload = payload.into_inner();
    let name = payload.name.trim().to_string();
    let currency = payload.currency.trim().to_uppercase();
    if name.is_empty() {
        return err_bad_request("Account name is required");
    }
    if currency.len() != 3 {
        return err_bad_request("Currency must be a 3-letter code, e.g. AUD");
    }

    let conn = match open_db(db_path.as_ref()) {
        Ok(c) => c,
        Err(err) => return err_internal(err.to_string()),
    };
    let result = conn.execute(
        "INSERT INTO cash_accounts (name, currency, interest_rate, include_in_portfolio, notes, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            name,
            currency,
            payload.interest_rate,
            if payload.include_in_portfolio.unwrap_or(true) { 1 } else { 0 },
            payload.notes,
            Utc::now().to_rfc3339(),
        ],
    );
    match result {
        Ok(_) => {
            let id = conn.last_insert_rowid();
            let _ = insert_event_log(&db_path, "info", "cash_account_create", "api", None, &format!("Created cash account {} ({})", name, currency));
            HttpResponse::Ok().json(serde_json::json!({ "id": id }))
        }
        Err(err) => {
            let message = format!("Failed to create cash account: {}", err);
            let _ = insert_event_log(&db_path, "error", "cash_account_create", "api", None, &message);
            err_bad_request(message)
        }
    }
}

#[utoipa::path(put, path = "/api/v1/cash/accounts/{id}", tag = "cash", params(("id" = i64, Path, description = "id")), responses((status = 200, description = "Update a cash account")))]
#[put("/api/cash/accounts/{id}")]
async fn update_cash_account(
    db_path: web::Data<PathBuf>,
    path: web::Path<i64>,
    payload: web::Json<CashAccountPayload>,
) -> impl Responder {
    let id = path.into_inner();
    let payload = payload.into_inner();
    let conn = match open_db(db_path.as_ref()) {
        Ok(c) => c,
        Err(err) => return err_internal(err.to_string()),
    };

    // Changing currency would silently reinterpret every existing amount.
    let existing = cash_account_currency(&conn, id);
    let Some(existing_currency) = existing else {
        return err_not_found(format!("Cash account {} not found", id));
    };
    let currency = payload.currency.trim().to_uppercase();
    if currency != existing_currency {
        // Refuse rather than assume. A balance that cannot be read used to come
        // back as 0.0, which read as "empty account" and let the change through
        // — reinterpreting every amount already in the ledger.
        let balance = match cash_balance(&conn, id, None) {
            Ok(b) => b,
            Err(err) => {
                let _ = insert_event_log(&db_path, "error", "cash_account_update", "api", None, &err);
                return err_internal(err);
            }
        };
        if balance != 0.0 {
            return err_bad_request(format!(
                "Cannot change currency from {} to {} while the account has a non-zero balance",
                existing_currency, currency
            ));
        }
    }

    let result = conn.execute(
        "UPDATE cash_accounts SET name = ?2, currency = ?3, interest_rate = ?4,
                include_in_portfolio = ?5, notes = ?6
          WHERE id = ?1",
        params![
            id,
            payload.name.trim(),
            currency,
            payload.interest_rate,
            if payload.include_in_portfolio.unwrap_or(true) { 1 } else { 0 },
            payload.notes,
        ],
    );
    match result {
        Ok(0) => err_not_found(format!("Cash account {} not found", id)),
        Ok(_) => HttpResponse::Ok().json(serde_json::json!({ "id": id })),
        Err(err) => {
            let message = format!("Failed to update cash account {}: {}", id, err);
            let _ = insert_event_log(&db_path, "error", "cash_account_update", "api", None, &message);
            err_bad_request(message)
        }
    }
}

#[utoipa::path(delete, path = "/api/v1/cash/accounts/{id}", tag = "cash", params(("id" = i64, Path, description = "id")), responses((status = 204, description = "Delete a cash account")))]
#[delete("/api/cash/accounts/{id}")]
async fn delete_cash_account(db_path: web::Data<PathBuf>, path: web::Path<i64>) -> impl Responder {
    let id = path.into_inner();
    let conn = match open_db(db_path.as_ref()) {
        Ok(c) => c,
        Err(err) => return err_internal(err.to_string()),
    };

    // Deleting an account with history would orphan its ledger, and SQLite's
    // foreign keys are not enforced here, so the check has to be explicit.
    // Defaulting to zero here made the guard fail *open*: a failed count read as
    // "no transactions" and the account was deleted anyway, orphaning the ledger
    // this check exists to protect.
    let count: i64 = match conn
        .query_row("SELECT COUNT(*) FROM cash_transactions WHERE account_id = ?1", params![id], |r| r.get(0))
    {
        Ok(n) => n,
        Err(err) => {
            let message = format!("Could not check cash account {} for transactions: {}", id, err);
            let _ = insert_event_log(&db_path, "error", "cash_account_delete", "api", None, &message);
            return err_internal(message);
        }
    };
    if count > 0 {
        return err_bad_request(format!(
            "Cash account {} still has {} transaction(s); delete or reassign them first",
            id, count
        ));
    }

    match conn.execute("DELETE FROM cash_accounts WHERE id = ?1", params![id]) {
        Ok(0) => err_not_found(format!("Cash account {} not found", id)),
        Ok(_) => {
            let _ = insert_event_log(&db_path, "info", "cash_account_delete", "api", None, &format!("Deleted cash account {}", id));
            HttpResponse::NoContent().finish()
        }
        Err(err) => err_internal(err.to_string()),
    }
}

#[derive(Deserialize)]
struct CashTxPayload {
    account_id: i64,
    date: String,
    amount: f64,
    kind: String,
    notes: Option<String>,
}

#[derive(Serialize)]
struct CashTxRow {
    id: i64,
    account_id: i64,
    account_name: String,
    currency: String,
    date: String,
    amount: f64,
    kind: String,
    holding_tx_id: Option<i64>,
    transfer_group_id: Option<String>,
    notes: Option<String>,
    created_at: String,
}

#[derive(Deserialize)]
struct CashTxQuery {
    account_id: Option<i64>,
    from: Option<String>,
    to: Option<String>,
}

/// Quote a CSV field only when it needs it, doubling any embedded quotes.
///
/// Notes are free text and routinely contain commas — an unquoted
/// "purchase SPCX — USD 810.00 at 1.4189" would split into two columns and
/// shift every field after it.
fn csv_field(value: &str) -> String {
    if value.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

/// One cash account's ledger as a CSV download, with a running balance.
///
/// Served as a file rather than JSON the browser assembles: the balance is a
/// running total that only means anything in a fixed order, so the order and
/// the arithmetic are settled here rather than depending on however the client
/// happens to sort.
#[derive(Serialize)]
struct ChartDrawing {
    id: i64,
    symbol: String,
    kind: String,
    /// In the symbol's own currency — the chart applies the FX rate at render.
    price: f64,
    label: Option<String>,
    colour: Option<String>,
    /// Trendlines only: the two anchors are (start_date, price) and
    /// (end_date, end_price). Null on a horizontal level.
    start_date: Option<String>,
    end_date: Option<String>,
    end_price: Option<f64>,
    created_at: String,
}

#[derive(Deserialize)]
struct NewChartDrawing {
    /// Omitted for a horizontal level, which is the default.
    kind: Option<String>,
    price: f64,
    label: Option<String>,
    colour: Option<String>,
    start_date: Option<String>,
    end_date: Option<String>,
    end_price: Option<f64>,
}

fn load_chart_drawings(conn: &Connection, symbol: &str) -> Result<Vec<ChartDrawing>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT id, symbol, kind, price, label, colour, start_date, end_date, end_price, created_at
               FROM chart_drawings WHERE symbol = ?1 ORDER BY price DESC, id",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![symbol], |r| {
            Ok(ChartDrawing {
                id: r.get(0)?,
                symbol: r.get(1)?,
                kind: r.get(2)?,
                price: r.get(3)?,
                label: r.get(4)?,
                colour: r.get(5)?,
                start_date: r.get(6)?,
                end_date: r.get(7)?,
                end_price: r.get(8)?,
                created_at: r.get(9)?,
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
}

#[utoipa::path(get, path = "/api/v1/chart-drawings/{symbol}", tag = "charts",
    params(("symbol" = String, Path, description = "symbol")),
    responses((status = 200, description = "Price levels drawn on this symbol's chart")))]
#[get("/api/chart-drawings/{symbol}")]
async fn get_chart_drawings(db_path: web::Data<PathBuf>, path: web::Path<String>) -> impl Responder {
    let symbol = path.into_inner().to_uppercase();
    let conn = match open_db(db_path.as_ref()) {
        Ok(c) => c,
        Err(err) => return err_internal(err.to_string()),
    };
    match load_chart_drawings(&conn, &symbol) {
        Ok(rows) => HttpResponse::Ok().json(serde_json::json!({ "drawings": rows })),
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "chart_drawings", "api", Some(&symbol), &err);
            err_internal(err)
        }
    }
}

#[utoipa::path(post, path = "/api/v1/chart-drawings/{symbol}", tag = "charts",
    params(("symbol" = String, Path, description = "symbol")),
    responses((status = 200, description = "Draw a horizontal price level")))]
#[post("/api/chart-drawings/{symbol}")]
async fn add_chart_drawing(
    db_path: web::Data<PathBuf>,
    path: web::Path<String>,
    payload: web::Json<NewChartDrawing>,
) -> impl Responder {
    let symbol = path.into_inner().to_uppercase();
    let payload = payload.into_inner();
    // A level at or below zero is not a price. Rejecting it here keeps a
    // mis-drag from writing a line that can never be seen on the chart.
    if !payload.price.is_finite() || payload.price <= 0.0 {
        return err_bad_request("Price level must be a positive number".to_string());
    }
    let kind = payload.kind.as_deref().unwrap_or("horizontal");
    if kind == "trend" {
        // A trendline is defined by two anchors. Storing one with a missing or
        // non-positive second anchor would leave a row that can be read but
        // never drawn — a line with no slope and no end.
        let ok = payload.start_date.is_some()
            && payload.end_date.is_some()
            && payload.end_price.is_some_and(|p| p.is_finite() && p > 0.0);
        if !ok {
            return err_bad_request(
                "A trendline needs both anchors: start_date, end_date and a positive end_price".to_string(),
            );
        }
        if payload.start_date == payload.end_date {
            return err_bad_request("A trendline's two anchors must be on different dates".to_string());
        }
    } else if kind != "horizontal" {
        return err_bad_request(format!("Unknown drawing kind '{}'", kind));
    }
    let conn = match open_db(db_path.as_ref()) {
        Ok(c) => c,
        Err(err) => return err_internal(err.to_string()),
    };
    let result = conn.execute(
        "INSERT INTO chart_drawings (symbol, kind, price, label, colour, start_date, end_date, end_price, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![symbol, kind, payload.price, payload.label, payload.colour,
                payload.start_date, payload.end_date, payload.end_price, Utc::now().to_rfc3339()],
    );
    match result {
        Ok(_) => {
            let id = conn.last_insert_rowid();
            let _ = insert_event_log(&db_path, "info", "chart_drawings", "api", Some(&symbol),
                &format!("Drew level {} at {}", id, payload.price));
            match load_chart_drawings(&conn, &symbol) {
                Ok(rows) => HttpResponse::Ok().json(serde_json::json!({ "drawings": rows })),
                Err(err) => err_internal(err),
            }
        }
        Err(err) => err_internal(err.to_string()),
    }
}

#[derive(Deserialize)]
struct MovedChartDrawing {
    price: f64,
}

#[utoipa::path(patch, path = "/api/v1/chart-drawings/id/{id}", tag = "charts",
    params(("id" = i64, Path, description = "id")),
    responses((status = 200, description = "Move a horizontal level to a new price")))]
#[patch("/api/chart-drawings/id/{id}")]
async fn move_chart_drawing(
    db_path: web::Data<PathBuf>,
    path: web::Path<i64>,
    payload: web::Json<MovedChartDrawing>,
) -> impl Responder {
    let id = path.into_inner();
    let price = payload.into_inner().price;
    // Same rule as drawing one: a level at or below zero can never be seen.
    if !price.is_finite() || price <= 0.0 {
        return err_bad_request("Price level must be a positive number".to_string());
    }
    let conn = match open_db(db_path.as_ref()) {
        Ok(c) => c,
        Err(err) => return err_internal(err.to_string()),
    };
    let found: Option<(String, String, f64)> = match conn
        .query_row(
            "SELECT symbol, kind, price FROM chart_drawings WHERE id = ?1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
    {
        Ok(found) => found,
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "chart_drawings", "api", None, &format!("Failed to load drawing {}: {}", id, err));
            return err_internal(err.to_string());
        }
    };
    let Some((symbol, kind, old_price)) = found else {
        return err_not_found(format!("Drawing {} not found", id));
    };
    // A trendline's `price` is only its first anchor; moving that alone would
    // silently change the line's slope.
    if kind != "horizontal" {
        return err_bad_request("Only a horizontal level can be moved to a new price".to_string());
    }
    if let Err(err) = conn.execute("UPDATE chart_drawings SET price = ?1 WHERE id = ?2", params![price, id]) {
        let _ = insert_event_log(&db_path, "error", "chart_drawings", "api", Some(&symbol), &format!("Failed to move level {}: {}", id, err));
        return err_internal(err.to_string());
    }
    let _ = insert_event_log(&db_path, "info", "chart_drawings", "api", Some(&symbol),
        &format!("Moved level {} from {} to {}", id, old_price, price));
    match load_chart_drawings(&conn, &symbol) {
        Ok(rows) => HttpResponse::Ok().json(serde_json::json!({ "drawings": rows })),
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "chart_drawings", "api", Some(&symbol), &err);
            err_internal(err)
        }
    }
}

#[utoipa::path(delete, path = "/api/v1/chart-drawings/id/{id}", tag = "charts",
    params(("id" = i64, Path, description = "id")),
    responses((status = 204, description = "Remove a drawn level")))]
#[delete("/api/chart-drawings/id/{id}")]
async fn delete_chart_drawing(db_path: web::Data<PathBuf>, path: web::Path<i64>) -> impl Responder {
    let id = path.into_inner();
    let conn = match open_db(db_path.as_ref()) {
        Ok(c) => c,
        Err(err) => return err_internal(err.to_string()),
    };
    match conn.execute("DELETE FROM chart_drawings WHERE id = ?1", params![id]) {
        Ok(0) => err_not_found(format!("Drawing {} not found", id)),
        Ok(_) => {
            let _ = insert_event_log(&db_path, "info", "chart_drawings", "api", None, &format!("Removed level {}", id));
            HttpResponse::NoContent().finish()
        }
        Err(err) => err_internal(err.to_string()),
    }
}

#[utoipa::path(get, path = "/api/v1/cash/accounts/{id}/transactions.csv", tag = "cash",
    params(("id" = i64, Path, description = "id")),
    responses((status = 200, description = "Cash account ledger as CSV")))]
#[get("/api/cash/accounts/{id}/transactions.csv")]
async fn export_cash_account_csv(db_path: web::Data<PathBuf>, path: web::Path<i64>) -> impl Responder {
    let account_id = path.into_inner();
    let conn = match open_db(db_path.as_ref()) {
        Ok(c) => c,
        Err(err) => return err_internal(err.to_string()),
    };

    let account: Option<(String, String)> = conn
        .query_row(
            "SELECT name, currency FROM cash_accounts WHERE id = ?1",
            params![account_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .ok()
        .flatten();
    let Some((name, currency)) = account else {
        return err_not_found(format!("Cash account {} not found", account_id));
    };

    let rows = (|| -> Result<Vec<(String, String, String, f64)>, String> {
        let mut stmt = conn
            .prepare(
                "SELECT c.date, c.kind, c.amount, c.notes, c.transfer_group_id,
                        h.symbol, h.transaction_type
                   FROM cash_transactions c
                   LEFT JOIN holdings_transactions h ON h.id = c.holding_tx_id
                  WHERE c.account_id = ?1
                  ORDER BY c.date, c.id",
            )
            .map_err(|e| e.to_string())?;
        let mapped = stmt
            .query_map(params![account_id], |r| {
                let date: String = r.get(0)?;
                let kind: String = r.get(1)?;
                let amount: f64 = r.get(2)?;
                let notes: Option<String> = r.get(3)?;
                let group: Option<String> = r.get(4)?;
                let symbol: Option<String> = r.get(5)?;
                let tx_type: Option<String> = r.get(6)?;
                // A leg written before notes were generated has none; say what
                // it is rather than leaving the column blank.
                let description = match (notes, group, symbol, tx_type) {
                    (Some(n), _, _, _) if !n.trim().is_empty() => n,
                    (_, Some(_), _, _) => "Transfer between accounts".to_string(),
                    (_, _, Some(sym), Some(t)) => format!("{} {}", t, sym),
                    _ => String::new(),
                };
                Ok((date, kind, description, amount))
            })
            .map_err(|e| e.to_string())?;
        mapped.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
    })();

    let rows = match rows {
        Ok(r) => r,
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "cash_export", "api", None, &err);
            return err_internal(err);
        }
    };

    let mut csv = format!(
        "Date,Transaction,Description,Amount ({0}),Balance ({0})\n",
        currency
    );
    let mut balance = 0.0_f64;
    for (date, kind, description, amount) in &rows {
        balance += amount;
        csv.push_str(&format!(
            "{},{},{},{:.2},{:.2}\n",
            csv_field(date),
            csv_field(kind),
            csv_field(description),
            amount,
            balance
        ));
    }

    // A filename the user can tell apart from the other accounts' exports.
    let slug: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("-");

    HttpResponse::Ok()
        .content_type("text/csv; charset=utf-8")
        .insert_header((
            "Content-Disposition",
            format!("attachment; filename=\"cash-{}.csv\"", slug),
        ))
        .body(csv)
}

#[utoipa::path(get, path = "/api/v1/cash/transactions", tag = "cash", responses((status = 200, description = "List cash transactions")))]
#[get("/api/cash/transactions")]
async fn get_cash_transactions(db_path: web::Data<PathBuf>, query: web::Query<CashTxQuery>) -> impl Responder {
    let conn = match open_db(db_path.as_ref()) {
        Ok(c) => c,
        Err(err) => return err_internal(err.to_string()),
    };
    let mut stmt = match conn.prepare(
        "SELECT t.id, t.account_id, a.name, a.currency, t.date, t.amount, t.kind,
                t.holding_tx_id, t.transfer_group_id, t.notes, t.created_at
           FROM cash_transactions t
           JOIN cash_accounts a ON a.id = t.account_id
          WHERE (?1 IS NULL OR t.account_id = ?1)
            AND (?2 IS NULL OR t.date >= ?2)
            AND (?3 IS NULL OR t.date <= ?3)
          ORDER BY t.date DESC, t.id DESC",
    ) {
        Ok(s) => s,
        Err(err) => return err_internal(err.to_string()),
    };
    let rows = stmt.query_map(params![query.account_id, query.from, query.to], |row| {
        Ok(CashTxRow {
            id: row.get(0)?,
            account_id: row.get(1)?,
            account_name: row.get(2)?,
            currency: row.get(3)?,
            date: row.get(4)?,
            amount: row.get(5)?,
            kind: row.get(6)?,
            holding_tx_id: row.get(7)?,
            transfer_group_id: row.get(8)?,
            notes: row.get(9)?,
            created_at: row.get(10)?,
        })
    });
    match rows {
        Ok(rows) => HttpResponse::Ok().json(rows.filter_map(|r| r.ok()).collect::<Vec<_>>()),
        Err(err) => err_internal(err.to_string()),
    }
}

/// Shared validation for a manually entered cash transaction.
fn validate_cash_tx(conn: &Connection, payload: &CashTxPayload) -> Result<String, String> {
    if !is_valid_cash_tx_kind(&payload.kind) {
        return Err(format!("Unknown kind '{}'. Valid kinds: {}", payload.kind, CASH_TX_KINDS.join(", ")));
    }
    if is_trade_owned_kind(&payload.kind) {
        return Err(format!(
            "'{}' entries are created from the trade that settles them, not entered directly",
            payload.kind
        ));
    }
    if NaiveDate::parse_from_str(&payload.date, "%Y-%m-%d").is_err() {
        return Err("Invalid date format. Use YYYY-MM-DD.".to_string());
    }
    if !payload.amount.is_finite() || payload.amount == 0.0 {
        return Err("Amount must be a non-zero number".to_string());
    }
    cash_account_currency(conn, payload.account_id)
        .ok_or_else(|| format!("Cash account {} not found", payload.account_id))
}

#[utoipa::path(post, path = "/api/v1/cash/transactions", tag = "cash", responses((status = 200, description = "Record a cash transaction")))]
#[post("/api/cash/transactions")]
async fn add_cash_transaction(db_path: web::Data<PathBuf>, payload: web::Json<CashTxPayload>) -> impl Responder {
    let payload = payload.into_inner();
    let conn = match open_db(db_path.as_ref()) {
        Ok(c) => c,
        Err(err) => return err_internal(err.to_string()),
    };
    if let Err(err) = validate_cash_tx(&conn, &payload) {
        return err_bad_request(err);
    }

    let result = conn.execute(
        "INSERT INTO cash_transactions (account_id, date, amount, kind, notes, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![payload.account_id, payload.date, payload.amount, payload.kind, payload.notes, Utc::now().to_rfc3339()],
    );
    match result {
        Ok(_) => HttpResponse::Ok().json(serde_json::json!({ "id": conn.last_insert_rowid() })),
        Err(err) => {
            let message = format!("Failed to record cash transaction: {}", err);
            let _ = insert_event_log(&db_path, "error", "cash_tx_create", "api", None, &message);
            err_internal(message)
        }
    }
}

#[utoipa::path(put, path = "/api/v1/cash/transactions/{id}", tag = "cash", params(("id" = i64, Path, description = "id")), responses((status = 200, description = "Update a cash transaction")))]
#[put("/api/cash/transactions/{id}")]
async fn update_cash_transaction(
    db_path: web::Data<PathBuf>,
    path: web::Path<i64>,
    payload: web::Json<CashTxPayload>,
) -> impl Responder {
    let id = path.into_inner();
    let payload = payload.into_inner();
    let conn = match open_db(db_path.as_ref()) {
        Ok(c) => c,
        Err(err) => return err_internal(err.to_string()),
    };

    let existing: Option<(String, Option<String>, Option<i64>)> = conn
        .query_row(
            "SELECT kind, transfer_group_id, holding_tx_id FROM cash_transactions WHERE id = ?1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .ok()
        .flatten();
    match existing {
        None => return err_not_found(format!("Cash transaction {} not found", id)),
        Some((_, _, holding_tx_id)) if is_transaction_owned(holding_tx_id) => {
            return err_bad_request("This entry belongs to a transaction — edit the transaction instead".to_string());
        }
        // Both legs of a conversion have to move together. Editing one in
        // isolation would create money in one currency and destroy it in the
        // other; deleting removes the pair, so re-entering is the safe path.
        Some((_, Some(_), _)) => {
            return err_bad_request(
                "This is one leg of a transfer between accounts — delete it and record the transfer again".to_string(),
            );
        }
        Some((_, None, _)) => {}
    }
    if let Err(err) = validate_cash_tx(&conn, &payload) {
        return err_bad_request(err);
    }

    match conn.execute(
        "UPDATE cash_transactions SET account_id = ?2, date = ?3, amount = ?4, kind = ?5, notes = ?6 WHERE id = ?1",
        params![id, payload.account_id, payload.date, payload.amount, payload.kind, payload.notes],
    ) {
        Ok(0) => err_not_found(format!("Cash transaction {} not found", id)),
        Ok(_) => HttpResponse::Ok().json(serde_json::json!({ "id": id })),
        Err(err) => err_internal(err.to_string()),
    }
}

#[utoipa::path(delete, path = "/api/v1/cash/transactions/{id}", tag = "cash", params(("id" = i64, Path, description = "id")), responses((status = 204, description = "Delete a cash transaction")))]
#[delete("/api/cash/transactions/{id}")]
async fn delete_cash_transaction(db_path: web::Data<PathBuf>, path: web::Path<i64>) -> impl Responder {
    let id = path.into_inner();
    let conn = match open_db(db_path.as_ref()) {
        Ok(c) => c,
        Err(err) => return err_internal(err.to_string()),
    };

    let row: Option<(Option<String>, Option<i64>)> = conn
        .query_row(
            "SELECT transfer_group_id, holding_tx_id FROM cash_transactions WHERE id = ?1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .ok()
        .flatten();
    let Some((transfer_group_id, holding_tx_id)) = row else {
        return err_not_found(format!("Cash transaction {} not found", id));
    };
    if is_transaction_owned(holding_tx_id) {
        return err_bad_request("This entry belongs to a transaction — delete the transaction instead".to_string());
    }

    // Removing one leg of a conversion would invent money in one currency and
    // destroy it in another, so both legs go together.
    let deleted = match &transfer_group_id {
        Some(group) => conn.execute("DELETE FROM cash_transactions WHERE transfer_group_id = ?1", params![group]),
        None => conn.execute("DELETE FROM cash_transactions WHERE id = ?1", params![id]),
    };
    match deleted {
        Ok(0) => err_not_found(format!("Cash transaction {} not found", id)),
        Ok(n) => {
            let _ = insert_event_log(&db_path, "info", "cash_tx_delete", "api", None, &format!("Deleted {} cash transaction row(s) for id {}", n, id));
            HttpResponse::NoContent().finish()
        }
        Err(err) => err_internal(err.to_string()),
    }
}

/// Rewrite the cash leg belonging to one trade.
///
/// Re-read from the stored row rather than the request payload, so the leg
/// reflects what was actually persisted — including a price and rate the server
/// resolved rather than the client supplying. Delete-then-insert keeps this
/// idempotent: calling it after any create or edit converges on exactly one leg,
/// or none when the trade names no account.
///
/// The settlement is expressed in the trade's own currency. `price` is AUD and
/// `original_price` is native, but `brokerage` is AUD by the existing engine's
/// convention (`realised_pl = qty * price - brokerage - cost`), so a foreign
/// trade converts it back at that trade's own recorded rate.
fn sync_trade_cash_leg(conn: &Connection, holding_tx_id: i64) -> Result<(), String> {
    conn.execute("DELETE FROM cash_transactions WHERE holding_tx_id = ?1", params![holding_tx_id])
        .map_err(|e| e.to_string())?;

    let row: Option<(Option<i64>, String, String, Option<f64>, Option<f64>, Option<f64>, Option<f64>, Option<f64>, String, Option<f64>)> = conn
        .query_row(
            "SELECT cash_account_id, transaction_type, date, quantity, price, original_price, fx_rate, brokerage, symbol, amount
               FROM holdings_transactions WHERE id = ?1",
            params![holding_tx_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?, r.get(8)?, r.get(9)?)),
        )
        .optional()
        .map_err(|e| e.to_string())?;

    let Some((Some(account_id), tx_type, date, quantity, price, original_price, fx_rate, brokerage, symbol, total_amount)) = row else {
        return Ok(()); // no trade, or it settles against no account
    };

    let account_currency = cash_account_currency(conn, account_id)
        .ok_or_else(|| format!("Cash account {} not found", account_id))?;
    let trade_currency: String = conn
        .query_row(
            "SELECT COALESCE(currency, 'AUD') FROM holdings_transactions WHERE id = ?1",
            params![holding_tx_id],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    let is_foreign = trade_currency != "AUD";

    // Two settlement styles, because brokers differ:
    //
    //   * Matching currencies (AUD/AUD, USD/USD) book in that currency. This is
    //     an account that carries a real balance in it, like IBKR's USD.
    //   * A foreign trade against an AUD account converts at the trade's own
    //     rate. CommSec International works this way — the account holds only
    //     AUD and the broker converts per trade, so no USD balance ever exists
    //     to settle against. The rate is not lost: `fx_rate` is stored on the
    //     trade, `price` is the AUD it produced, and the note records it.
    //
    // Anything else (a USD account against a EUR trade, say) is a conversion we
    // have no rate for, and still has to be split into a transfer plus a trade.
    let settle_converted = if account_currency == trade_currency {
        false
    } else if account_currency == "AUD" && is_foreign {
        true
    } else {
        return Err(format!(
            "{} trade settles in {} but the chosen cash account is {}. \
             Convert the funds first, then settle from an account in the trade's currency.",
            symbol, trade_currency, account_currency
        ));
    };

    // `price` is always AUD; `original_price` is the trade's own currency.
    let book_native = is_foreign && !settle_converted;

    // Brokerage is stored in AUD, so it only needs converting when the leg is
    // booked in a foreign currency.
    let brokerage_settled = match (brokerage, book_native, fx_rate) {
        (Some(b), true, Some(rate)) if rate != 0.0 => b / rate,
        (Some(b), false, _) => b,
        (Some(_), true, _) => {
            return Err(format!(
                "{} trade has brokerage but no exchange rate, so the fee cannot be expressed in {}",
                symbol, trade_currency
            ))
        }
        (None, _, _) => 0.0,
    };

    // A trade is defined by quantity and unit price; a dividend is defined by
    // its total, and that is all the form requires. Deriving both the same way
    // would leave a hand-entered dividend — total only, no share count — with
    // no cash leg at all.
    let gross = if tx_type == "dividend" {
        let native_total = quantity.zip(original_price).map(|(q, unit)| q * unit);
        let aud_total = total_amount.or_else(|| quantity.zip(price).map(|(q, unit)| q * unit));
        let settled = if book_native {
            native_total.or_else(|| {
                aud_total
                    .zip(fx_rate)
                    .and_then(|(total, rate)| (rate != 0.0).then_some(total / rate))
            })
        } else {
            aud_total
        };
        let Some(settled) = settled else { return Ok(()) };
        settled
    } else {
        let unit_price = if book_native { original_price } else { price };
        let (Some(unit_price), Some(quantity)) = (unit_price, quantity) else { return Ok(()) };
        quantity * unit_price
    };
    let (amount, kind) = match tx_type.as_str() {
        "purchase" => (-(gross + brokerage_settled), "trade_buy"),
        "sale" => (gross - brokerage_settled, "trade_sell"),
        // Income, not a flow: `dividend` is classified as return in
        // CASH_TX_KINDS, so it lifts the growth figure instead of being
        // written off as money the portfolio was handed from outside.
        "dividend" => (gross - brokerage_settled, "dividend"),
        _ => return Ok(()),
    };

    // A dividend is filed under its ex-date, but the cash lands on the payment
    // date — often weeks later. Settling on the ex-date would credit the
    // account before the money existed, so use the payment date when the
    // fetched event knows it.
    let settle_date = if tx_type == "dividend" {
        conn.query_row(
            "SELECT payment_date FROM dividend_events WHERE symbol = ?1 AND ex_date = ?2",
            params![symbol, date],
            |r| r.get::<_, Option<String>>(0),
        )
        .optional()
        .map_err(|e| e.to_string())?
        .flatten()
        .unwrap_or_else(|| date.clone())
    } else {
        date.clone()
    };

    // A converted leg states the foreign amount and rate on its face, so the
    // ledger reads as an explanation rather than an unexplained AUD figure.
    let foreign_total = quantity.zip(original_price).map(|(q, unit)| q * unit);
    let note = match (settle_converted, foreign_total, fx_rate) {
        (true, Some(native), Some(rate)) => format!(
            "{} {} — {} {:.2} at {:.4}",
            tx_type, symbol, trade_currency, native, rate
        ),
        _ => format!("{} {}", tx_type, symbol),
    };

    conn.execute(
        "INSERT INTO cash_transactions (account_id, date, amount, kind, holding_tx_id, notes, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            account_id,
            settle_date,
            amount,
            kind,
            holding_tx_id,
            note,
            Utc::now().to_rfc3339(),
        ],
    )
    .map_err(|e| e.to_string())?;

    // Withholding is deducted at source, so the bank only ever sees the net.
    // It is booked as a second leg rather than by shrinking the dividend: the
    // tax withheld is a figure you need at tax time, and netting it away
    // destroys it. `fee` is classified as return in CASH_TX_KINDS, so gross
    // less withholding lands on the growth figure as the net actually received.
    if tx_type == "dividend" {
        // An amount recorded against the payment wins over the symbol's
        // standing rate. TFN withholding applies to the unfranked portion, so
        // it varies with each distribution's franking and stops once a TFN is
        // quoted — a per-symbol percentage cannot express that.
        let explicit: Option<f64> = conn
            .query_row(
                "SELECT withholding_amount FROM holdings_transactions WHERE id = ?1",
                params![holding_tx_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?
            .flatten();

        let withholding_pct: Option<f64> = conn
            .query_row(
                "SELECT dividend_withholding_pct FROM symbol_info WHERE symbol = ?1",
                params![symbol],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?
            .flatten();

        let (withheld, label) = match (explicit.filter(|a| *a > 0.0), withholding_pct.filter(|p| *p > 0.0)) {
            (Some(a), _) => (
                (a * 100.0).round() / 100.0,
                format!("tax withheld on {} dividend", symbol),
            ),
            (None, Some(pct)) => {
                // Round the net, then take the fee as the remainder, so the two
                // legs always sum to the cash the account actually received.
                let net = ((amount * (1.0 - pct / 100.0)) * 100.0).round() / 100.0;
                (
                    ((amount - net) * 100.0).round() / 100.0,
                    format!("withholding tax {}% on {} dividend", pct, symbol),
                )
            }
            (None, None) => (0.0, String::new()),
        };

        if withheld.abs() >= 0.005 {
            conn.execute(
                "INSERT INTO cash_transactions (account_id, date, amount, kind, holding_tx_id, notes, created_at)
                 VALUES (?1, ?2, ?3, 'fee', ?4, ?5, ?6)",
                params![
                    account_id,
                    settle_date,
                    -withheld,
                    holding_tx_id,
                    label,
                    Utc::now().to_rfc3339(),
                ],
            )
            .map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

#[derive(Deserialize)]
struct CashTransferPayload {
    from_account_id: i64,
    to_account_id: i64,
    date: String,
    /// Amount leaving the source account, in the source account's currency.
    from_amount: f64,
    /// Amount arriving in the destination account, in its own currency.
    to_amount: f64,
    notes: Option<String>,
}

#[utoipa::path(post, path = "/api/v1/cash/transfer", tag = "cash", responses((status = 200, description = "Move cash between accounts, including across currencies")))]
#[post("/api/cash/transfer")]
async fn add_cash_transfer(db_path: web::Data<PathBuf>, payload: web::Json<CashTransferPayload>) -> impl Responder {
    let payload = payload.into_inner();
    let mut conn = match open_db(db_path.as_ref()) {
        Ok(c) => c,
        Err(err) => return err_internal(err.to_string()),
    };

    if payload.from_account_id == payload.to_account_id {
        return err_bad_request("Source and destination accounts must differ");
    }
    if NaiveDate::parse_from_str(&payload.date, "%Y-%m-%d").is_err() {
        return err_bad_request("Invalid date format. Use YYYY-MM-DD.");
    }
    if payload.from_amount <= 0.0 || payload.to_amount <= 0.0 {
        return err_bad_request("Both amounts must be positive; direction is set by the accounts");
    }
    for id in [payload.from_account_id, payload.to_account_id] {
        if cash_account_currency(&conn, id).is_none() {
            return err_not_found(format!("Cash account {} not found", id));
        }
    }

    // Both legs share a group id and are written in one transaction: a half
    // written transfer would silently change the portfolio's total value.
    let group = format!("xfer-{}", Utc::now().timestamp_micros());
    let created_at = Utc::now().to_rfc3339();
    let tx = match conn.transaction() {
        Ok(t) => t,
        Err(err) => return err_internal(err.to_string()),
    };
    let legs = [
        (payload.from_account_id, -payload.from_amount, "fx_out"),
        (payload.to_account_id, payload.to_amount, "fx_in"),
    ];
    for (account_id, amount, kind) in legs {
        if let Err(err) = tx.execute(
            "INSERT INTO cash_transactions (account_id, date, amount, kind, transfer_group_id, notes, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![account_id, payload.date, amount, kind, group, payload.notes, created_at],
        ) {
            let message = format!("Failed to record transfer: {}", err);
            let _ = insert_event_log(&db_path, "error", "cash_transfer", "api", None, &message);
            return err_internal(message);
        }
    }
    match tx.commit() {
        Ok(_) => HttpResponse::Ok().json(serde_json::json!({
            "transfer_group_id": group,
            "implied_rate": payload.to_amount / payload.from_amount,
        })),
        Err(err) => err_internal(err.to_string()),
    }
}

fn cache_current_price(conn: &Connection, price: &CurrentPrice) -> Result<(), String> {
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

fn load_cached_prices(db_path: &PathBuf, symbols: &[String]) -> Result<Vec<CurrentPrice>, String> {
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

/// First stored close on or after `date`, in the symbol's own currency.
///
/// A baseline date names a calendar day, which is often not a trading day —
/// 1 January never is — so it resolves forward to the first bar that exists.
fn close_on_or_after(conn: &Connection, symbol: &str, date: &str) -> Option<f64> {
    conn.query_row(
        "SELECT close FROM prices
          WHERE symbol = ?1 AND date >= ?2 AND close IS NOT NULL
          ORDER BY date LIMIT 1",
        params![symbol, date],
        |row| row.get::<_, f64>(0),
    )
    .ok()
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

/// Record a warn/error against the event_log using an already-open connection.
fn log_event_on_conn(conn: &Connection, level: &str, event_type: &str, symbol: Option<&str>, details: &str) {
    let now = Utc::now().to_rfc3339();
    let _ = conn.execute(
        "INSERT INTO event_log (timestamp, level, source, event_type, symbol, details) VALUES (?1, ?2, 'api', ?3, ?4, ?5)",
        params![now, level, event_type, symbol, details],
    );
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
fn is_usable_quote(price: Option<f64>) -> bool {
    matches!(price, Some(p) if p.is_finite() && p > 0.0)
}

fn persist_price_to_history(conn: &Connection, symbol: &str, price: &CurrentPrice, fetched_at: &str) {
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
    if let (Some(close), Some(date)) = (price.price, &price.price_date)
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
            params![symbol, date, price.day_open, price.day_high, price.day_low, close, price.volume, fetched_at],
        ) {
            log_event_on_conn(conn, "warn", "price_persist", Some(symbol), &format!("Failed to persist current price: {}", err));
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

    // Drop the symbols marked dead. Every fetch path — watchlist, holdings and
    // sold — funnels through here, so this is the one place that has to know.
    // Their prices still resolve through the manual/last-close chain; what stops
    // is asking Yahoo a question it has already answered 404 to.
    let dead = open_db(db_path).map(|c| dead_symbols(&c)).unwrap_or_default();
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
/// Valid `cash_transactions.kind` values.
///
/// Grouped by how each behaves in a time-weighted return, which is the whole
/// point of recording them separately:
///
/// - `deposit` / `withdrawal` — **external flows**. Money entering or leaving
///   the portfolio. Excluded from return, so a monthly contribution cannot
///   masquerade as growth.
/// - `opening_balance` — establishes the starting value when an account is
///   first tracked. Not a flow; it is the V₀ the first return is measured from.
/// - `dividend` / `interest` — **return**. Income the portfolio generated.
///   Treating these as flows would erase exactly the earnings being measured.
/// - `fee` — **return**, negative. A cost the portfolio bore.
/// - `trade_buy` / `trade_sell` — **neutral**. Value moves between cash and
///   shares; the total is unchanged at the moment of the trade.
/// - `fx_out` / `fx_in` — **neutral**. The paired legs of a currency
///   conversion between accounts you already own.
/// - `adjustment` — reconciliation against a real statement. Neutral by
///   default; a large one is a sign something upstream was mis-recorded.
const CASH_TX_KINDS: &[&str] = &[
    "deposit",
    "withdrawal",
    "opening_balance",
    "dividend",
    "interest",
    "fee",
    "trade_buy",
    "trade_sell",
    "fx_out",
    "fx_in",
    "adjustment",
];


/// Currencies the portfolio actually holds value in, excluding AUD (the base).
///
/// Derived from the data rather than configured, so adding a GBP holding starts
/// its rate history automatically. Reads both `symbol_info` (the authority for a
/// symbol's currency) and `holdings_transactions` (which carries the currency a
/// trade actually settled in, and covers symbols missing from symbol_info).
fn fx_currencies_in_use(conn: &Connection) -> Vec<String> {
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


fn last_expected_trading_day(now: chrono::DateTime<Utc>, symbol: &str) -> String {
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
    if let (Some(p), Some(s)) = (price, sma50)
        && p > s {
            let stats = ind::crossover_stats(&points, &sma50_arr, volume);
            days_50 = Some(stats.days);
            vol_50 = stats.volume_pct;
        }
    let mut days_150 = None;
    let mut vol_150 = None;
    if let (Some(p), Some(s)) = (price, sma150)
        && p > s {
            let stats = ind::crossover_stats(&points, &sma150_arr, volume);
            days_150 = Some(stats.days);
            vol_150 = stats.volume_pct;
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

    let mut info: HashMap<String, SymbolInfo> = HashMap::new();
    let info_result = (|| -> Result<Vec<(String, SymbolInfo)>, String> {
        let conn = open_db(db_path.as_ref()).map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare("SELECT symbol, instrument_type, long_name, currency FROM symbol_info")
            .map_err(|e| e.to_string())?;
        let info_rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, (row.get::<_, Option<String>>(1)?, row.get::<_, Option<String>>(2)?, row.get::<_, Option<String>>(3)?)))
            })
            .map_err(|e| e.to_string())?;
        info_rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
    })();
    match info_result {
        Ok(info_rows) => {
            for (symbol, symbol_info) in info_rows {
                info.insert(symbol, symbol_info);
            }
        }
        Err(err) => {
            // Rows degrade to symbol-only badges — record why
            let _ = insert_event_log(&db_path, "warn", "watchlist_fetch", "api", None, &format!("symbol_info unavailable for enriched watchlist: {}", err));
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











/// Result of one FX sync pass, for logging and the manual endpoint.
#[derive(Serialize, Default)]
struct FxSyncReport {
    currencies: Vec<String>,
    fetched: usize,
    skipped: usize,
    bars_written: usize,
    errors: Vec<String>,
}

/// Bring stored FX history up to date for every currency the portfolio uses.
///
/// Valuing a foreign holding on a past date needs that date's rate, so the
/// rates live in `prices` as daily bars rather than being fetched live per
/// lookup. Already-current pairs cost one indexed query and no network, which
/// makes this safe to call on every startup.
async fn sync_fx_history(db_path: &PathBuf, days: i64) -> FxSyncReport {
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

    let client = match Client::builder().user_agent("stocks-api/1.0").build() {
        Ok(c) => c,
        Err(err) => {
            let message = format!("FX sync could not build HTTP client: {}", err);
            let _ = insert_event_log(db_path, "error", "fx_sync", "api", None, &message);
            report.errors.push(message);
            return report;
        }
    };

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

        match fetch_price_history_from_yahoo(&client, &pair, days).await {
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
async fn post_fx_sync(db_path: web::Data<PathBuf>) -> impl Responder {
    // `fetch_price_history_from_yahoo` asks for a 2-year range for any days > 365,
    // so this is the widest window available through the shared fetch path —
    // ample, given the earliest holding transaction is 2025-01-02. Extending
    // beyond that would need explicit period1/period2 bounds, as backfill_ohlc
    // uses, because Yahoo downsamples `range=max` to monthly bars.
    HttpResponse::Ok().json(sync_fx_history(db_path.as_ref(), 600).await)
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
/// Symbol-level keys for the profit/loss baseline. `_price` is derived from
/// `_date` when the date is saved, and both live in `holdings_symbol_fields`,
/// which is already audited — so this needs no schema change and no new
/// triggers.
/// Earliest date the portfolio value chart will report, empty for no floor.
///
/// A holding cannot be valued before its first stored price bar, so the years
/// before that are drawn with the stock line flat at zero — cash alone, dressed
/// up as portfolio history. The floor cuts that stretch off rather than
/// inviting it to be read as a real drawdown.
const PORTFOLIO_HISTORY_START: &str = "portfolio_history_start";

const PL_BASIS_DATE: &str = "pl_basis_date";
const PL_BASIS_PRICE: &str = "pl_basis_price";

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

/// Exponential moving average over *weekly* closes.
///
/// A 40-week EMA is not a 200-day EMA. Both span roughly the same calendar, but
/// the weekly one is computed from one close per week, so it steps once a week
/// and is far less sensitive to a single day's move. Chartists mean the weekly
/// figure, so it is what gets computed.
///
/// Weeks start on Monday and take that week's last available close, matching
/// `toWeeklyBars` in the web client — the table and the chart's Week interval
/// must not disagree about what a week is.
fn stored_weekly_ema(conn: &Connection, symbol: &str, period: usize) -> Option<f64> {
    if period == 0 {
        return None;
    }
    // Daily rows to read before collapsing. An EMA needs history well beyond
    // its period to settle, and a week costs ~5 rows, so 40 weeks of settled
    // average needs years of dailies behind it.
    const LOOKBACK_DAYS: i64 = 3000;
    let mut stmt = conn
        .prepare(
            "SELECT date, close FROM prices
              WHERE symbol = ?1 AND close IS NOT NULL
              ORDER BY date DESC LIMIT ?2",
        )
        .ok()?;
    let mut rows: Vec<(String, f64)> = stmt
        .query_map(params![symbol, LOOKBACK_DAYS], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?))
        })
        .ok()?
        .flatten()
        .collect();
    rows.reverse(); // oldest first, so each week's last close wins

    let mut weekly: Vec<f64> = Vec::new();
    let mut current_week: Option<NaiveDate> = None;
    for (date, close) in rows {
        let Ok(parsed) = NaiveDate::parse_from_str(&date, "%Y-%m-%d") else { continue };
        let monday = parsed.week(chrono::Weekday::Mon).first_day();
        if current_week == Some(monday) {
            // Same week — this later close replaces the earlier one.
            *weekly.last_mut()? = close;
        } else {
            current_week = Some(monday);
            weekly.push(close);
        }
    }

    if weekly.len() < period {
        return None;
    }
    let k = 2.0 / (period as f64 + 1.0);
    let mut ema = weekly[..period].iter().sum::<f64>() / period as f64;
    for close in &weekly[period..] {
        ema = close * k + ema * (1.0 - k);
    }
    Some(ema)
}

/// The cost basis and dividends a holding's performance is measured against,
/// and the baseline it came from when one is set.
///
/// A per-symbol baseline *replaces* the purchase rather than sitting beside it:
/// for a holding bought decades ago the original price is a record, not a
/// useful denominator. Both the holdings endpoint and the overview aggregate go
/// through here, so the Holdings screen and the Dashboard total can never
/// disagree about the same position.
///
/// The stored basis price is native, like the closes it came from, so it is
/// converted at today's rate — an approximation for a foreign holding, but
/// consistent with the current value it is measured against.
fn effective_basis(
    ctx: &PortfolioContext,
    symbol: &str,
    txs: &[portfolio::PortfolioTx],
    remaining_shares: f64,
    purchase_cost: f64,
    purchase_dividends: f64,
) -> (f64, f64, Option<(String, f64)>) {
    let fields = ctx.fields.get(symbol);
    let date = fields.and_then(|f| f.get(PL_BASIS_DATE)).map(|d| d.trim()).filter(|d| !d.is_empty());
    let native = fields
        .and_then(|f| f.get(PL_BASIS_PRICE))
        .and_then(|p| p.trim().parse::<f64>().ok())
        .filter(|p| *p > 0.0);
    if let (Some(date), Some(native)) = (date, native) {
        let basis = portfolio::rebase_at(txs, remaining_shares, date, ctx.to_aud(symbol, native));
        // A zero basis would divide the percentage by nothing; fall back to the
        // purchase rather than reporting an infinite return.
        if basis.cost > 0.0 {
            return (basis.cost, basis.dividends, Some((date.to_string(), native)));
        }
    }
    (purchase_cost, purchase_dividends, None)
}

/// Latest value of a dashboard `indicator:` field, in the symbol's own
/// currency — the same basis as the stored closes it is derived from, so it
/// needs no FX conversion to compare against a native price.
///
/// Keys are matched exactly rather than parsed for a period: the Configuration
/// screen offers a fixed set, and an explicit list keeps a typo from silently
/// producing an empty dashboard table.
fn indicator_value(conn: &Connection, symbol: &str, key: &str) -> Option<f64> {
    match key {
        "sma50" => stored_sma(conn, symbol, 50),
        "sma150" => stored_sma(conn, symbol, 150),
        "ema40w" => stored_weekly_ema(conn, symbol, 40),
        _ => None,
    }
}

/// Stored daily bars for one symbol, newest-last, straight from the database.
///
/// Deliberately not `fetch_price_history`: that tops up from Yahoo when the
/// stored window looks short, and a crossover list spanning a few hundred
/// watchlist symbols would turn one dashboard load into a burst of fetches.
/// Whatever has been ingested is what the indicator is built from.
fn load_local_history(conn: &Connection, symbol: &str, days: i64) -> Vec<PriceHistoryPoint> {
    let Ok(mut stmt) = conn.prepare(
        "SELECT date, open, high, low, close, volume FROM prices
          WHERE symbol = ?1 AND close IS NOT NULL
          ORDER BY date DESC LIMIT ?2",
    ) else {
        return Vec::new();
    };
    let Ok(rows) = stmt.query_map(params![symbol, days], |row| {
        Ok(PriceHistoryPoint {
            date: row.get(0)?,
            open: row.get(1)?,
            high: row.get(2)?,
            low: row.get(3)?,
            close: row.get(4)?,
            volume: row.get(5)?,
        })
    }) else {
        return Vec::new();
    };
    let mut history: Vec<PriceHistoryPoint> = rows.flatten().collect();
    history.reverse();
    history
}

/// The reference series a crossover is measured against, aligned one-to-one
/// with `history` so a crossing can be counted in trading days.
///
/// `constant` covers a stored field — a breakthrough price does not move, so
/// its series is that number repeated, and "days above" reads as days since the
/// price cleared it.
fn crossover_reference(
    history: &[PriceHistoryPoint],
    field_key: &str,
    constant: Option<f64>,
) -> Option<Vec<Option<f64>>> {
    use stocks::indicators as ind;
    let points = indicator_points(history);
    match field_key {
        "sma50" => Some(ind::calculate_sma(&points, 50)),
        "sma150" => Some(ind::calculate_sma(&points, 150)),
        "ema40w" => weekly_ema_series(history, 40),
        _ => constant.map(|c| vec![Some(c); history.len()]),
    }
}

/// A weekly EMA spread back over the daily bars it covers, so a weekly
/// indicator can still be crossed in days.
///
/// Each day carries the EMA of the last *completed* week before it. Using the
/// running week's own value would let a bar be compared against an average that
/// includes its own close — the count would then shift retroactively as the
/// week finished.
fn weekly_ema_series(history: &[PriceHistoryPoint], period: usize) -> Option<Vec<Option<f64>>> {
    if period == 0 {
        return None;
    }
    // Collapse to one close per week, remembering which week each daily bar is
    // in so the finished EMA can be mapped back onto the dailies.
    let mut weeks: Vec<NaiveDate> = Vec::new();
    let mut weekly_closes: Vec<f64> = Vec::new();
    let mut bar_week: Vec<Option<usize>> = Vec::with_capacity(history.len());
    for bar in history {
        let parsed = NaiveDate::parse_from_str(&bar.date, "%Y-%m-%d").ok();
        match (parsed, bar.close) {
            (Some(date), Some(close)) => {
                let monday = date.week(chrono::Weekday::Mon).first_day();
                if weeks.last() == Some(&monday) {
                    *weekly_closes.last_mut()? = close;
                } else {
                    weeks.push(monday);
                    weekly_closes.push(close);
                }
                bar_week.push(Some(weeks.len() - 1));
            }
            _ => bar_week.push(None),
        }
    }
    if weekly_closes.len() < period {
        return None;
    }

    // EMA over the weekly closes: index i holds the value once week i closes.
    let k = 2.0 / (period as f64 + 1.0);
    let mut weekly_ema: Vec<Option<f64>> = vec![None; weekly_closes.len()];
    let mut ema = weekly_closes[..period].iter().sum::<f64>() / period as f64;
    weekly_ema[period - 1] = Some(ema);
    for i in period..weekly_closes.len() {
        ema = weekly_closes[i] * k + ema * (1.0 - k);
        weekly_ema[i] = Some(ema);
    }

    Some(
        bar_week
            .iter()
            .map(|w| w.and_then(|i| if i == 0 { None } else { weekly_ema[i - 1] }))
            .collect(),
    )
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
    info: HashMap<String, SymbolInfo>,
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
    let mut info: HashMap<String, SymbolInfo> = HashMap::new();
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
    // Last traded bar per symbol, for anything the live feed no longer quotes.
    let last_closes = latest_closes(db_path, &symbols);
    let mut prices: HashMap<String, EffectivePrice> = HashMap::new();
    for symbol in &symbols {
        let c = cached_map.get(symbol);
        let mut native = c.and_then(|p| p.price).filter(|p| *p > 0.0);
        let mut source = if native.is_some() { "cache" } else { "none" };
        let mut price_date = c.and_then(|p| p.price_date.clone());
        if native.is_none()
            && let Some(manual) = config.get(&format!("manual_price_{}", symbol))
                && let Ok(v) = manual.parse::<f64>() {
                    native = Some(v);
                    source = "manual";
                }
        // A delisted symbol has no quote and often no manual override either.
        // Its last traded price is the only honest figure available — better
        // than nothing, which would drop the holding out of the portfolio
        // total, and far better than the feed's zero. `source` says where it
        // came from so the UI can mark it stale.
        if native.is_none()
            && let Some((close, date)) = last_closes.get(symbol) {
                native = Some(*close);
                source = "last_close";
                price_date = Some(date.clone());
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
            price_date,
            change: c.and_then(|p| p.change),
            change_percent: c.and_then(|p| p.change_percent),
            volume: c.and_then(|p| p.volume),
        });
    }

    Ok(PortfolioContext { groups, prices, info, fields, intl, etf, all_aud: all_aud_map, fx_rates })
}

/// Daily bars for one symbol from `from` onward, shaped for the hindsight
/// engine. The floor keeps the query to the span actually being measured
/// instead of every bar the symbol has ever had.
fn load_bars_since(conn: &Connection, symbol: &str, from: &str) -> Vec<hindsight::Bar> {
    let mut stmt = match conn.prepare(
        "SELECT date, high, low, close FROM prices
          WHERE symbol = ?1 AND date >= ?2 AND close IS NOT NULL
          ORDER BY date",
    ) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    stmt.query_map(params![symbol, from], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<f64>>(1)?,
            row.get::<_, Option<f64>>(2)?,
            row.get::<_, f64>(3)?,
        ))
    })
    .map(|rows| {
        rows.flatten()
            .filter_map(|(date, high, low, close)| {
                Some(hindsight::Bar {
                    date: NaiveDate::parse_from_str(&date, "%Y-%m-%d").ok()?,
                    high,
                    low,
                    close,
                })
            })
            .collect()
    })
    .unwrap_or_default()
}

/// One sale, and what the shares did afterwards.
#[derive(Serialize)]
struct HindsightRow {
    symbol: String,
    /// Native throughout — the row never converts, so the client must not
    /// assume AUD when rendering it.
    currency: String,
    sale_date: String,
    quantity: f64,
    sale_price: f64,
    /// FIFO cost of the shares this sale consumed, brokerage included. `None`
    /// where the matching purchase was never recorded.
    purchase_price: Option<f64>,
    /// What the trade itself made, per share, as a percentage. The six points
    /// are measured against the sale price; this is the only figure looking
    /// backward to the purchase.
    realised_pct: Option<f64>,
    delisted_on: Option<String>,
    points: hindsight::Points,
}

/// Every sale, priced at six later moments.
///
/// Built on `build_portfolio_context` rather than its own price lookup so the
/// "current" column is literally the same number the Holdings screen shows. A
/// second resolver would be free to drift, and a screen whose whole purpose is
/// comparison cannot afford to disagree with the one it is compared against.
#[utoipa::path(get, path = "/api/v1/hindsight", tag = "portfolio", responses((status = 200, description = "Sold positions priced at six later moments")))]
#[get("/api/hindsight")]
async fn get_hindsight(db_path: web::Data<PathBuf>) -> impl Responder {
    let ctx = match build_portfolio_context(&db_path).await {
        Ok(c) => c,
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "hindsight", "api", None, &err);
            return err_internal(err);
        }
    };
    let conn = match open_db(db_path.as_ref()) {
        Ok(c) => c,
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "hindsight", "api", None, &err.to_string());
            return err_internal(err.to_string());
        }
    };
    let dead = dead_symbols(&conn);
    let today = Utc::now().date_naive();

    let mut rows: Vec<HindsightRow> = Vec::new();
    for (symbol, txs) in &ctx.groups {
        let sales = portfolio::sale_costs(txs);
        if sales.is_empty() {
            continue;
        }
        // One read per symbol, floored at its earliest sale less the lookback
        // window, so a weekend target still finds the Friday before it.
        let floor = sales
            .iter()
            .map(|s| s.date.as_str())
            .min()
            .and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
            .and_then(|d| d.checked_sub_days(chrono::Days::new(7)))
            .map(|d| d.to_string())
            .unwrap_or_default();
        let bars = load_bars_since(&conn, symbol, &floor);

        let current = ctx.prices.get(symbol).and_then(|p| p.native);
        let delisted_on = dead
            .get(symbol)
            .and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok());

        for sale in sales {
            let Ok(sale_date) = NaiveDate::parse_from_str(&sale.date, "%Y-%m-%d") else {
                let _ = insert_event_log(&db_path, "warn", "hindsight", "api", Some(symbol),
                    &format!("Sale dated '{}' is not a usable date and was skipped", sale.date));
                continue;
            };
            let s = hindsight::Sale {
                date: sale_date,
                price: sale.native_price,
                quantity: sale.quantity,
            };
            rows.push(HindsightRow {
                symbol: symbol.clone(),
                currency: ctx.currency_of(symbol),
                sale_date: sale.date.clone(),
                quantity: sale.quantity,
                sale_price: sale.native_price,
                purchase_price: sale.native_cost_per_share,
                realised_pct: sale
                    .native_cost_per_share
                    .filter(|c| *c > 0.0)
                    .map(|cost| (sale.native_price - cost) / cost * 100.0),
                delisted_on: delisted_on.map(|d| d.to_string()),
                points: hindsight::price_points(&s, &bars, current, today, delisted_on),
            });
        }
    }

    // Most recent sale first: the trades still worth second-guessing are the
    // ones whose windows are still filling in.
    rows.sort_by(|a, b| b.sale_date.cmp(&a.sale_date).then_with(|| a.symbol.cmp(&b.symbol)));
    HttpResponse::Ok().json(rows)
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
        let sym_fields = ctx.fields.get(symbol);
        let fields: HashMap<&String, &String> = sym_fields
            .map(|f| f.iter().filter(|(k, _)| k.as_str() != "_notes").collect())
            .unwrap_or_default();
        let sma150 = stored_sma(&conn, symbol, 150).map(|v| ctx.to_aud(symbol, v));
        let itype = ctx.info.get(symbol).and_then(|i| i.0.clone());
        // Effective stop loss (manual field, or the trailing-sell trigger) in
        // the symbol's native currency, matching native_current_price.
        let (stop_loss, is_trailing_sell) =
            match effective_stop_loss(&conn, symbol, sym_fields, ep.and_then(|p| p.native), |p| p) {
                Some((sl, trailing)) => (Some(sl), trailing),
                None => (None, false),
            };

        let (invested, dividends, rebased) = effective_basis(
            &ctx,
            symbol,
            txs,
            summary.remaining_shares,
            summary.remaining_cost,
            dividends,
        );
        let (avg_cost, native_avg_cost) = match rebased.as_ref() {
            Some((_, native)) => (Some(ctx.to_aud(symbol, *native)), Some(*native)),
            None => (
                (summary.remaining_shares > 0.0)
                    .then(|| summary.remaining_cost / summary.remaining_shares),
                (summary.remaining_shares > 0.0)
                    .then(|| summary.native_remaining_cost / summary.remaining_shares),
            ),
        };
        let pl = current_value - invested + dividends;
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
            "avg_cost": avg_cost,
            "native_avg_cost": native_avg_cost,
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
            // Present when the figures above are measured from a baseline
            // rather than from the purchase, so the client can say which.
            "basis_date": rebased.as_ref().map(|(date, _)| date.clone()),
            "basis_price": rebased.as_ref().map(|(_, native)| *native),
            "sma150": sma150,
            "stop_loss": stop_loss,
            "is_trailing_sell": is_trailing_sell,
        }));
    }

    HttpResponse::Ok().json(serde_json::json!({ "holdings": holdings, "fx_rates": ctx.fx_rates }))
}

/// Effective stop-loss for a holding: the manual `stop_loss` symbol field if
/// set, otherwise the trailing-sell trigger — the highest close since
/// `trailing_sell_date` (plus the current price) minus `trailing_sell_pct`.
/// Returns `(price, is_trailing)`. `convert` maps stored native closes into
/// the caller's currency and must match the currency of `current_price`.
fn effective_stop_loss(
    conn: &Connection,
    symbol: &str,
    sym_fields: Option<&std::collections::HashMap<String, String>>,
    current_price: Option<f64>,
    convert: impl Fn(f64) -> f64,
) -> Option<(f64, bool)> {
    let manual = sym_fields
        .and_then(|f| f.get("stop_loss"))
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|v| *v != 0.0);
    if let Some(sl) = manual {
        return Some((sl, false));
    }
    let pct = sym_fields
        .and_then(|f| f.get("trailing_sell_pct"))
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|v| *v > 0.0)?;
    let mut reference = current_price;
    if let Some(since) = sym_fields.and_then(|f| f.get("trailing_sell_date")).filter(|d| !d.is_empty()) {
        // A trailing stop ratchets on the highest price actually *reached*, so
        // the peak is taken from the intraday high — that is what brokers trail
        // on. Using the close instead understates the peak whenever a bar spikes
        // and gives back the gain (TXG: high 60.69 vs close 58.48, a $1.80
        // difference in the resulting stop).
        //
        // COALESCE falls back to the close for bars predating OHLC ingest that
        // the backfill could not reach, so a missing high degrades to the old
        // behaviour for that bar rather than dropping it from the peak.
        let mut peaks: Vec<f64> = conn
            .prepare(
                "SELECT COALESCE(high, close) FROM prices
                  WHERE symbol = ?1 AND COALESCE(high, close) IS NOT NULL AND date >= ?2",
            )
            .ok()
            .and_then(|mut stmt| {
                stmt.query_map(params![symbol, since], |row| row.get::<_, f64>(0))
                    .ok()
                    .map(|r| r.flatten().map(&convert).collect())
            })
            .unwrap_or_default();
        if let Some(c) = current_price {
            peaks.push(c);
        }
        if !peaks.is_empty() {
            reference = peaks.into_iter().reduce(f64::max);
        }
    }
    let r = reference.filter(|r| *r != 0.0)?;
    Some((r * (1.0 - pct / 100.0), true))
}

#[derive(Deserialize)]
struct PortfolioOverviewQuery {
    /// Per-list sort override, as comma-separated `list_key:asc|desc` pairs
    /// (e.g. `stop_losses:desc`). Overrides the `sort` in each list's config.
    ///
    /// This has to be a server-side concern: each list is ranked and then
    /// truncated to its `limit` before being sent, so reversing the order in
    /// the browser would only reverse the rows that survived the cut. Sorting
    /// here changes *which* rows are selected.
    list_sort: Option<String>,
}

/// Parse a `list_sort` parameter into list_key → direction. Unknown or
/// malformed pairs are ignored so a bad query degrades to configured order
/// rather than failing the whole dashboard.
fn parse_list_sort(raw: Option<&str>) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let Some(raw) = raw else { return out };
    for pair in raw.split(',') {
        if let Some((key, dir)) = pair.split_once(':') {
            let dir = dir.trim().to_ascii_lowercase();
            if dir == "asc" || dir == "desc" {
                out.insert(key.trim().to_string(), dir);
            }
        }
    }
    out
}

#[utoipa::path(get, path = "/api/v1/portfolio/overview", tag = "portfolio", responses((status = 200, description = "Get portfolio overview")))]
#[get("/api/portfolio/overview")]
async fn get_portfolio_overview(
    db_path: web::Data<PathBuf>,
    query: web::Query<PortfolioOverviewQuery>,
) -> impl Responder {
    let list_sort_overrides = parse_list_sort(query.list_sort.as_deref());
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
            // Same basis the Holdings screen reports, so the totals here are the
            // sum of what that screen shows rather than a second opinion.
            let (cost, dividends, _) = effective_basis(
                &ctx,
                symbol,
                txs,
                pos.remaining_shares,
                pos.remaining_cost,
                pos.dividends,
            );
            let sym_pl = current_value - cost + dividends;
            holdings_agg.add(current_value, dividends, sym_pl, cost);
            if ctx.etf.get(symbol).copied().unwrap_or(false) {
                etf_agg.add(current_value, dividends, sym_pl, cost);
            } else {
                equity_agg.add(current_value, dividends, sym_pl, cost);
            }
            let sector = ctx.sector_of(symbol).unwrap_or_else(|| "Unallocated".to_string());
            sector_aggs.entry(sector).or_default().add(current_value, dividends, sym_pl, cost);
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
            if let (Some(p), Some(s)) = (price, sma150)
                && s != 0.0 {
                    let pct = (p - s) / s * 100.0;
                    scored.push((pct, serde_json::json!({ "symbol": symbol, "price": p, "sma150": s, "pct_diff": pct })));
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
        /// What the field is compared against: "price" (default) or "volume".
        /// Volume is a raw share count with no currency, so it is never
        /// converted the way a price is.
        #[serde(default)]
        compare: Option<String>,
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
    let list_defs: Vec<DashboardListDef> = config_json(&db_path, &config, "dashboard_custom_lists");
    let holdings_field_defs: Vec<FieldDef> = config_json(&db_path, &config, "holdings_custom_fields");
    let watchlist_field_defs: Vec<FieldDef> = config_json(&db_path, &config, "watchlist_custom_fields");

    // Needed to derive trailing stop-loss triggers for the stop_loss lists;
    // on failure those lists degrade to manual stop losses only.
    let list_conn = match open_db(db_path.as_ref()) {
        Ok(c) => Some(c),
        Err(err) => {
            let _ = insert_event_log(&db_path, "warn", "portfolio_fetch", "api", None, &format!("Custom lists: DB unavailable for trailing stop losses: {}", err));
            None
        }
    };
    // Crossover operators need the whole aligned series, not just the latest
    // indicator value, so their history is read up front — but only when a list
    // actually asks for one, leaving every other dashboard load untouched.
    const CROSS_OPS: [&str; 3] = ["days_above", "days_below", "volume_cross_pct"];
    let cross_defs: Vec<&DashboardListDef> = list_defs
        .iter()
        .filter(|d| CROSS_OPS.contains(&d.operator.as_str()))
        .collect();
    // A 40-week EMA needs years of bars to settle; the same lookback as
    // `stored_weekly_ema` keeps a "days above" list agreeing with the plain
    // "above" list on the same indicator.
    let cross_window: i64 = if cross_defs.iter().any(|d| d.field_key.ends_with(":ema40w")) { 3000 } else { 600 };
    let mut cross_symbols: std::collections::HashSet<String> = std::collections::HashSet::new();
    for def in &cross_defs {
        if def.source == "holdings" || def.source == "both" {
            for (symbol, txs) in &ctx.groups {
                if portfolio::calc_symbol_position(txs).remaining_shares > 0.0 {
                    cross_symbols.insert(symbol.clone());
                }
            }
        }
        if def.source == "watchlist" || def.source == "both" {
            cross_symbols.extend(watch_unique.iter().cloned());
        }
    }
    let cross_histories: HashMap<String, Vec<PriceHistoryPoint>> = match list_conn.as_ref() {
        Some(conn) => cross_symbols
            .iter()
            .map(|symbol| (symbol.clone(), load_local_history(conn, symbol, cross_window)))
            .collect(),
        None => HashMap::new(),
    };
    if !cross_defs.is_empty() && cross_histories.is_empty() {
        let _ = insert_event_log(&db_path, "warn", "portfolio_fetch", "api", None, "Crossover lists configured but no price history could be read; those lists will be empty");
    }

    let cross_stats = |symbol: &str,
                       field_key: &str,
                       constant: Option<f64>,
                       direction: stocks::indicators::CrossDirection,
                       today_volume: Option<i64>|
     -> Option<stocks::indicators::CrossoverStats> {
        let history = cross_histories.get(symbol)?;
        let reference = crossover_reference(history, field_key, constant)?;
        let points = indicator_points(history);
        Some(stocks::indicators::crossover_stats_dir(&points, &reference, today_volume, direction))
    };

    // Indicators are read per symbol from `prices`, and a weekly EMA sweeps
    // thousands of rows. Two lists on the same indicator would pay that twice,
    // so results are memoised for the life of the request.
    let indicator_cache: std::cell::RefCell<HashMap<(String, String), Option<f64>>> =
        std::cell::RefCell::new(HashMap::new());
    let cached_indicator = |symbol: &str, key: &str| -> Option<f64> {
        let cache_key = (symbol.to_string(), key.to_string());
        if let Some(hit) = indicator_cache.borrow().get(&cache_key) {
            return *hit;
        }
        let value = list_conn.as_ref().and_then(|conn| indicator_value(conn, symbol, key));
        indicator_cache.borrow_mut().insert(cache_key, value);
        value
    };

    let custom_lists: Vec<serde_json::Value> = list_defs
        .iter()
        .map(|def| {
            let (field_source, field_key) = def.field_key.split_once(':').unwrap_or(("", ""));
            // An indicator is derived from price history, so it exists for any
            // symbol regardless of which table the symbol lives in. `source`
            // alone then decides which branches run.
            let is_indicator = field_source == "indicator";
            struct Entry {
                symbol: String,
                price: f64,
                /// The side of the comparison the diff is measured from —
                /// equal to `price` unless the list compares volume.
                compare_value: f64,
                /// Which table this symbol came from. Per entry rather than per
                /// list because an indicator list sourced from "both" mixes
                /// holdings and watchlist rows, and each navigates elsewhere.
                origin: &'static str,
                /// Trading days since the crossing, on a crossover list only.
                days: Option<i64>,
                /// Volume on the crossing day vs the preceding 20-day average.
                volume_cross_pct: Option<f64>,
                field_value: f64,
                diff: f64,
                pct_diff: f64,
                currency: Option<String>,
                is_trailing: bool,
            }
            let compare_volume = def.compare.as_deref() == Some("volume");
            // "Volume on cross" is the breakout case measured a different way,
            // so it shares a direction with days_above and differs only in what
            // the list is ranked by.
            let cross_dir = match def.operator.as_str() {
                "days_above" | "volume_cross_pct" => Some(stocks::indicators::CrossDirection::Above),
                "days_below" => Some(stocks::indicators::CrossDirection::Below),
                _ => None,
            };
            // The column the list is ranked by, so the client knows which
            // header to make sortable and which figures to show.
            let metric = match def.operator.as_str() {
                "days_above" | "days_below" => "days",
                "volume_cross_pct" => "volume_cross_pct",
                _ => "pct_diff",
            };
            let matches_op = |diff: f64| match def.operator.as_str() {
                "above" | "pct_below" | "days_above" | "volume_cross_pct" => diff > 0.0,
                "below" | "pct_above" | "days_below" => diff < 0.0,
                _ => false,
            };
            let mut entries: Vec<Entry> = Vec::new();

            if (def.source == "holdings" || def.source == "both") && (is_indicator || field_source == "holdings") {
                for (symbol, txs) in &ctx.groups {
                    let pos = portfolio::calc_symbol_position(txs);
                    if pos.remaining_shares <= 0.0 {
                        continue;
                    }
                    let Some(price) = ctx.prices.get(symbol).and_then(|p| p.native) else { continue };
                    // A volume list still needs the price above: the stop-loss
                    // fallback below is priced, and the price stays on the
                    // entry as context.
                    let compare_value = if compare_volume {
                        match ctx.prices.get(symbol).and_then(|p| p.volume).filter(|v| *v > 0) {
                            Some(v) => v as f64,
                            None => continue,
                        }
                    } else {
                        price
                    };
                    // The built-in stop_loss field falls back to the
                    // trailing-sell trigger, so holdings protected by a
                    // trailing stop appear in stop-loss lists too. Prices
                    // here are native, so closes need no conversion.
                    let (fv, is_trailing) = if is_indicator {
                        match cached_indicator(symbol, field_key).filter(|v| *v > 0.0) {
                            Some(v) => (v, false),
                            None => continue,
                        }
                    } else if field_key == "stop_loss" && let Some(conn) = list_conn.as_ref() {
                        match effective_stop_loss(conn, symbol, ctx.fields.get(symbol), Some(price), |p| p) {
                            Some((sl, trailing)) if sl > 0.0 => (sl, trailing),
                            _ => continue,
                        }
                    } else {
                        let Some(fv) = ctx.fields.get(symbol).and_then(|f| f.get(field_key)).and_then(|v| v.parse::<f64>().ok()).filter(|v| *v > 0.0) else { continue };
                        (fv, false)
                    };
                    let diff = compare_value - fv;
                    if matches_op(diff) {
                        // A row that cannot be dated has nothing to rank on, so
                        // it is dropped rather than shown with a blank column.
                        let (days, volume_cross_pct) = match cross_dir {
                            Some(dir) => {
                                let constant = if is_indicator { None } else { Some(fv) };
                                let volume = ctx.prices.get(symbol).and_then(|p| p.volume);
                                match cross_stats(symbol, field_key, constant, dir, volume) {
                                    Some(stats) => (Some(stats.days), stats.volume_pct),
                                    None => continue,
                                }
                            }
                            None => (None, None),
                        };
                        entries.push(Entry {
                            symbol: symbol.clone(),
                            price,
                            compare_value,
                            field_value: fv,
                            diff,
                            pct_diff: diff / fv * 100.0,
                            currency: ctx.info.get(symbol).and_then(|i| i.2.clone()),
                            is_trailing,
                            origin: "holdings",
                            days,
                            volume_cross_pct,
                        });
                    }
                }
            }

            if (def.source == "watchlist" || def.source == "both") && (is_indicator || field_source == "watchlist") {
                for row in &watchlist_rows {
                    if entries.iter().any(|e| e.symbol == row.symbol) {
                        continue;
                    }
                    let Some(price) = watch_prices.get(&row.symbol).and_then(|p| p.price) else { continue };
                    let compare_value = if compare_volume {
                        match watch_prices.get(&row.symbol).and_then(|p| p.volume).filter(|v| *v > 0) {
                            Some(v) => v as f64,
                            None => continue,
                        }
                    } else {
                        price
                    };
                    let fv = if is_indicator {
                        cached_indicator(&row.symbol, field_key)
                    } else {
                        match field_key {
                            "breakthrough_price" => row.breakthrough_price,
                            "stop_loss_price" => row.stop_loss_price,
                            _ => row.custom_fields.get(field_key).and_then(|v| v.parse::<f64>().ok()),
                        }
                    };
                    let Some(fv) = fv.filter(|v| *v > 0.0) else { continue };
                    let diff = compare_value - fv;
                    if matches_op(diff) {
                        let (days, volume_cross_pct) = match cross_dir {
                            Some(dir) => {
                                let constant = if is_indicator { None } else { Some(fv) };
                                let volume = watch_prices.get(&row.symbol).and_then(|p| p.volume);
                                match cross_stats(&row.symbol, field_key, constant, dir, volume) {
                                    Some(stats) => (Some(stats.days), stats.volume_pct),
                                    None => continue,
                                }
                            }
                            None => (None, None),
                        };
                        entries.push(Entry {
                            symbol: row.symbol.clone(),
                            price,
                            compare_value,
                            field_value: fv,
                            diff,
                            pct_diff: diff / fv * 100.0,
                            currency: None,
                            is_trailing: false,
                            origin: "watchlist",
                            days,
                            volume_cross_pct,
                        });
                    }
                }
            }

            // A request-time override beats the list's configured direction, so
            // clicking the Difference header re-ranks before the truncate below
            // and can surface rows that were previously cut.
            let sort_dir = list_sort_overrides
                .get(&def.key)
                .map(|s| s.as_str())
                .or(def.sort.as_deref());
            let pct_op = def.operator == "pct_above" || def.operator == "pct_below";
            entries.sort_by(|a, b| {
                // A missing figure sorts last in the requested direction rather
                // than drifting to the top of a reversed list.
                let cmp = match metric {
                    "days" => a.days.unwrap_or(i64::MAX).cmp(&b.days.unwrap_or(i64::MAX)),
                    "volume_cross_pct" => a
                        .volume_cross_pct
                        .unwrap_or(f64::MAX)
                        .partial_cmp(&b.volume_cross_pct.unwrap_or(f64::MAX))
                        .unwrap_or(std::cmp::Ordering::Equal),
                    _ if pct_op => a.pct_diff.abs().partial_cmp(&b.pct_diff.abs()).unwrap_or(std::cmp::Ordering::Equal),
                    _ => a.pct_diff.partial_cmp(&b.pct_diff).unwrap_or(std::cmp::Ordering::Equal),
                };
                if sort_dir == Some("desc") { cmp.reverse() } else { cmp }
            });
            let limit = def.limit.unwrap_or(15);
            let truncated = entries.len() > limit;
            entries.truncate(limit);

            let builtin_labels: HashMap<&str, &str> = HashMap::from([
                ("sma50", "50-Day SMA"),
                ("sma150", "150-Day SMA"),
                ("ema40w", "40-Week EMA"),
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
                // Which side the diff is measured from, so the client can label
                // and format that column as a price or a share count.
                "compare": if compare_volume { "volume" } else { "price" },
                // Which column the rows are ranked by: "pct_diff", "days" or
                // "volume_cross_pct".
                "metric": metric,
                "field_label": field_label,
                // The direction actually applied — the request override if one
                // was given, else the list's config, else the "asc" default. The
                // client renders its sort indicator from this rather than
                // assuming, so the arrow is right on first load too.
                "sort": sort_dir.unwrap_or("asc"),
                // True when more rows qualified than `limit` allowed through, so
                // the UI can say the view is truncated rather than complete.
                "truncated": truncated,
                "entries": entries.iter().map(|e| serde_json::json!({
                    "symbol": e.symbol,
                    "price": e.price,
                    "compare_value": e.compare_value,
                    "field_value": e.field_value,
                    "diff": e.diff,
                    "pct_diff": e.pct_diff,
                    "currency": e.currency,
                    "is_trailing": e.is_trailing,
                    // Per-entry so a "both"-sourced indicator list sends each
                    // row to the screen its symbol actually lives on.
                    "origin": e.origin,
                    "days": e.days,
                    "volume_cross_pct": e.volume_cross_pct,
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
        let (stop_loss, is_trailing) = match effective_stop_loss(&conn, symbol, sym_fields, current_price, to_display) {
            Some((sl, trailing)) => (Some(sl), trailing),
            None => (None, false),
        };

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
        let ema40w = stored_weekly_ema(&conn, symbol, 40).map(&to_display);
        // Highest price *reached* over the window, so an intraday spike counts.
        // Taking the close instead understates it whenever a bar runs up and
        // gives the gain back — RMS.AX touched 3.79 on a day it closed at 3.67,
        // which put a real purchase at 3.76 above its own "30d High".
        //
        // The row filter stays on `close IS NOT NULL` so the window is still the
        // 30 most recent trading bars, and COALESCE falls back to the close for
        // bars the OHLC backfill could not reach.
        let high30d: Option<f64> = conn
            .prepare(
                "SELECT COALESCE(high, close) FROM prices
                  WHERE symbol = ?1 AND close IS NOT NULL
                  ORDER BY date DESC LIMIT 30",
            )
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
            "ema40w": ema40w,
            "high30d": high30d,
            "total_invested": invested,
            // Needed to express the gap to the stop loss as a position-level
            // dollar amount. Deriving it client-side from total_invested /
            // purchase_price breaks whenever purchase_price is absent or zero.
            "shares": shares,
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
    let latest_result = (|| -> Result<HashMap<String, String>, String> {
        let conn = open_db(db_path.as_ref()).map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare("SELECT table_name, MAX(timestamp) FROM audit_log GROUP BY table_name")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)))
            .map_err(|e| e.to_string())?;
        let mut latest = HashMap::new();
        for row in rows {
            let (table, ts) = row.map_err(|e| e.to_string())?;
            if let Some(ts) = ts {
                latest.insert(table, ts);
            }
        }
        Ok(latest)
    })();
    let latest = match latest_result {
        Ok(latest) => latest,
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "sync_state_fetch", "api", None, &err);
            return err_internal(err);
        }
    };
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
        "daily_prices_updated_at": config.get("daily_prices_updated_at"),
        "sold_prices_updated_at": config.get("sold_prices_updated_at"),
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
    let sectors: serde_json::Value = match config.get("sectors") {
        Some(raw) => match serde_json::from_str(raw) {
            Ok(parsed) => parsed,
            Err(err) => {
                let _ = insert_event_log(&db_path, "error", "config_parse", "api", Some("sectors"), &format!("Could not parse sectors, falling back to the built-in list: {err}"));
                serde_json::from_str(DEFAULT_SECTORS_JSON).unwrap()
            }
        },
        None => serde_json::from_str(DEFAULT_SECTORS_JSON).unwrap(),
    };
    let parse_defs = |key: &str| -> serde_json::Value {
        let defs: Vec<serde_json::Value> = config_json(&db_path, &config, key);
        serde_json::Value::Array(defs)
    };
    HttpResponse::Ok().json(serde_json::json!({
        "sectors": sectors,
        "currencies": SUPPORTED_CURRENCIES,
        // The cash ledger's vocabulary, published so clients build their
        // pickers from the server's list rather than a duplicated constant.
        "cash_transaction_kinds": CASH_TX_KINDS,
        "holdings_custom_fields": parse_defs("holdings_custom_fields"),
        "watchlist_custom_fields": parse_defs("watchlist_custom_fields"),
        "dashboard_custom_lists": parse_defs("dashboard_custom_lists"),
        "reserved_holdings_keys": ["stop_loss", "trailing_sell_pct", "trailing_sell_date", "sector", PL_BASIS_DATE, PL_BASIS_PRICE],
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
            cash_account_id: None,
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

    /// Enforces the Audit Logging rule in CLAUDE.md: every column of every
    /// audited table must appear in its triggers' `json_object(...)` payloads.
    ///
    /// Columns get added with `add_column_if_missing()` long after a trigger is
    /// written, and nothing about that fails loudly — the audit log keeps
    /// working and quietly omits the new field. That is exactly how
    /// `watchlist_symbols.breakthrough_price` and `stop_loss_price` went
    /// unrecorded, leaving them unrecoverable when the rows were wiped.
    #[test]
    fn audit_triggers_cover_every_column() {
        // High-volume machine-fetched data is deliberately not audited: `prices`
        // alone would dwarf the database, and both are re-derivable from Yahoo.
        const NOT_AUDITED: &[&str] = &[
            "prices",
            "cached_current_prices",
            "audit_log",
            "event_log",
            "watchlist_prices",
            "sqlite_sequence",
        ];

        let (_file, path) = setup_test_db();
        let conn = open_db(&path).unwrap();

        let tables: Vec<String> = {
            let mut stmt = conn
                .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
                .unwrap();
            let rows = stmt.query_map([], |row| row.get::<_, String>(0)).unwrap();
            rows.filter_map(|r| r.ok()).collect()
        };

        let mut failures: Vec<String> = Vec::new();
        for table in tables {
            if NOT_AUDITED.contains(&table.as_str()) {
                continue;
            }

            // Checked per trigger, not against all three concatenated: adding a
            // column to two of the three is the realistic mistake, and a
            // combined check would wave it through.
            let triggers: Vec<(String, String)> = {
                let mut stmt = conn
                    .prepare("SELECT name, sql FROM sqlite_master WHERE type='trigger' AND tbl_name = ?1")
                    .unwrap();
                let rows = stmt
                    .query_map(params![table], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
                    .unwrap();
                rows.filter_map(|r| r.ok()).collect()
            };
            if triggers.is_empty() {
                failures.push(format!("{}: no audit triggers at all", table));
                continue;
            }
            for action in ["insert", "update", "delete"] {
                if !triggers.iter().any(|(name, _)| name.ends_with(action)) {
                    failures.push(format!("{}: no {} trigger", table, action));
                }
            }

            let columns: Vec<String> = {
                let mut stmt = conn.prepare(&format!("PRAGMA table_info({})", table)).unwrap();
                let rows = stmt.query_map([], |row| row.get::<_, String>(1)).unwrap();
                rows.filter_map(|r| r.ok()).collect()
            };

            // Each trigger must reference the aliases it actually has: an
            // update sees both sides, and recording only one loses half the
            // change; an insert has only NEW, a delete only OLD.
            for column in &columns {
                for (name, sql) in &triggers {
                    let aliases: &[&str] = if name.ends_with("update") {
                        &["NEW", "OLD"]
                    } else if name.ends_with("insert") {
                        &["NEW"]
                    } else {
                        &["OLD"]
                    };
                    for alias in aliases {
                        if !sql.contains(&format!("{}.{}", alias, column)) {
                            failures.push(format!("{}: {}.{} missing from {}", table, alias, column, name));
                        }
                    }
                }
            }
        }

        assert!(failures.is_empty(), "audit trigger coverage gaps:\n  {}", failures.join("\n  "));
    }

    /// The static check above proves the trigger *text* names every column.
    /// This proves the values actually land in audit_log — and, critically,
    /// that re-running `init_db` replaces an existing trigger rather than
    /// leaving a stale `IF NOT EXISTS` definition in place.
    #[test]
    fn audit_log_records_watchlist_prices_after_reinit() {
        let (_file, path) = setup_test_db();
        // A second init_db is what a restart does; the drop-then-create must win.
        init_db(&path).unwrap();

        let conn = open_db(&path).unwrap();
        conn.execute(
            "INSERT INTO watchlist_symbols (symbol, notes, updated_at, breakthrough_price, stop_loss_price)
             VALUES ('TST.AX', 'thesis', '2026-01-01T00:00:00Z', 12.5, 9.75)",
            [],
        )
        .unwrap();
        conn.execute(
            "UPDATE watchlist_symbols SET breakthrough_price = 14.0 WHERE symbol = 'TST.AX'",
            [],
        )
        .unwrap();

        let (old_bt, new_bt, new_sl): (Option<f64>, Option<f64>, Option<f64>) = conn
            .query_row(
                "SELECT json_extract(old_values,'$.breakthrough_price'),
                        json_extract(new_values,'$.breakthrough_price'),
                        json_extract(new_values,'$.stop_loss_price')
                   FROM audit_log
                  WHERE table_name='watchlist_symbols' AND action='UPDATE'
                  ORDER BY id DESC LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(old_bt, Some(12.5), "pre-change breakthrough_price must be recorded");
        assert_eq!(new_bt, Some(14.0), "post-change breakthrough_price must be recorded");
        assert_eq!(new_sl, Some(9.75), "stop_loss_price must be recorded");
    }

    /// Regression: `init_db` runs on every API start, and the two legacy
    /// watchlist rebuilds only copy the columns they name. Step 1's guard is
    /// "list_name is absent", which is also true of the *normalised* table, so
    /// it used to fire on every restart — dropping notes, breakthrough_price
    /// and stop_loss_price, then Step 3 rebuilt the table and the ALTERs below
    /// re-added them empty. The schema looked right; the user's data was gone.
    #[test]
    fn repeated_init_db_preserves_watchlist_notes_and_prices() {
        let (_file, path) = setup_test_db();
        let conn = open_db(&path).unwrap();
        conn.execute(
            "INSERT INTO watchlist_symbols (symbol, notes, updated_at, breakthrough_price, stop_loss_price)
             VALUES ('TST.AX', 'my thesis', '2026-01-01T00:00:00Z', 12.5, 9.75)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO watchlist_memberships (symbol, list_name, added_at) VALUES ('TST.AX', 'Default', '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        drop(conn);

        // Three more startups, as if the server were restarted three times
        for _ in 0..3 {
            init_db(&path).unwrap();
        }

        let conn = open_db(&path).unwrap();
        let (notes, breakthrough, stop_loss): (Option<String>, Option<f64>, Option<f64>) = conn
            .query_row(
                "SELECT notes, breakthrough_price, stop_loss_price FROM watchlist_symbols WHERE symbol = 'TST.AX'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(notes.as_deref(), Some("my thesis"), "notes must survive restarts");
        assert_eq!(breakthrough, Some(12.5), "breakthrough_price must survive restarts");
        assert_eq!(stop_loss, Some(9.75), "stop_loss_price must survive restarts");

        // The membership must still be there too
        let memberships: i64 = conn
            .query_row("SELECT COUNT(*) FROM watchlist_memberships WHERE symbol = 'TST.AX'", [], |row| row.get(0))
            .unwrap();
        assert_eq!(memberships, 1);
    }

    fn insert_tx(db_path: &PathBuf, id: i64, tx_type: &str, date: &str, qty: f64, price: f64, brokerage: f64) {
        let conn = open_db(db_path).unwrap();
        conn.execute(
            "INSERT INTO holdings_transactions (id, symbol, transaction_type, date, quantity, price, brokerage, created_at)
             VALUES (?1, 'TST.AX', ?2, ?3, ?4, ?5, ?6, '2024-01-01T00:00:00Z')",
            rusqlite::params![id, tx_type, date, qty, price, brokerage],
        ).unwrap();
    }

    /// Drive the endpoint and hand back the parsed rows.
    async fn hindsight_rows(db_path: &PathBuf) -> Vec<serde_json::Value> {
        let app = actix_web::test::init_service(
            App::new().app_data(web::Data::new(db_path.clone())).service(get_hindsight),
        )
        .await;
        let req = actix_web::test::TestRequest::get().uri("/api/hindsight").to_request();
        let body: serde_json::Value = actix_web::test::call_and_read_body_json(&app, req).await;
        body.as_array().cloned().unwrap_or_default()
    }

    /// The row carries both sides: what the trade made, and what the shares did
    /// afterwards. The two are measured from different prices — the purchase
    /// for the first, the sale for the rest — which is the point of the screen.
    #[actix_web::test]
    async fn hindsight_prices_a_sale_against_what_came_after() {
        let (_file, db_path) = setup_test_db();
        insert_tx(&db_path, 1, "purchase", "2025-01-01", 100.0, 8.0, 0.0);
        insert_tx(&db_path, 2, "sale", "2025-01-03", 100.0, 10.0, 0.0);
        insert_price(&db_path, "TST.AX", "2025-01-10", 11.0); // +1 week
        insert_price(&db_path, "TST.AX", "2025-02-14", 12.0); // +6 weeks
        insert_price(&db_path, "TST.AX", "2025-04-03", 9.0);  // +3 months

        let rows = hindsight_rows(&db_path).await;
        assert_eq!(rows.len(), 1);
        let r = &rows[0];
        assert_eq!(r["symbol"], "TST.AX");
        assert_eq!(r["sale_price"], 10.0);
        assert_eq!(r["purchase_price"], 8.0);
        assert_eq!(r["quantity"], 100.0);
        assert_eq!(r["currency"], "AUD", "native only, and AUD is the default");
        assert_eq!(r["realised_pct"], 25.0, "the trade itself, measured from the purchase");

        assert_eq!(r["points"]["week1"]["value"], 11.0);
        assert_eq!(r["points"]["week1"]["status"], "ok");
        // 100 shares × $1 left on the table.
        assert_eq!(r["points"]["week1"]["delta"], 100.0);
        assert_eq!(r["points"]["week6"]["value"], 12.0);
        assert_eq!(r["points"]["month3"]["value"], 9.0);
        assert_eq!(r["points"]["peak"]["value"], 12.0);
        assert_eq!(r["points"]["low"]["value"], 9.0);
    }

    /// A stock bought and sold more than once gets a row per sale, each costed
    /// against the lots that sale actually consumed.
    #[actix_web::test]
    async fn hindsight_reports_every_sale_separately() {
        let (_file, db_path) = setup_test_db();
        insert_tx(&db_path, 1, "purchase", "2025-01-01", 100.0, 10.0, 0.0);
        insert_tx(&db_path, 2, "sale", "2025-02-01", 100.0, 15.0, 0.0);
        insert_tx(&db_path, 3, "purchase", "2025-03-01", 100.0, 20.0, 0.0);
        insert_tx(&db_path, 4, "sale", "2025-04-01", 100.0, 25.0, 0.0);
        insert_price(&db_path, "TST.AX", "2025-05-01", 30.0);

        let rows = hindsight_rows(&db_path).await;
        assert_eq!(rows.len(), 2);
        // Most recent first.
        assert_eq!(rows[0]["sale_date"], "2025-04-01");
        assert_eq!(rows[0]["purchase_price"], 20.0);
        assert_eq!(rows[1]["sale_date"], "2025-02-01");
        assert_eq!(rows[1]["purchase_price"], 10.0, "the first sale ate the cheaper lot");
    }

    /// A delisted symbol's windows never arrive. Reporting them as missing data
    /// would send the reader looking for a backfill that cannot exist.
    #[actix_web::test]
    async fn hindsight_marks_a_delisted_symbols_windows_as_delisted() {
        let (_file, db_path) = setup_test_db();
        insert_tx(&db_path, 1, "purchase", "2025-03-28", 700.0, 2.25, 0.0);
        insert_tx(&db_path, 2, "sale", "2025-08-01", 700.0, 3.91, 0.0);
        open_db(&db_path)
            .unwrap()
            .execute_batch(
                "INSERT INTO app_config (key, value) VALUES ('dead_symbol_TST.AX', '2025-08-01')",
            )
            .unwrap();

        let rows = hindsight_rows(&db_path).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["delisted_on"], "2025-08-01");
        for point in ["week1", "week6", "month3", "peak", "low", "current"] {
            assert_eq!(rows[0]["points"][point]["status"], "delisted", "{point}");
        }
    }

    /// Only sales are second-guessed. A position still held belongs to the
    /// holdings screens, and listing it here would be asking about a decision
    /// that has not been made.
    #[actix_web::test]
    async fn hindsight_ignores_a_position_still_held() {
        let (_file, db_path) = setup_test_db();
        insert_tx(&db_path, 1, "purchase", "2025-01-01", 100.0, 10.0, 0.0);
        insert_price(&db_path, "TST.AX", "2025-06-01", 20.0);

        assert!(hindsight_rows(&db_path).await.is_empty());
    }

    /// A refresh has to converge on what Yahoo now reports. Upserting alone let
    /// the table grow: when a date fix shifted ex-dates by a day, each refresh
    /// added a parallel copy of every dividend instead of correcting it, and
    /// the duplicates were then paid twice into the cash ledger.
    #[test]
    fn refetching_dividends_replaces_the_symbols_history() {
        let (_file, db_path) = setup_test_db();
        let stored = |db: &PathBuf| -> Vec<(String, f64)> {
            let conn = open_db(db).unwrap();
            let mut stmt = conn
                .prepare("SELECT ex_date, amount FROM dividend_events WHERE symbol = 'TST.AX' ORDER BY ex_date")
                .unwrap();
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
            rows.filter_map(|r| r.ok()).collect()
        };
        let event = |ex_date: &str, amount: f64| DividendEvent {
            symbol: "TST.AX".to_string(),
            ex_date: NaiveDate::parse_from_str(ex_date, "%Y-%m-%d").unwrap(),
            payment_date: None,
            record_date: None,
            amount,
            fetched_at: "2026-08-14T00:00:00Z".to_string(),
        };

        store_dividend_events_for_symbol(&db_path, "TST.AX", &[event("2025-02-27", 0.10)]).unwrap();
        assert_eq!(stored(&db_path), vec![("2025-02-27".to_string(), 0.10)]);

        // The same dividend, re-reported a day later after the date fix.
        store_dividend_events_for_symbol(&db_path, "TST.AX", &[event("2025-02-28", 0.10)]).unwrap();
        assert_eq!(
            stored(&db_path),
            vec![("2025-02-28".to_string(), 0.10)],
            "the corrected date should replace the old one, not sit alongside it"
        );

        // A failed fetch reports nothing; that is not evidence of no dividends.
        store_dividend_events_for_symbol(&db_path, "TST.AX", &[]).unwrap();
        assert_eq!(
            stored(&db_path),
            vec![("2025-02-28".to_string(), 0.10)],
            "an empty fetch must not erase real history"
        );
    }

    /// Yahoo repeats some distributions on adjacent days with an identical
    /// amount — one payment described twice. Paid into the cash ledger as-is,
    /// the holder is credited double.
    #[test]
    fn duplicate_dividend_reports_collapse_to_the_earliest() {
        let ev = |ex_date: &str, amount: f64| DividendEvent {
            symbol: "VAE.AX".to_string(),
            ex_date: NaiveDate::parse_from_str(ex_date, "%Y-%m-%d").unwrap(),
            payment_date: None,
            record_date: None,
            amount,
            fetched_at: "x".to_string(),
        };
        let dates = |events: Vec<DividendEvent>| -> Vec<String> {
            events.iter().map(|e| e.ex_date.format("%Y-%m-%d").to_string()).collect()
        };

        // The real VAE.AX case: 86400 apart, identical to six decimals.
        assert_eq!(
            dates(dedupe_dividend_events(&[ev("2025-07-02", 0.677876), ev("2025-07-01", 0.677876)])),
            vec!["2025-07-01"],
            "the earlier date is the true ex-date"
        );
        // Also seen three days apart.
        assert_eq!(
            dates(dedupe_dividend_events(&[ev("2024-10-01", 0.72522), ev("2024-10-04", 0.72522)])),
            vec!["2024-10-01"]
        );
        // Quarterly distributions of the same size must survive — far apart.
        assert_eq!(
            dates(dedupe_dividend_events(&[ev("2025-01-02", 0.5), ev("2025-04-01", 0.5)])).len(),
            2,
            "a genuine repeat months later is not a duplicate"
        );
        // Different amounts on adjacent days are two real events.
        assert_eq!(
            dates(dedupe_dividend_events(&[ev("2025-07-01", 0.10), ev("2025-07-02", 0.25)])).len(),
            2
        );
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

    /// The two sets have to partition the symbols between them: a symbol priced
    /// by neither stops being fetched, and one priced by both is fetched twice
    /// every refresh.
    #[test]
    fn exited_and_held_symbols_do_not_overlap() {
        let (_file, db_path) = setup_test_db();
        // Fully sold.
        insert_tx(&db_path, 1, "purchase", "2024-01-01", 100.0, 10.0, 0.0);
        insert_tx(&db_path, 2, "sale", "2024-06-01", 100.0, 12.0, 0.0);

        let exited = load_exited_symbols(&db_path).unwrap();
        let held = load_holding_symbols(&db_path).unwrap();
        assert_eq!(exited, vec!["TST.AX".to_string()]);
        assert!(held.is_empty());
        assert!(exited.iter().all(|s| !held.contains(s)));
    }

    /// A part-sold position is still held, so the holdings refresh already
    /// prices it. Counting it as exited too would fetch it twice.
    #[test]
    fn a_partly_sold_holding_is_not_treated_as_exited() {
        let (_file, db_path) = setup_test_db();
        insert_tx(&db_path, 1, "purchase", "2024-01-01", 100.0, 10.0, 0.0);
        insert_tx(&db_path, 2, "sale", "2024-06-01", 40.0, 12.0, 0.0);

        assert!(load_exited_symbols(&db_path).unwrap().is_empty());
        assert_eq!(load_holding_symbols(&db_path).unwrap(), vec!["TST.AX".to_string()]);
    }

    /// A holding that was never sold is not exited — the sale is what makes it
    /// interesting to Hindsight, not the absence of shares.
    #[test]
    fn a_never_sold_symbol_is_not_exited() {
        let (_file, db_path) = setup_test_db();
        insert_tx(&db_path, 1, "purchase", "2024-01-01", 100.0, 10.0, 0.0);
        assert!(load_exited_symbols(&db_path).unwrap().is_empty());
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
            cash_account_id: None,
            withholding_amount: None,
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
            cash_account_id: None,
            withholding_amount: None,
            confirm: None,
        };

        let result = insert_holding_transaction(&db_path, "TST.AX", payload);
        assert!(result.is_err(), "should reject zero quantity");
    }

    // -------------------------------------------------------------------------
    // Yahoo timestamp → trading-date conversion (gmtoffset)
    //
    // Yahoo stamps daily bars at the market open. ASX opens 10:00 Sydney,
    // which during daylight saving (AEDT, Oct–Apr) is 23:00 UTC the previous
    // day — dating bars in UTC filed every AEDT trading day one day early
    // (Monday closes stored under Sunday). These tests pin the fix.
    // -------------------------------------------------------------------------

    const AEDT: i64 = 11 * 3600;
    const AEST: i64 = 10 * 3600;
    const EST: i64 = -5 * 3600;

    fn utc_ts(y: i32, m: u32, d: u32, h: u32, min: u32) -> i64 {
        Utc.with_ymd_and_hms(y, m, d, h, min, 0).unwrap().timestamp()
    }

    #[test]
    fn yahoo_local_date_aedt_open_dates_as_local_trading_day() {
        // Mon 2026-01-05 10:00 AEDT = Sun 2026-01-04 23:00 UTC
        let ts = utc_ts(2026, 1, 4, 23, 0);
        assert_eq!(yahoo_local_date(ts, Some(AEDT)), NaiveDate::from_ymd_opt(2026, 1, 5));
    }

    #[test]
    fn yahoo_local_date_aest_open_is_unchanged() {
        // Mon 2026-06-01 10:00 AEST = Mon 2026-06-01 00:00 UTC
        let ts = utc_ts(2026, 6, 1, 0, 0);
        assert_eq!(yahoo_local_date(ts, Some(AEST)), NaiveDate::from_ymd_opt(2026, 6, 1));
    }

    #[test]
    fn yahoo_local_date_us_open_is_unchanged() {
        // Mon 2026-01-05 09:30 EST = Mon 2026-01-05 14:30 UTC — proves the
        // fix does not shift US-listed symbols
        let ts = utc_ts(2026, 1, 5, 14, 30);
        assert_eq!(yahoo_local_date(ts, Some(EST)), NaiveDate::from_ymd_opt(2026, 1, 5));
    }

    #[test]
    fn yahoo_local_date_missing_offset_falls_back_to_utc() {
        let ts = utc_ts(2026, 1, 4, 23, 0);
        assert_eq!(yahoo_local_date(ts, None), NaiveDate::from_ymd_opt(2026, 1, 4));
    }

    /// gmtoffset must survive deserialisation of the history payload — if
    /// the field is dropped, every AEDT bar silently shifts a day early.
    #[test]
    fn history_response_parses_gmtoffset() {
        let json = r#"{
            "chart": {
                "result": [{
                    "meta": { "currency": "AUD", "gmtoffset": 39600 },
                    "timestamp": [1767564000],
                    "indicators": { "quote": [{ "close": [37.39], "volume": [100000] }] }
                }],
                "error": null
            }
        }"#;
        let payload: YahooHistoryResponse = serde_json::from_str(json).unwrap();
        let result = &payload.chart.result.unwrap()[0];
        let gmtoffset = result.meta.as_ref().and_then(|m| m.gmtoffset);
        assert_eq!(gmtoffset, Some(AEDT));
        // 1767564000 = 2026-01-04 22:00 UTC = 2026-01-05 09:00 AEDT
        assert_eq!(
            yahoo_local_date(result.timestamp.as_ref().unwrap()[0], gmtoffset),
            NaiveDate::from_ymd_opt(2026, 1, 5)
        );
    }

    // -------------------------------------------------------------------------
    // effective_stop_loss — the one function that decides the stop-loss
    // number shown on the Analysis screen, the Dashboard Stop Losses list
    // and the Holdings SMA chart. Manual field wins; otherwise the
    // trailing-sell trigger is the highest close since the placement date
    // (plus the live price) minus the trailing percentage.
    // -------------------------------------------------------------------------

    fn sym_fields(pairs: &[(&str, &str)]) -> std::collections::HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    fn insert_price(db_path: &PathBuf, symbol: &str, date: &str, close: f64) {
        let conn = open_db(db_path).unwrap();
        conn.execute(
            "INSERT INTO prices (symbol, date, close, fetched_at) VALUES (?1, ?2, ?3, ?4)",
            params![symbol, date, close, "2026-01-01T00:00:00Z"],
        )
        .unwrap();
    }

    #[test]
    fn stop_loss_manual_field_wins_over_trailing() {
        let (_file, db_path) = setup_test_db();
        let conn = open_db(&db_path).unwrap();
        let fields = sym_fields(&[
            ("stop_loss", "4.50"),
            ("trailing_sell_pct", "10"),
            ("trailing_sell_date", "2026-05-01"),
        ]);
        let result = effective_stop_loss(&conn, "TST.AX", Some(&fields), Some(5.0), |p| p);
        assert_eq!(result, Some((4.50, false)));
    }

    #[test]
    fn stop_loss_zero_manual_falls_back_to_trailing() {
        let (_file, db_path) = setup_test_db();
        let conn = open_db(&db_path).unwrap();
        let fields = sym_fields(&[("stop_loss", "0"), ("trailing_sell_pct", "10")]);
        let result = effective_stop_loss(&conn, "TST.AX", Some(&fields), Some(10.0), |p| p);
        assert_eq!(result, Some((9.0, true)));
    }

    fn insert_ohlc(db_path: &PathBuf, symbol: &str, date: &str, high: f64, close: f64) {
        let conn = open_db(db_path).unwrap();
        conn.execute(
            "INSERT INTO prices (symbol, date, high, close, fetched_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![symbol, date, high, close, "2026-01-01T00:00:00Z"],
        )
        .unwrap();
    }

    /// Bars written before OHLC ingest existed have a NULL high, so the peak
    /// falls back to the close for those rows rather than skipping them.
    #[test]
    fn trailing_falls_back_to_close_when_high_is_missing() {
        let (_file, db_path) = setup_test_db();
        insert_price(&db_path, "TST.AX", "2026-05-01", 20.0); // before placement — excluded
        insert_price(&db_path, "TST.AX", "2026-05-12", 12.0);
        insert_price(&db_path, "TST.AX", "2026-06-01", 11.0);
        let conn = open_db(&db_path).unwrap();
        let fields = sym_fields(&[("trailing_sell_pct", "10"), ("trailing_sell_date", "2026-05-10")]);
        let result = effective_stop_loss(&conn, "TST.AX", Some(&fields), Some(11.5), |p| p);
        // Reference = max(12.0, 11.0, live 11.5) = 12.0; trigger = 12.0 × 0.90
        let (sl, trailing) = result.unwrap();
        assert!((sl - 10.8).abs() < 1e-9, "expected 10.8, got {sl}");
        assert!(trailing);
    }

    /// A trailing stop ratchets on the highest price *reached*, which is what a
    /// broker trails on. This is the TXG case: an intraday spike to 60.69 that
    /// closed back at 58.48 must still lift the stop.
    #[test]
    fn trailing_uses_intraday_high_not_the_close() {
        let (_file, db_path) = setup_test_db();
        insert_ohlc(&db_path, "TST.AX", "2026-06-10", 52.22, 52.03);
        insert_ohlc(&db_path, "TST.AX", "2026-06-11", 59.87, 58.57);
        insert_ohlc(&db_path, "TST.AX", "2026-06-12", 60.69, 58.48); // spiked, gave it back
        let conn = open_db(&db_path).unwrap();
        let fields = sym_fields(&[("trailing_sell_pct", "15"), ("trailing_sell_date", "2026-06-09")]);

        let (sl, trailing) = effective_stop_loss(&conn, "TST.AX", Some(&fields), Some(58.48), |p| p).unwrap();
        assert!(trailing);
        // Peak 60.69 × 0.85 = 51.5865, matching the broker — not 58.57 × 0.85 = 49.78
        assert!((sl - 51.5865).abs() < 1e-4, "expected ~51.59 from the high, got {sl}");
        assert!(sl > 58.57 * 0.85, "must not trail the highest close");
    }

    /// Mixed history: a backfilled bar with a high and a legacy close-only bar
    /// whose close beats every recorded high.
    #[test]
    fn trailing_peak_spans_bars_with_and_without_highs() {
        let (_file, db_path) = setup_test_db();
        insert_ohlc(&db_path, "TST.AX", "2026-05-12", 11.0, 10.0);
        insert_price(&db_path, "TST.AX", "2026-05-13", 12.0); // no high; close is the peak
        let conn = open_db(&db_path).unwrap();
        let fields = sym_fields(&[("trailing_sell_pct", "10"), ("trailing_sell_date", "2026-05-10")]);

        let (sl, _) = effective_stop_loss(&conn, "TST.AX", Some(&fields), Some(9.0), |p| p).unwrap();
        assert!((sl - 10.8).abs() < 1e-9, "peak should be 12.0 from the close-only bar, got stop {sl}");
    }

    /// The live price still counts: an intraday move above every stored bar
    /// lifts the stop immediately rather than waiting for the bar to land.
    #[test]
    fn trailing_peak_includes_the_live_price() {
        let (_file, db_path) = setup_test_db();
        insert_ohlc(&db_path, "TST.AX", "2026-05-12", 11.0, 10.5);
        let conn = open_db(&db_path).unwrap();
        let fields = sym_fields(&[("trailing_sell_pct", "10"), ("trailing_sell_date", "2026-05-10")]);

        let (sl, _) = effective_stop_loss(&conn, "TST.AX", Some(&fields), Some(20.0), |p| p).unwrap();
        assert!((sl - 18.0).abs() < 1e-9, "live 20.0 should set the peak, got stop {sl}");
    }

    #[test]
    fn trailing_live_price_can_be_the_reference() {
        let (_file, db_path) = setup_test_db();
        insert_price(&db_path, "TST.AX", "2026-05-12", 12.0);
        let conn = open_db(&db_path).unwrap();
        let fields = sym_fields(&[("trailing_sell_pct", "10"), ("trailing_sell_date", "2026-05-10")]);
        // Live price above every stored close — the trigger trails the live high
        let result = effective_stop_loss(&conn, "TST.AX", Some(&fields), Some(13.0), |p| p);
        let (sl, trailing) = result.unwrap();
        assert!((sl - 11.7).abs() < 1e-9, "expected 11.7, got {sl}");
        assert!(trailing);
    }

    #[test]
    fn trailing_without_placement_date_uses_current_price_only() {
        let (_file, db_path) = setup_test_db();
        insert_price(&db_path, "TST.AX", "2026-05-12", 15.0); // ignored without a date
        let conn = open_db(&db_path).unwrap();
        let fields = sym_fields(&[("trailing_sell_pct", "10")]);
        let result = effective_stop_loss(&conn, "TST.AX", Some(&fields), Some(10.0), |p| p);
        assert_eq!(result, Some((9.0, true)));
    }

    #[test]
    fn trailing_empty_placement_date_is_treated_as_absent() {
        let (_file, db_path) = setup_test_db();
        insert_price(&db_path, "TST.AX", "2026-05-12", 15.0);
        let conn = open_db(&db_path).unwrap();
        let fields = sym_fields(&[("trailing_sell_pct", "10"), ("trailing_sell_date", "")]);
        let result = effective_stop_loss(&conn, "TST.AX", Some(&fields), Some(10.0), |p| p);
        assert_eq!(result, Some((9.0, true)));
    }

    #[test]
    fn stop_loss_none_when_no_fields() {
        let (_file, db_path) = setup_test_db();
        let conn = open_db(&db_path).unwrap();
        assert_eq!(effective_stop_loss(&conn, "TST.AX", None, Some(10.0), |p| p), None);
        let empty = sym_fields(&[]);
        assert_eq!(effective_stop_loss(&conn, "TST.AX", Some(&empty), Some(10.0), |p| p), None);
    }

    #[test]
    fn stop_loss_zero_or_negative_trailing_pct_ignored() {
        let (_file, db_path) = setup_test_db();
        let conn = open_db(&db_path).unwrap();
        let zero = sym_fields(&[("trailing_sell_pct", "0")]);
        assert_eq!(effective_stop_loss(&conn, "TST.AX", Some(&zero), Some(10.0), |p| p), None);
        let negative = sym_fields(&[("trailing_sell_pct", "-5")]);
        assert_eq!(effective_stop_loss(&conn, "TST.AX", Some(&negative), Some(10.0), |p| p), None);
    }

    #[test]
    fn trailing_none_when_no_price_and_no_closes() {
        let (_file, db_path) = setup_test_db();
        let conn = open_db(&db_path).unwrap();
        let fields = sym_fields(&[("trailing_sell_pct", "10"), ("trailing_sell_date", "2026-05-10")]);
        assert_eq!(effective_stop_loss(&conn, "TST.AX", Some(&fields), None, |p| p), None);
    }

    #[test]
    fn trailing_conversion_applies_to_stored_closes() {
        let (_file, db_path) = setup_test_db();
        insert_price(&db_path, "TST", "2026-05-12", 10.0); // native close
        let conn = open_db(&db_path).unwrap();
        let fields = sym_fields(&[("trailing_sell_pct", "10"), ("trailing_sell_date", "2026-05-10")]);
        // Display currency is 2× native (e.g. USD→AUD); current price is
        // already converted. Reference = max(10.0 × 2, 18.0) = 20.0.
        let result = effective_stop_loss(&conn, "TST", Some(&fields), Some(18.0), |p| p * 2.0);
        let (sl, trailing) = result.unwrap();
        assert!((sl - 18.0).abs() < 1e-9, "expected 18.0, got {sl}");
        assert!(trailing);
    }

    /// Same guarantee for the dividends payload: ASX ex-dividend dates are
    /// stamped at the market open and shift a day early without the offset.
    #[test]
    fn dividend_response_parses_gmtoffset() {
        let json = r#"{
            "chart": {
                "result": [{
                    "meta": { "currency": "AUD", "gmtoffset": 39600 },
                    "events": {
                        "dividends": {
                            "1767564000": { "amount": 0.44, "date": 1767564000 }
                        }
                    }
                }],
                "error": null
            }
        }"#;
        let payload: YahooDivResponse = serde_json::from_str(json).unwrap();
        let result = payload.chart.result.unwrap().into_iter().next().unwrap();
        let gmtoffset = result.meta.as_ref().and_then(|m| m.gmtoffset);
        assert_eq!(gmtoffset, Some(AEDT));
        let entry_ts = result
            .events
            .unwrap()
            .dividends
            .unwrap()
            .values()
            .next()
            .unwrap()
            .date
            .unwrap();
        assert_eq!(yahoo_local_date(entry_ts, gmtoffset), NaiveDate::from_ymd_opt(2026, 1, 5));
    }

    // -------------------------------------------------------------------------
    // Portfolio endpoint handler tests (P1.3)
    //
    // Full actix App against a seeded temp DB. All symbols are AUD so no
    // handler touches the network (FX resolution short-circuits on an empty
    // currency list; the watchlist is empty so no history fetches run).
    //
    // Fixture:
    //   MAN.AX  — 100 @ $10, manual stop loss $9,   cached $12, 150-SMA $10
    //   TRL.AX  —  50 @ $20, 10% trail since 1 May (peak close $30 → $27), cached $28
    //   PART.AX — 100 @ $1, 40 sold @ $2 → 60 remaining, cached $1.50, 150-SMA $3
    //   SOLD.AX —  10 @ $5, all sold @ $6 → excluded everywhere
    // -------------------------------------------------------------------------

    fn seed_portfolio_fixture() -> (NamedTempFile, PathBuf) {
        let (file, db_path) = setup_test_db();
        let conn = open_db(&db_path).unwrap();

        let mut tx_id = 1000;
        let mut add_tx = |symbol: &str, tx_type: &str, date: &str, qty: f64, price: f64| {
            tx_id += 1;
            conn.execute(
                "INSERT INTO holdings_transactions (id, symbol, transaction_type, date, quantity, price, brokerage, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0.0, '2026-01-01T00:00:00Z')",
                params![tx_id, symbol, tx_type, date, qty, price],
            )
            .unwrap();
        };
        add_tx("MAN.AX", "purchase", "2026-01-05", 100.0, 10.0);
        add_tx("TRL.AX", "purchase", "2026-02-02", 50.0, 20.0);
        add_tx("PART.AX", "purchase", "2026-01-05", 100.0, 1.0);
        add_tx("PART.AX", "sale", "2026-03-02", 40.0, 2.0);
        add_tx("SOLD.AX", "purchase", "2026-01-05", 10.0, 5.0);
        add_tx("SOLD.AX", "sale", "2026-04-01", 10.0, 6.0);

        for (sym, price, volume) in [
            ("MAN.AX", 12.0, 500_000_i64),
            ("TRL.AX", 28.0, 120_000),
            ("PART.AX", 1.5, 90_000),
            ("SOLD.AX", 7.0, 10_000),
        ] {
            conn.execute(
                "INSERT INTO cached_current_prices (symbol, price, volume, last_updated, price_date)
                 VALUES (?1, ?2, ?3, '2026-07-11T00:00:00Z', '2026-07-10')",
                params![sym, price, volume],
            )
            .unwrap();
        }

        for (sym, key, value) in [
            ("MAN.AX", "stop_loss", "9.0"),
            ("TRL.AX", "trailing_sell_pct", "10"),
            ("TRL.AX", "trailing_sell_date", "2026-05-01"),
        ] {
            conn.execute(
                "INSERT INTO holdings_symbol_fields (symbol, field_key, value) VALUES (?1, ?2, ?3)",
                params![sym, key, value],
            )
            .unwrap();
        }

        // Trailing reference closes for TRL.AX (peak 30.0 since 1 May)
        for (date, close) in [("2026-05-15", 30.0), ("2026-06-01", 25.0)] {
            conn.execute(
                "INSERT INTO prices (symbol, date, close, fetched_at) VALUES ('TRL.AX', ?1, ?2, '2026-07-01T00:00:00Z')",
                params![date, close],
            )
            .unwrap();
        }

        // Flat 150-day history so the 150-SMA equals the close: MAN.AX at
        // 10.0 (price 12 → +20%), PART.AX at 3.0 (price 1.50 → −50%).
        // TRL.AX has only 2 closes → no SMA → excluded from worst holdings.
        let start = NaiveDate::from_ymd_opt(2025, 8, 1).unwrap();
        let mut stmt = conn
            .prepare("INSERT INTO prices (symbol, date, close, fetched_at) VALUES (?1, ?2, ?3, '2026-07-01T00:00:00Z')")
            .unwrap();
        for i in 0..150 {
            let date = (start + chrono::Duration::days(i)).format("%Y-%m-%d").to_string();
            stmt.execute(params!["MAN.AX", date, 10.0]).unwrap();
            stmt.execute(params!["PART.AX", date, 3.0]).unwrap();
        }
        drop(stmt);

        conn.execute(
            "INSERT INTO app_config (key, value) VALUES ('dashboard_custom_lists',
             '[{\"key\":\"stop_losses\",\"label\":\"Stop Losses\",\"source\":\"holdings\",\"field_key\":\"holdings:stop_loss\",\"operator\":\"pct_below\",\"limit\":20}]')",
            [],
        )
        .unwrap();

        (file, db_path)
    }

    async fn get_json(db_path: &std::path::Path, uri: &str) -> serde_json::Value {
        let app = actix_web::test::init_service(
            App::new()
                .app_data(web::Data::new(db_path.to_path_buf()))
                .service(get_portfolio_holdings)
                .service(get_portfolio_overview)
                .service(get_portfolio_risk),
        )
        .await;
        let req = actix_web::test::TestRequest::get().uri(uri).to_request();
        let resp = actix_web::test::call_service(&app, req).await;
        assert!(resp.status().is_success(), "{} returned {}", uri, resp.status());
        actix_web::test::read_body_json(resp).await
    }

    fn find<'a>(rows: &'a serde_json::Value, symbol: &str) -> &'a serde_json::Value {
        rows.as_array()
            .unwrap()
            .iter()
            .find(|r| r["symbol"] == symbol)
            .unwrap_or_else(|| panic!("{symbol} missing from response"))
    }

    fn close_to(v: &serde_json::Value, expected: f64) -> bool {
        v.as_f64().map(|f| (f - expected).abs() < 1e-6).unwrap_or(false)
    }

    /// Re-point the stop_losses list at a smaller limit so the truncate is the
    /// thing under test.
    fn set_stop_loss_limit(db_path: &std::path::Path, limit: usize) {
        let conn = open_db(&db_path.to_path_buf()).unwrap();
        conn.execute(
            "INSERT INTO app_config (key, value) VALUES ('dashboard_custom_lists', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![format!(
                "[{{\"key\":\"stop_losses\",\"label\":\"Stop Losses\",\"source\":\"holdings\",\"field_key\":\"holdings:stop_loss\",\"operator\":\"pct_below\",\"limit\":{}}}]",
                limit
            )],
        )
        .unwrap();
    }

    fn stop_loss_list(body: &serde_json::Value) -> &serde_json::Value {
        body["custom_lists"]
            .as_array()
            .unwrap()
            .iter()
            .find(|l| l["key"] == "stop_losses")
            .expect("stop_losses list missing")
    }

    /// The whole point of sorting server-side: the list is ranked *then* cut to
    /// `limit`, so flipping the direction must change which rows survive — not
    /// just their order. Reversing client-side could never surface the row that
    /// fell outside the cut (the TXG case).
    #[actix_web::test]
    async fn list_sort_override_changes_which_rows_survive_the_limit() {
        let (_file, db_path) = seed_portfolio_fixture();
        // Two qualifying holdings: TRL.AX at +3.7%, MAN.AX at +33.33%.
        set_stop_loss_limit(&db_path, 1);

        // Ascending (the default) keeps the row closest to triggering.
        let body = get_json(&db_path, "/api/portfolio/overview").await;
        let list = stop_loss_list(&body);
        let entries = list["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["symbol"], "TRL.AX");
        assert_eq!(list["sort"], "asc");
        assert_eq!(list["truncated"], true, "a row was cut, so the list is truncated");

        // Descending re-ranks before the cut and surfaces the other row.
        let body = get_json(&db_path, "/api/portfolio/overview?list_sort=stop_losses:desc").await;
        let list = stop_loss_list(&body);
        let entries = list["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0]["symbol"], "MAN.AX",
            "desc must surface the furthest-from-trigger row, which asc cut"
        );
        assert_eq!(list["sort"], "desc");
    }

    #[actix_web::test]
    async fn list_sort_override_is_scoped_and_fails_safe() {
        let (_file, db_path) = seed_portfolio_fixture();
        set_stop_loss_limit(&db_path, 1);

        // An override naming a different list must not affect stop_losses
        let body = get_json(&db_path, "/api/portfolio/overview?list_sort=some_other_list:desc").await;
        assert_eq!(stop_loss_list(&body)["entries"][0]["symbol"], "TRL.AX");

        // Malformed values degrade to the configured order rather than erroring
        for uri in [
            "/api/portfolio/overview?list_sort=stop_losses:sideways",
            "/api/portfolio/overview?list_sort=stop_losses",
            "/api/portfolio/overview?list_sort=",
        ] {
            let body = get_json(&db_path, uri).await;
            let list = stop_loss_list(&body);
            assert_eq!(list["entries"][0]["symbol"], "TRL.AX", "{uri} should fall back to configured order");
            assert_eq!(list["sort"], "asc");
        }
    }

    #[test]
    fn parse_list_sort_accepts_only_valid_directions() {
        let parsed = parse_list_sort(Some("a:desc, b:ASC ,c:nonsense,d,:desc,e:"));
        assert_eq!(parsed.get("a").map(String::as_str), Some("desc"));
        assert_eq!(parsed.get("b").map(String::as_str), Some("asc"), "whitespace and case tolerated");
        assert!(!parsed.contains_key("c"));
        assert!(!parsed.contains_key("d"));
        assert!(!parsed.contains_key("e"));
        assert!(parse_list_sort(None).is_empty());
    }

    #[actix_web::test]
    async fn holdings_endpoint_reports_positions_and_stop_losses() {
        let (_file, db_path) = seed_portfolio_fixture();
        let body = get_json(&db_path, "/api/portfolio/holdings").await;
        let holdings = &body["holdings"];
        assert_eq!(holdings.as_array().unwrap().len(), 3, "fully sold symbol must be excluded");

        let man = find(holdings, "MAN.AX");
        assert!(close_to(&man["shares"], 100.0));
        assert!(close_to(&man["invested"], 1000.0));
        assert!(close_to(&man["avg_cost"], 10.0));
        assert!(close_to(&man["current_price"], 12.0));
        assert!(close_to(&man["pl"], 200.0));
        assert!(close_to(&man["pl_pct"], 20.0));
        assert!(close_to(&man["stop_loss"], 9.0), "manual stop loss passes through");
        assert_eq!(man["is_trailing_sell"], false);

        let trl = find(holdings, "TRL.AX");
        assert!(close_to(&trl["stop_loss"], 27.0), "trailing trigger = peak 30.0 × 0.90");
        assert_eq!(trl["is_trailing_sell"], true);

        let part = find(holdings, "PART.AX");
        assert!(close_to(&part["shares"], 60.0), "partial sale leaves remaining shares only");
        assert!(close_to(&part["invested"], 60.0));
        assert!(part["stop_loss"].is_null());
    }

    #[actix_web::test]
    async fn overview_endpoint_totals_reconcile_with_seeded_data() {
        let (_file, db_path) = seed_portfolio_fixture();
        let body = get_json(&db_path, "/api/portfolio/overview").await;

        let totals = &body["totals"];
        assert_eq!(totals["stock_count"], 3);
        // 100×12 + 50×28 + 60×1.5
        assert!(close_to(&totals["total_value"], 2690.0));
        // Holdings P/L 630 (200 + 400 + 30) + sold P/L 50 (PART 40 + SOLD 10)
        assert!(close_to(&totals["holdings_pl"], 630.0));
        assert!(close_to(&totals["sold_pl"], 50.0));
        assert!(close_to(&totals["total_pl"], 680.0));
    }

    #[actix_web::test]
    async fn overview_worst_holdings_sorted_by_sma_gap() {
        let (_file, db_path) = seed_portfolio_fixture();
        let body = get_json(&db_path, "/api/portfolio/overview").await;

        let worst = body["worst_holdings"].as_array().unwrap();
        // TRL.AX has no 150-SMA (2 closes) and must be absent
        assert_eq!(worst.len(), 2);
        // PART.AX (−50%) is worse than MAN.AX (+20%) and must sort first
        assert_eq!(worst[0]["symbol"], "PART.AX");
        assert!(close_to(&worst[0]["pct_diff"], -50.0));
        assert_eq!(worst[1]["symbol"], "MAN.AX");
        assert!(close_to(&worst[1]["pct_diff"], 20.0));
    }

    #[actix_web::test]
    async fn overview_stop_loss_list_includes_trailing_holdings() {
        let (_file, db_path) = seed_portfolio_fixture();
        let body = get_json(&db_path, "/api/portfolio/overview").await;

        let lists = body["custom_lists"].as_array().unwrap();
        let stop_losses = lists.iter().find(|l| l["key"] == "stop_losses").expect("configured list missing");
        let entries = &stop_losses["entries"];

        let man = find(entries, "MAN.AX");
        assert!(close_to(&man["field_value"], 9.0));
        assert_eq!(man["is_trailing"], false);
        // (12 − 9) / 9
        assert!(close_to(&man["pct_diff"], 33.333333333333336));

        let trl = find(entries, "TRL.AX");
        assert!(close_to(&trl["field_value"], 27.0), "trailing trigger appears as the list value");
        assert_eq!(trl["is_trailing"], true);

        // PART.AX has no stop loss of either kind
        assert!(entries.as_array().unwrap().iter().all(|e| e["symbol"] != "PART.AX"));
    }

    /// A list may rank on the day's volume instead of the price. Everything
    /// else — the operator, the ranking, the truncate — is unchanged, so the
    /// only thing under test is which number reaches the comparison.
    #[actix_web::test]
    async fn overview_list_can_compare_volume_against_a_stored_field() {
        let (_file, db_path) = seed_portfolio_fixture();
        let conn = open_db(&db_path).unwrap();
        for (sym, value) in [("MAN.AX", "100000"), ("TRL.AX", "200000")] {
            conn.execute(
                "INSERT INTO holdings_symbol_fields (symbol, field_key, value) VALUES (?1, 'min_volume', ?2)",
                params![sym, value],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO app_config (key, value) VALUES ('dashboard_custom_lists', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![r#"[{"key":"vol","label":"Heavy Volume","source":"holdings","field_key":"holdings:min_volume","operator":"above","compare":"volume"}]"#],
        )
        .unwrap();

        let body = get_json(&db_path, "/api/portfolio/overview").await;
        let list = body["custom_lists"].as_array().unwrap().iter().find(|l| l["key"] == "vol").expect("volume list missing");
        assert_eq!(list["compare"], "volume");

        let entries = &list["entries"];
        let man = find(entries, "MAN.AX");
        assert!(close_to(&man["compare_value"], 500_000.0), "volume drives the comparison");
        assert!(close_to(&man["price"], 12.0), "the price stays on the entry as context");
        // (500000 − 100000) / 100000
        assert!(close_to(&man["pct_diff"], 400.0));

        // 120k volume is below its 200k threshold, so the operator excludes it
        // even though its *price* is far above the same number would suggest.
        assert!(entries.as_array().unwrap().iter().all(|e| e["symbol"] != "TRL.AX"));
    }

    /// An `indicator:` field is derived from stored closes rather than typed by
    /// a user, so it exists for any symbol with enough history. The fixture's
    /// flat 150-day series makes every SMA equal to the close it repeats.
    #[actix_web::test]
    async fn overview_indicator_list_ranks_holdings_against_the_50_day_sma() {
        let (_file, db_path) = seed_portfolio_fixture();
        let conn = open_db(&db_path).unwrap();
        conn.execute(
            "INSERT INTO app_config (key, value) VALUES ('dashboard_custom_lists', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![r#"[{"key":"above_sma","label":"Above 50SMA","source":"holdings","field_key":"indicator:sma50","operator":"above"}]"#],
        )
        .unwrap();

        let body = get_json(&db_path, "/api/portfolio/overview").await;
        let list = body["custom_lists"].as_array().unwrap().iter().find(|l| l["key"] == "above_sma").expect("indicator list missing");
        assert_eq!(list["field_label"], "50-Day SMA", "the key resolves to a readable heading");

        let entries = &list["entries"];
        let man = find(entries, "MAN.AX");
        assert!(close_to(&man["field_value"], 10.0), "flat closes at 10.0");
        assert!(close_to(&man["pct_diff"], 20.0), "price 12 against a 10.0 average");
        assert_eq!(man["origin"], "holdings", "drives which screen the symbol links to");

        // PART.AX trades at 1.50 against a 3.0 average, so `above` excludes it.
        assert!(entries.as_array().unwrap().iter().all(|e| e["symbol"] != "PART.AX"));
        // TRL.AX has two closes — no 50-day average exists, so it cannot qualify.
        assert!(entries.as_array().unwrap().iter().all(|e| e["symbol"] != "TRL.AX"));
    }

    /// The three keys the Configuration screen offers, and the two ways a key
    /// yields nothing: an unknown name, and history too short for the window.
    #[test]
    fn indicator_value_resolves_the_three_offered_keys() {
        let (_file, db_path) = setup_test_db();
        let conn = open_db(&db_path).unwrap();
        let start = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        for i in 0..150 {
            let date = (start + chrono::Duration::days(i)).format("%Y-%m-%d").to_string();
            conn.execute(
                "INSERT INTO prices (symbol, date, close, fetched_at) VALUES ('TST.AX', ?1, 10.0, 'x')",
                params![date],
            )
            .unwrap();
        }

        assert_eq!(indicator_value(&conn, "TST.AX", "sma50"), Some(10.0));
        assert_eq!(indicator_value(&conn, "TST.AX", "sma150"), Some(10.0));
        // 150 calendar days is about 21 weeks — a 40-week average has no value
        // to report yet, and must say so rather than average what it has.
        assert_eq!(indicator_value(&conn, "TST.AX", "ema40w"), None);
        assert_eq!(indicator_value(&conn, "TST.AX", "sma999"), None, "an unknown key is not a period to guess at");
    }

    /// Seed one holding whose closes sit on `before` for 50 bars and `after`
    /// for 5, so a crossing of a 10.0 threshold lands exactly 5 bars back. The
    /// crossing bar carries triple volume, making its surge checkable.
    fn seed_crossing_fixture(before: f64, after: f64) -> (NamedTempFile, PathBuf) {
        let (file, db_path) = setup_test_db();
        let conn = open_db(&db_path).unwrap();
        conn.execute(
            "INSERT INTO holdings_transactions (id, symbol, transaction_type, date, quantity, price, brokerage, created_at)
             VALUES (1, 'TST.AX', 'purchase', '2025-12-01', 100.0, 9.0, 0.0, '2025-12-01T00:00:00Z')",
            [],
        )
        .unwrap();
        let start = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        for i in 0..55 {
            let date = (start + chrono::Duration::days(i)).format("%Y-%m-%d").to_string();
            let (close, volume) = if i < 50 {
                (before, 100_i64)
            } else if i == 50 {
                (after, 300)
            } else {
                (after, 100)
            };
            conn.execute(
                "INSERT INTO prices (symbol, date, close, volume, fetched_at) VALUES ('TST.AX', ?1, ?2, ?3, 'x')",
                params![date, close, volume],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO cached_current_prices (symbol, price, volume, last_updated, price_date)
             VALUES ('TST.AX', ?1, 100, '2026-03-01T00:00:00Z', '2026-02-24')",
            params![after],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO holdings_symbol_fields (symbol, field_key, value) VALUES ('TST.AX', 'target', '10.0')",
            [],
        )
        .unwrap();
        (file, db_path)
    }

    fn set_single_list(db_path: &PathBuf, json: &str) {
        let conn = open_db(db_path).unwrap();
        conn.execute(
            "INSERT INTO app_config (key, value) VALUES ('dashboard_custom_lists', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![json],
        )
        .unwrap();
    }

    /// "Days above" dates the crossing rather than measuring the gap, and
    /// reports the volume behind it — the pair that makes a breakout readable.
    #[actix_web::test]
    async fn overview_days_above_list_counts_from_the_crossing() {
        let (_file, db_path) = seed_crossing_fixture(8.0, 12.0);
        set_single_list(&db_path, r#"[{"key":"broke_out","label":"Broke Out","source":"holdings","field_key":"holdings:target","operator":"days_above"}]"#);

        let body = get_json(&db_path, "/api/portfolio/overview").await;
        let list = body["custom_lists"].as_array().unwrap().iter().find(|l| l["key"] == "broke_out").unwrap();
        assert_eq!(list["metric"], "days", "the ranked column, so the client knows which header sorts");

        let tst = find(&list["entries"], "TST.AX");
        assert_eq!(tst["days"], 5, "50 bars below then 5 above");
        // The crossing bar traded 300 against a 100 average over the prior 20.
        assert!(close_to(&tst["volume_cross_pct"], 200.0));
    }

    /// The mirror case: the same shape upside down must date the breakdown,
    /// not silently reuse the breakout walk.
    #[actix_web::test]
    async fn overview_days_below_list_dates_the_breakdown() {
        let (_file, db_path) = seed_crossing_fixture(12.0, 8.0);
        set_single_list(&db_path, r#"[{"key":"broke_down","label":"Broke Down","source":"holdings","field_key":"holdings:target","operator":"days_below"}]"#);

        let body = get_json(&db_path, "/api/portfolio/overview").await;
        let list = body["custom_lists"].as_array().unwrap().iter().find(|l| l["key"] == "broke_down").unwrap();
        let tst = find(&list["entries"], "TST.AX");
        assert_eq!(tst["days"], 5);
        assert!(close_to(&tst["volume_cross_pct"], 200.0));

        // A "days above" list over the same data must find nothing: the price
        // is below its target, so the operator excludes it before any counting.
        set_single_list(&db_path, r#"[{"key":"broke_out","label":"Broke Out","source":"holdings","field_key":"holdings:target","operator":"days_above"}]"#);
        let body = get_json(&db_path, "/api/portfolio/overview").await;
        let list = body["custom_lists"].as_array().unwrap().iter().find(|l| l["key"] == "broke_out").unwrap();
        assert!(list["entries"].as_array().unwrap().is_empty());
    }

    /// Ranking by the volume behind the crossing is a different order from
    /// ranking by its date, so the metric has to reach the sort.
    #[actix_web::test]
    async fn overview_volume_cross_list_reports_its_metric() {
        let (_file, db_path) = seed_crossing_fixture(8.0, 12.0);
        set_single_list(&db_path, r#"[{"key":"conviction","label":"Conviction","source":"holdings","field_key":"holdings:target","operator":"volume_cross_pct","sort":"desc"}]"#);

        let body = get_json(&db_path, "/api/portfolio/overview").await;
        let list = body["custom_lists"].as_array().unwrap().iter().find(|l| l["key"] == "conviction").unwrap();
        assert_eq!(list["metric"], "volume_cross_pct");
        assert_eq!(list["sort"], "desc");
        let tst = find(&list["entries"], "TST.AX");
        assert!(close_to(&tst["volume_cross_pct"], 200.0));
        assert_eq!(tst["days"], 5, "the date is still reported alongside it");
    }

    /// Every shape the Configuration screen can produce must survive the
    /// guard — a validator that rejects valid input is worse than none.
    #[test]
    fn config_validation_accepts_everything_the_editor_can_build() {
        let valid = r#"[
            {"key":"a","label":"Above","source":"holdings","field_key":"holdings:stop_loss","operator":"above","limit":15,"sort":"asc"},
            {"key":"b","label":"Volume","source":"watchlist","field_key":"watchlist:breakthrough_price","operator":"pct_below","compare":"volume"},
            {"key":"c","label":"SMA","source":"both","field_key":"indicator:sma50","operator":"days_above"},
            {"key":"d","label":"SMA150","source":"holdings","field_key":"indicator:sma150","operator":"days_below"},
            {"key":"e","label":"EMA","source":"holdings","field_key":"indicator:ema40w","operator":"volume_cross_pct","sort":"desc"},
            {"key":"f","label":"Nulls","source":"holdings","field_key":"holdings:x","operator":"below","compare":null,"sort":null,"limit":null}
        ]"#;
        assert_eq!(validate_dashboard_custom_lists(valid), Ok(()));
        assert_eq!(validate_dashboard_custom_lists("[]"), Ok(()), "no lists is a legitimate configuration");
    }

    /// Each rejection names the offending list and value: the message is the
    /// whole point, since the alternative was an empty dashboard and silence.
    #[test]
    fn config_validation_rejects_and_explains() {
        let base = |extra: &str| format!(r#"[{{"key":"a","label":"A","source":"holdings","field_key":"holdings:x","operator":"above"{extra}}}]"#);
        let cases: Vec<(String, &str)> = vec![
            ("{not json".to_string(), "not valid JSON"),
            (r#"{"key":"a"}"#.to_string(), "must be a JSON array"),
            ("[3]".to_string(), "must be an object"),
            (r#"[{"label":"A","source":"holdings","field_key":"holdings:x","operator":"above"}]"#.to_string(), "key is required"),
            (r#"[{"key":"a","source":"holdings","field_key":"holdings:x","operator":"above"}]"#.to_string(), "label is required"),
            (r#"[{"key":"a","label":"A","source":"nowhere","field_key":"holdings:x","operator":"above"}]"#.to_string(), "source 'nowhere' is not one of"),
            (r#"[{"key":"a","label":"A","source":"holdings","field_key":"holdings:x","operator":"sideways"}]"#.to_string(), "operator 'sideways' is not one of"),
            (r#"[{"key":"a","label":"A","source":"holdings","field_key":"stop_loss","operator":"above"}]"#.to_string(), "field_key must look like"),
            (r#"[{"key":"a","label":"A","source":"holdings","field_key":"sectors:x","operator":"above"}]"#.to_string(), "prefix 'sectors' is not"),
            (r#"[{"key":"a","label":"A","source":"holdings","field_key":"indicator:sma200","operator":"above"}]"#.to_string(), "indicator 'sma200' is not one of"),
            (base(r#","compare":"turnover""#), "compare 'turnover' is not one of"),
            (base(r#","sort":"up""#), "sort 'up' is not one of"),
            (base(r#","limit":0"#), "limit 0 is outside 1-100"),
            (base(r#","limit":500"#), "limit 500 is outside 1-100"),
            (base(r#","limit":"lots""#), "limit must be a whole number"),
        ];
        for (json, expected) in cases {
            let err = validate_dashboard_custom_lists(&json)
                .expect_err(&format!("should have been rejected: {json}"));
            assert!(err.contains(expected), "expected {expected:?} in {err:?}");
        }

        // A duplicate key silently shadows a list in the client, so it is caught here.
        let dup = r#"[{"key":"a","label":"A","source":"holdings","field_key":"holdings:x","operator":"above"},
                      {"key":"a","label":"B","source":"holdings","field_key":"holdings:y","operator":"below"}]"#;
        assert!(validate_dashboard_custom_lists(dup).unwrap_err().contains("duplicate key 'a'"));
    }


    /// A delisting date is compared against bar dates as text, so a value in
    /// any other shape sorts wrongly instead of failing loudly.
    #[test]
    fn a_dead_symbol_mark_must_carry_a_real_date() {
        assert_eq!(validate_config_value("dead_symbol_JLG.AX", "2025-08-01"), Ok(()));
        assert_eq!(validate_config_value("dead_symbol_JLG.AX", ""), Ok(()), "clearing the mark is allowed");
        assert!(validate_config_value("dead_symbol_JLG.AX", "01/08/2025").is_err());
        assert!(validate_config_value("dead_symbol_JLG.AX", "delisted").is_err());
    }

    #[test]
    fn dead_symbols_reads_the_marks_and_ignores_cleared_ones() {
        let (_file, db_path) = setup_test_db();
        let conn = open_db(&db_path).unwrap();
        conn.execute_batch(
            "INSERT INTO app_config (key, value) VALUES
                ('dead_symbol_JLG.AX', '2025-08-01'),
                ('dead_symbol_CCLD.AX', '   '),
                ('manual_price_JLG.AX', '3.91'),
                ('portfolio_history_start', '2025-01-01')",
        )
        .unwrap();

        let dead = dead_symbols(&conn);
        assert_eq!(dead.get("JLG.AX"), Some(&"2025-08-01".to_string()));
        // The UI clears a mark by writing an empty value rather than deleting
        // the row, so a blank must not read back as a dead symbol.
        assert!(!dead.contains_key("CCLD.AX"), "a cleared mark is not a dead symbol");
        // The prefix match must not swallow neighbouring keys.
        assert_eq!(dead.len(), 1);
    }

    /// The point of the mark: a delisted symbol is never sent to Yahoo again.
    /// Returning early with no request is what stops the 404-per-refresh that
    /// buries real failures in the event log.
    #[actix_web::test]
    async fn a_refresh_of_only_dead_symbols_asks_yahoo_nothing() {
        let (_file, db_path) = setup_test_db();
        open_db(&db_path)
            .unwrap()
            .execute_batch("INSERT INTO app_config (key, value) VALUES ('dead_symbol_JLG.AX', '2025-08-01')")
            .unwrap();

        let prices = fetch_and_cache_current_prices(
            &db_path,
            &["JLG.AX".to_string()],
            "sold_prices_updated_at",
        )
        .await
        .unwrap();

        assert!(prices.is_empty(), "a dead symbol yields no quote and no request");
    }

    /// Marking a symbol dead stops the refresh overwriting its cached quote, so
    /// whatever is cached at that moment would otherwise be served as the
    /// current price forever.
    #[actix_web::test]
    async fn marking_a_symbol_dead_discards_its_stale_quote() {
        let (_file, db_path) = setup_test_db();
        open_db(&db_path)
            .unwrap()
            .execute_batch(
                "INSERT INTO cached_current_prices (symbol, price, last_updated, price_date)
                 VALUES ('JLG.AX', 3.91, '2025-08-01T00:00:00Z', '2025-08-01')",
            )
            .unwrap();

        let app = actix_web::test::init_service(
            App::new().app_data(web::Data::new(db_path.clone())).service(update_config),
        )
        .await;
        let req = actix_web::test::TestRequest::put()
            .uri("/api/config")
            .set_json(&serde_json::json!({ "key": "dead_symbol_JLG.AX", "value": "2025-08-01" }))
            .to_request();
        assert!(actix_web::test::call_service(&app, req).await.status().is_success());

        let cached: i64 = open_db(&db_path)
            .unwrap()
            .query_row("SELECT COUNT(*) FROM cached_current_prices WHERE symbol='JLG.AX'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(cached, 0, "the stale quote must not survive the mark");
    }

    /// Clearing the mark is how a symbol comes back — a wrong entry, or a
    /// ticker that resumed trading. It must not also wipe the cache, which is
    /// what the next refresh is about to refill.
    #[actix_web::test]
    async fn clearing_the_mark_leaves_the_cache_alone() {
        let (_file, db_path) = setup_test_db();
        open_db(&db_path)
            .unwrap()
            .execute_batch(
                "INSERT INTO cached_current_prices (symbol, price, last_updated, price_date)
                 VALUES ('BACK.AX', 1.20, '2025-08-01T00:00:00Z', '2025-08-01')",
            )
            .unwrap();

        let app = actix_web::test::init_service(
            App::new().app_data(web::Data::new(db_path.clone())).service(update_config),
        )
        .await;
        let req = actix_web::test::TestRequest::put()
            .uri("/api/config")
            .set_json(&serde_json::json!({ "key": "dead_symbol_BACK.AX", "value": "" }))
            .to_request();
        assert!(actix_web::test::call_service(&app, req).await.status().is_success());

        let cached: i64 = open_db(&db_path)
            .unwrap()
            .query_row("SELECT COUNT(*) FROM cached_current_prices WHERE symbol='BACK.AX'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(cached, 1);
    }
    #[test]
    fn config_validation_leaves_unrelated_keys_alone() {
        // Most config values are plain scalars and must not be parsed as JSON.
        assert_eq!(validate_config_value("manual_price_TST.AX", "12.50"), Ok(()));
        assert_eq!(validate_config_value("sectors", "not json at all"), Ok(()));
        // Field definitions share the failure mode, so they share the guard.
        assert!(validate_config_value("holdings_custom_fields", "{}").is_err());
        assert_eq!(validate_config_value("holdings_custom_fields", r#"[{"key":"k","label":"L"}]"#), Ok(()));
        assert!(validate_config_value("watchlist_custom_fields", r#"[{"key":"k"}]"#).unwrap_err().contains("label is required"));
    }

    /// The endpoint must refuse the write, not accept it and fail later.
    #[actix_web::test]
    async fn update_config_rejects_a_malformed_dashboard_list() {
        let (_file, db_path) = seed_portfolio_fixture();
        let before: String = open_db(&db_path)
            .unwrap()
            .query_row("SELECT value FROM app_config WHERE key = 'dashboard_custom_lists'", [], |r| r.get(0))
            .unwrap();

        let app = actix_web::test::init_service(
            App::new().app_data(web::Data::new(db_path.clone())).service(update_config),
        )
        .await;
        let body = serde_json::json!({
            "key": "dashboard_custom_lists",
            "value": r#"[{"key":"a","label":"A","source":"holdings","field_key":"indicator:sma42","operator":"above"}]"#,
        });
        let req = actix_web::test::TestRequest::put().uri("/api/config").set_json(&body).to_request();
        let resp = actix_web::test::call_service(&app, req).await;
        assert_eq!(resp.status(), actix_web::http::StatusCode::BAD_REQUEST);
        let json: serde_json::Value = actix_web::test::read_body_json(resp).await;
        assert!(json["error"]["message"].as_str().unwrap().contains("sma42"), "the message names the bad value: {json}");

        let after: String = open_db(&db_path)
            .unwrap()
            .query_row("SELECT value FROM app_config WHERE key = 'dashboard_custom_lists'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(before, after, "a rejected write must not touch the stored config");
    }

    /// A value written before the guard existed still has to be survivable —
    /// but it must leave a trail rather than an unexplained empty dashboard.
    #[actix_web::test]
    async fn a_malformed_stored_config_is_logged_not_swallowed() {
        let (_file, db_path) = seed_portfolio_fixture();
        open_db(&db_path)
            .unwrap()
            .execute(
                "UPDATE app_config SET value = '[{\"key\": truncated' WHERE key = 'dashboard_custom_lists'",
                [],
            )
            .unwrap();

        let body = get_json(&db_path, "/api/portfolio/overview").await;
        assert!(body["custom_lists"].as_array().unwrap().is_empty(), "the rest of the dashboard still renders");

        let logged: i64 = open_db(&db_path)
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM event_log WHERE level = 'error' AND event_type = 'config_parse' AND symbol = 'dashboard_custom_lists'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(logged, 1, "the parse failure is recorded against the key that caused it");
    }

    /// One long-held position: bought cheaply in 2024, worth far more now, with
    /// dividends either side of a 2025 baseline.
    fn seed_legacy_holding() -> (NamedTempFile, PathBuf) {
        let (file, db_path) = setup_test_db();
        let conn = open_db(&db_path).unwrap();
        conn.execute(
            "INSERT INTO holdings_transactions (id, symbol, transaction_type, date, quantity, price, brokerage, created_at)
             VALUES (1, 'TST.AX', 'purchase', '2024-01-10', 100.0, 5.0, 0.0, '2024-01-10T00:00:00Z')",
            [],
        )
        .unwrap();
        for (id, date, amount) in [(2, "2024-06-01", 50.0), (3, "2025-06-01", 30.0)] {
            conn.execute(
                "INSERT INTO holdings_transactions (id, symbol, transaction_type, date, quantity, amount, created_at)
                 VALUES (?1, 'TST.AX', 'dividend', ?2, 100.0, ?3, '2024-01-01T00:00:00Z')",
                params![id, date, amount],
            )
            .unwrap();
        }
        // 1 January is never a trading day, so the baseline has to resolve
        // forward to the 2nd.
        for (date, close) in [("2025-01-02", 20.0), ("2026-06-01", 25.0)] {
            conn.execute(
                "INSERT INTO prices (symbol, date, close, fetched_at) VALUES ('TST.AX', ?1, ?2, 'x')",
                params![date, close],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO cached_current_prices (symbol, price, last_updated, price_date)
             VALUES ('TST.AX', 25.0, '2026-06-02T00:00:00Z', '2026-06-01')",
            [],
        )
        .unwrap();
        (file, db_path)
    }

    async fn put_basis_date(db_path: &PathBuf, date: &str) -> actix_web::http::StatusCode {
        let app = actix_web::test::init_service(
            App::new().app_data(web::Data::new(db_path.clone())).service(update_holdings_symbol_fields),
        )
        .await;
        let body = serde_json::json!({ "custom_fields": { PL_BASIS_DATE: date } });
        let req = actix_web::test::TestRequest::put()
            .uri("/api/holdings/symbol-fields/TST.AX")
            .set_json(&body)
            .to_request();
        actix_web::test::call_service(&app, req).await.status()
    }

    fn stored_field(db_path: &PathBuf, key: &str) -> Option<String> {
        open_db(db_path)
            .unwrap()
            .query_row(
                "SELECT value FROM holdings_symbol_fields WHERE symbol = 'TST.AX' AND field_key = ?1",
                params![key],
                |r| r.get(0),
            )
            .ok()
    }

    /// The baseline price is resolved once on save. Deriving it on every read
    /// would let the figure move whenever stored history is trimmed, and a
    /// basis that moves is not a record of anything.
    #[actix_web::test]
    async fn saving_a_basis_date_resolves_and_stores_its_price() {
        let (_file, db_path) = seed_legacy_holding();
        assert!(put_basis_date(&db_path, "2025-01-01").await.is_success());
        assert_eq!(stored_field(&db_path, PL_BASIS_DATE).as_deref(), Some("2025-01-01"));
        assert_eq!(
            stored_field(&db_path, PL_BASIS_PRICE),
            Some("20".to_string()),
            "1 January is not a trading day; the close resolves forward to the 2nd"
        );
    }

    #[actix_web::test]
    async fn a_basis_date_with_no_stored_price_is_refused() {
        let (_file, db_path) = seed_legacy_holding();
        let status = put_basis_date(&db_path, "2030-01-01").await;
        assert_eq!(status, actix_web::http::StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(stored_field(&db_path, PL_BASIS_PRICE), None, "nothing is stored for a date we cannot price");
    }

    #[actix_web::test]
    async fn clearing_the_basis_date_clears_its_price() {
        let (_file, db_path) = seed_legacy_holding();
        assert!(put_basis_date(&db_path, "2025-01-01").await.is_success());
        assert!(put_basis_date(&db_path, "").await.is_success());
        assert_eq!(stored_field(&db_path, PL_BASIS_DATE), None);
        assert_eq!(stored_field(&db_path, PL_BASIS_PRICE), None, "a stale price outliving its date would be misread");
    }

    /// A baseline *replaces* the purchase as the cost basis. The point is not
    /// to show two numbers — it is that for a holding bought long ago the
    /// original price is a record, not a useful denominator.
    #[actix_web::test]
    async fn a_baseline_replaces_the_purchase_as_the_cost_basis() {
        let (_file, db_path) = seed_legacy_holding();
        assert!(put_basis_date(&db_path, "2025-01-01").await.is_success());

        let body = get_json(&db_path, "/api/portfolio/holdings").await;
        let tst = find(&body["holdings"], "TST.AX");

        // Bought at 5.00, but the baseline close was 20.00, so that is the cost.
        assert!(close_to(&tst["invested"], 2000.0), "100 shares at the 20.00 baseline, not the 5.00 purchase");
        assert!(close_to(&tst["avg_cost"], 20.0));
        assert!(close_to(&tst["native_avg_cost"], 20.0));
        // Only income earned since the baseline counts toward the return.
        assert!(close_to(&tst["dividends"], 30.0), "the 2024 payment belongs to the period we stopped caring about");
        // 100 × 25 − 2000 + 30
        assert!(close_to(&tst["pl"], 530.0));
        assert!(close_to(&tst["pl_pct"], 26.5));

        // Reported so the client can say which period the figures cover.
        assert_eq!(tst["basis_date"], "2025-01-01");
        assert!(close_to(&tst["basis_price"], 20.0));

        // The lifetime figures are gone from the payload — one holding, one
        // set of numbers. The purchase itself is untouched in the ledger.
        assert!(tst.get("pl_since").is_none());
        let txs = open_db(&db_path)
            .unwrap()
            .query_row("SELECT price FROM holdings_transactions WHERE id = 1", [], |r| r.get::<_, f64>(0))
            .unwrap();
        assert!((txs - 5.0).abs() < 1e-9, "the purchase price stays on record");
    }

    /// The Dashboard total must be the sum of what the Holdings screen shows.
    /// Two code paths computing the same position separately is exactly how
    /// they come to disagree, so both go through one basis helper.
    #[actix_web::test]
    async fn the_overview_total_follows_the_same_baseline() {
        let (_file, db_path) = seed_legacy_holding();

        let before = get_json(&db_path, "/api/portfolio/overview").await;
        assert!(close_to(&before["totals"]["holdings_pl"], 2080.0), "lifetime while no baseline is set");

        assert!(put_basis_date(&db_path, "2025-01-01").await.is_success());
        let after = get_json(&db_path, "/api/portfolio/overview").await;
        assert!(
            close_to(&after["totals"]["holdings_pl"], 530.0),
            "the total re-bases with the holding, not after it"
        );

        // And it equals what the Holdings screen reports for that position.
        let holdings = get_json(&db_path, "/api/portfolio/holdings").await;
        assert!(close_to(&find(&holdings["holdings"], "TST.AX")["pl"], 530.0));
    }

    /// Without a baseline nothing changes: the purchase is still the basis,
    /// and every dividend still counts.
    #[actix_web::test]
    async fn a_holding_with_no_basis_measures_from_its_purchase() {
        let (_file, db_path) = seed_legacy_holding();
        let body = get_json(&db_path, "/api/portfolio/holdings").await;
        let tst = find(&body["holdings"], "TST.AX");
        assert!(tst["basis_date"].is_null());
        assert!(close_to(&tst["invested"], 500.0));
        assert!(close_to(&tst["avg_cost"], 5.0));
        assert!(close_to(&tst["dividends"], 80.0), "both payments count");
        // 100 × 25 − 500 + 80
        assert!(close_to(&tst["pl"], 2080.0));
    }

    async fn history_json(db_path: &std::path::Path, uri: &str) -> serde_json::Value {
        let app = actix_web::test::init_service(
            App::new().app_data(web::Data::new(db_path.to_path_buf())).service(get_portfolio_history),
        )
        .await;
        let req = actix_web::test::TestRequest::get().uri(uri).to_request();
        let resp = actix_web::test::call_service(&app, req).await;
        assert!(resp.status().is_success(), "{} returned {}", uri, resp.status());
        actix_web::test::read_body_json(resp).await
    }

    fn set_history_floor(db_path: &PathBuf, value: &str) {
        open_db(db_path)
            .unwrap()
            .execute(
                "INSERT INTO app_config (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![PORTFOLIO_HISTORY_START, value],
            )
            .unwrap();
    }

    /// Without a floor the series still reaches back to the first transaction —
    /// the setting is opt-in, and an empty value must change nothing.
    #[actix_web::test]
    async fn history_without_a_floor_starts_at_the_first_transaction() {
        let (_file, db_path) = seed_portfolio_fixture();
        for value in ["", "   "] {
            set_history_floor(&db_path, value);
            let body = history_json(&db_path, "/api/portfolio/history").await;
            assert_eq!(body["summary"]["start_date"], "2026-01-05", "earliest holding transaction");
            assert!(body["start_floor"].is_null());
        }
    }

    #[actix_web::test]
    async fn a_floor_truncates_the_unbounded_range() {
        let (_file, db_path) = seed_portfolio_fixture();
        set_history_floor(&db_path, "2026-03-01");

        let body = history_json(&db_path, "/api/portfolio/history").await;
        assert_eq!(body["summary"]["start_date"], "2026-03-01");
        assert_eq!(body["start_floor"], "2026-03-01", "published so the client can drop swallowed ranges");
    }

    /// The floor is not only about "All". A five-year button reaches just as far
    /// back into the unpriced years as an unbounded request does.
    #[actix_web::test]
    async fn a_floor_raises_an_earlier_requested_range() {
        let (_file, db_path) = seed_portfolio_fixture();
        set_history_floor(&db_path, "2026-03-01");
        let body = history_json(&db_path, "/api/portfolio/history?from=2020-01-01").await;
        assert_eq!(body["summary"]["start_date"], "2026-03-01", "the request is raised to the floor");
    }

    /// A window that already starts after the floor is left alone — clamping
    /// must not widen a range the user deliberately narrowed.
    #[actix_web::test]
    async fn a_floor_leaves_a_later_range_untouched() {
        let (_file, db_path) = seed_portfolio_fixture();
        set_history_floor(&db_path, "2026-01-01");
        let body = history_json(&db_path, "/api/portfolio/history?from=2026-05-01").await;
        assert_eq!(body["summary"]["start_date"], "2026-05-01");
    }

    /// Re-basing the opening value is the point, not a side effect: a return
    /// measured across years the holdings could not be priced is meaningless.
    #[actix_web::test]
    async fn a_floor_rebases_the_opening_value_and_return() {
        let (_file, db_path) = seed_portfolio_fixture();
        let wide = history_json(&db_path, "/api/portfolio/history").await;
        set_history_floor(&db_path, "2026-03-01");
        let narrow = history_json(&db_path, "/api/portfolio/history").await;

        assert_ne!(
            wide["summary"]["opening_value"], narrow["summary"]["opening_value"],
            "the anchor moves with the window"
        );
        assert_ne!(wide["summary"]["twr_pct"], narrow["summary"]["twr_pct"]);
    }

    #[test]
    fn history_floor_must_be_a_date_or_empty() {
        assert_eq!(validate_config_value(PORTFOLIO_HISTORY_START, "2025-01-01"), Ok(()));
        assert_eq!(validate_config_value(PORTFOLIO_HISTORY_START, ""), Ok(()), "clearing the floor is allowed");
        // Stored and compared as text, so a non-ISO date would order wrongly
        // rather than fail — it has to be refused on the way in.
        assert!(validate_config_value(PORTFOLIO_HISTORY_START, "01/01/2025").is_err());
        assert!(validate_config_value(PORTFOLIO_HISTORY_START, "last year").is_err());
    }

    /// The chart's last point and the Stock Value beside it are the same
    /// portfolio at the same moment, so they have to be the same number.
    ///
    /// They were not. A bar exists for today as soon as the fetcher runs, and
    /// the history used it while the holdings endpoint used the live quote —
    /// separately for the price and for the FX rate, so every foreign holding
    /// landed a fraction out.
    #[actix_web::test]
    async fn the_charts_last_point_matches_the_holdings_total() {
        let (_file, db_path) = setup_test_db();
        let today = Utc::now().format("%Y-%m-%d").to_string();
        let conn = open_db(&db_path).unwrap();
        conn.execute(
            "INSERT INTO holdings_transactions (id, symbol, transaction_type, date, quantity, price, brokerage, created_at)
             VALUES (1, 'USX', 'purchase', '2026-01-05', 10.0, 150.0, 0.0, '2026-01-05T00:00:00Z')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO symbol_info (symbol, currency, updated_at) VALUES ('USX', 'USD', 'x')",
            [],
        )
        .unwrap();

        // A bar for today exists for both the stock and the rate — and both are
        // stale relative to the quotes the holdings endpoint reads.
        for (sym, close) in [("USX", 100.0), ("USDAUD=X", 1.5)] {
            conn.execute(
                "INSERT INTO prices (symbol, date, close, fetched_at) VALUES (?1, ?2, ?3, 'x')",
                params![sym, today, close],
            )
            .unwrap();
        }
        for (sym, price) in [("USX", 110.0), ("USDAUD=X", 1.6)] {
            conn.execute(
                "INSERT INTO cached_current_prices (symbol, price, last_updated, price_date)
                 VALUES (?1, ?2, ?3, ?4)",
                params![sym, price, format!("{}T23:00:00Z", today), today],
            )
            .unwrap();
        }
        drop(conn);

        let holdings = get_json(&db_path, "/api/portfolio/holdings").await;
        let total: f64 = holdings["holdings"].as_array().unwrap()
            .iter().map(|h| h["current_value"].as_f64().unwrap()).sum();

        let history = history_json(&db_path, "/api/portfolio/history").await;
        let last = history["series"].as_array().unwrap().last().unwrap();

        // 10 × 110 × 1.6, not 10 × 100 × 1.5.
        assert!((total - 1760.0).abs() < 0.01, "holdings should price at the quote, got {total}");
        assert!(
            (last["stocks"].as_f64().unwrap() - total).abs() < 0.01,
            "chart ends at {}, holdings say {total}",
            last["stocks"]
        );
    }

    /// A weekly indicator crossed in days must use the last *completed* week.
    /// Feeding a bar the average that already contains its own close would let
    /// the day count shift retroactively as the week finished.
    #[test]
    fn weekly_ema_series_uses_the_last_completed_week() {
        let bar = |date: &str, close: f64| PriceHistoryPoint {
            date: date.to_string(),
            open: None,
            high: None,
            low: None,
            close: Some(close),
            volume: None,
        };
        let history = vec![
            bar("2026-01-05", 100.0), // week 1 (Mon)
            bar("2026-01-09", 105.0), // week 1 close
            bar("2026-01-12", 200.0), // week 2
            bar("2026-01-16", 205.0), // week 2 close
            bar("2026-01-19", 300.0), // week 3
        ];
        let series = weekly_ema_series(&history, 2).expect("three weeks is enough for period 2");
        assert_eq!(series.len(), history.len(), "one value per daily bar");
        // Weeks 1 and 2 have no completed 2-week average behind them yet.
        assert_eq!(series[0], None);
        assert_eq!(series[1], None);
        assert_eq!(series[2], None);
        assert_eq!(series[3], None);
        // Week 3's bar sees week 2's seed — the mean of 105 and 205 — and not
        // the value that includes its own 300.
        assert_eq!(series[4], Some(155.0));

        // Two weeks cannot support a 40-week average.
        assert_eq!(weekly_ema_series(&history, 40), None);
    }

    /// The default is unchanged: a list with no `compare` still ranks on price.
    #[actix_web::test]
    async fn overview_list_defaults_to_comparing_price() {
        let (_file, db_path) = seed_portfolio_fixture();
        let body = get_json(&db_path, "/api/portfolio/overview").await;
        let list = stop_loss_list(&body);
        assert_eq!(list["compare"], "price");
        let man = find(&list["entries"], "MAN.AX");
        assert!(close_to(&man["compare_value"], 12.0));
    }

    #[actix_web::test]
    async fn risk_endpoint_reports_margins_and_dollar_impact() {
        let (_file, db_path) = seed_portfolio_fixture();
        let body = get_json(&db_path, "/api/portfolio/risk").await;
        let rows = &body["rows"];
        assert_eq!(rows.as_array().unwrap().len(), 3);

        let man = find(rows, "MAN.AX");
        assert!(close_to(&man["purchase_price"], 10.0));
        assert!(close_to(&man["current_price"], 12.0));
        assert!(close_to(&man["pl_pct"], 20.0));
        assert!(close_to(&man["stop_loss"], 9.0));
        assert!(close_to(&man["stop_loss_pct"], -10.0), "stop 9 vs purchase 10");
        assert!(close_to(&man["stop_loss_dollar"], -100.0), "(9 − 10) × 100 shares");
        assert_eq!(man["is_trailing_sell"], false);

        let trl = find(rows, "TRL.AX");
        assert!(close_to(&trl["stop_loss"], 27.0));
        assert!(close_to(&trl["stop_loss_pct"], 35.0), "stop 27 vs purchase 20");
        assert!(close_to(&trl["stop_loss_dollar"], 350.0), "(27 − 20) × 50 shares");
        assert_eq!(trl["is_trailing_sell"], true);

        let totals = &body["totals"];
        assert!(close_to(&totals["total_invested"], 2060.0));
        assert!(close_to(&totals["total_sl_dollar"], 250.0), "−100 + 350 + 0");
        assert!(close_to(&totals["total_sl_pct"], 250.0 / 2060.0 * 100.0));
    }

    /// "30d High" must be the highest price *reached*, not the highest close.
    /// RMS.AX touched 3.79 on a day it closed at 3.67 — reporting 3.70 put a
    /// real purchase at 3.76 above the stock's own 30-day high.
    #[actix_web::test]
    async fn high30d_uses_intraday_highs() {
        let (_file, db_path) = seed_portfolio_fixture();
        {
            let conn = open_db(&db_path).unwrap();
            // A bar that spiked well above every close in the window
            conn.execute(
                "INSERT INTO prices (symbol, date, high, close, fetched_at)
                 VALUES ('MAN.AX', '2026-07-02', 15.5, 11.0, '2026-07-02T00:00:00Z')",
                [],
            )
            .unwrap();
        }

        let body = get_json(&db_path, "/api/portfolio/risk").await;
        let man = find(&body["rows"], "MAN.AX");
        assert!(
            close_to(&man["high30d"], 15.5),
            "expected the intraday high 15.5, got {:?}",
            man["high30d"]
        );
    }

    // -------------------------------------------------------------------------
    // Portfolio value over time
    // -------------------------------------------------------------------------

    fn seed_close(conn: &Connection, symbol: &str, date: &str, close: f64) {
        conn.execute(
            "INSERT OR REPLACE INTO prices (symbol, date, close, fetched_at) VALUES (?1, ?2, ?3, 'x')",
            params![symbol, date, close],
        )
        .unwrap();
    }

    fn history_on(series: &[portfolio::DailyValue], date: &str) -> portfolio::DailyValue {
        series.iter().find(|p| p.date == date).unwrap_or_else(|| panic!("no point for {date}")).clone()
    }

    /// Shares are valued at the day's close and carried across non-trading days.
    #[test]
    fn history_values_holdings_at_each_day_close() {
        let (_file, db_path) = setup_test_db();
        let conn = open_db(&db_path).unwrap();
        seed_cash_account(&db_path, 1, "Settlement AUD", "AUD");
        conn.execute(
            "INSERT INTO cash_transactions (account_id, date, amount, kind, created_at)
             VALUES (1, '2026-03-02', 1000.0, 'deposit', 'x')",
            [],
        )
        .unwrap();

        let mut buy = trade_payload("BHP.AX", "purchase", 10.0, 40.0);
        buy.date = "2026-03-02".to_string();
        buy.cash_account_id = Some(1);
        insert_holding_transaction(&db_path, "BHP.AX", buy).unwrap();

        seed_close(&conn, "BHP.AX", "2026-03-02", 40.0);
        seed_close(&conn, "BHP.AX", "2026-03-03", 44.0);
        // No bar on the 4th — the last close carries forward

        let series = build_portfolio_history(&conn, None, Some("2026-03-04")).unwrap();
        let d2 = history_on(&series, "2026-03-02");
        assert!((d2.stocks - 400.0).abs() < 1e-9, "stocks {}", d2.stocks);
        assert!((d2.cash - 600.0).abs() < 1e-9, "cash {}", d2.cash); // 1000 deposited − 400 spent
        assert!((d2.flow - 1000.0).abs() < 1e-9, "only the deposit is a flow");

        let d3 = history_on(&series, "2026-03-03");
        assert!((d3.stocks - 440.0).abs() < 1e-9);
        assert!(d3.flow.abs() < 1e-9, "a price rise is not a flow");
        assert!((history_on(&series, "2026-03-04").stocks - 440.0).abs() < 1e-9, "close carries forward");
    }

    /// A trade with no cash leg is externally funded — every transaction
    /// recorded before the ledger existed is in that state, and without this
    /// the shares would look like value appearing from nowhere.
    #[test]
    fn trades_without_a_cash_leg_count_as_external_funding() {
        let (_file, db_path) = setup_test_db();
        let conn = open_db(&db_path).unwrap();

        let mut buy = trade_payload("BHP.AX", "purchase", 10.0, 40.0);
        buy.date = "2026-03-02".to_string();
        buy.brokerage = Some(9.5);
        insert_holding_transaction(&db_path, "BHP.AX", buy).unwrap();
        seed_close(&conn, "BHP.AX", "2026-03-02", 40.0);
        seed_close(&conn, "BHP.AX", "2026-03-03", 44.0);

        let series = build_portfolio_history(&conn, None, Some("2026-03-03")).unwrap();
        let funded = history_on(&series, "2026-03-02");
        assert!((funded.flow - 409.5).abs() < 1e-9, "cost plus brokerage entered the portfolio");
        assert!((funded.total() - 400.0).abs() < 1e-9);

        // The purchase must not register as a gain, only the later price rise
        let twr = portfolio::time_weighted_return(&series).unwrap();
        assert!((twr - 0.10).abs() < 1e-9, "expected +10%, got {}", twr * 100.0);
    }

    /// Foreign holdings and foreign cash are both converted at the day's rate.
    #[test]
    fn history_converts_foreign_holdings_and_cash_to_aud() {
        let (_file, db_path) = setup_test_db();
        let conn = open_db(&db_path).unwrap();
        conn.execute(
            "INSERT INTO symbol_info (symbol, currency, updated_at) VALUES ('AAPL', 'USD', 'x')",
            [],
        )
        .unwrap();
        seed_cash_account(&db_path, 1, "Settlement USD", "USD");
        conn.execute(
            "INSERT INTO cash_transactions (account_id, date, amount, kind, created_at)
             VALUES (1, '2026-03-02', 500.0, 'deposit', 'x')",
            [],
        )
        .unwrap();

        let mut buy = trade_payload("AAPL", "purchase", 10.0, 150.0);
        buy.date = "2026-03-02".to_string();
        buy.currency = Some("USD".to_string());
        buy.original_price = Some(100.0);
        buy.fx_rate = Some(1.5);
        buy.cash_account_id = Some(1);
        insert_holding_transaction(&db_path, "AAPL", buy).unwrap();

        seed_close(&conn, "AAPL", "2026-03-02", 100.0); // USD close
        seed_close(&conn, "USDAUD=X", "2026-03-02", 1.5);

        let day = history_on(&build_portfolio_history(&conn, None, Some("2026-03-02")).unwrap(), "2026-03-02");
        // 10 shares × US$100 × 1.5
        assert!((day.stocks - 1500.0).abs() < 1e-9, "stocks {}", day.stocks);
        // US$500 deposited − US$1,000 spent = −US$500, at 1.5
        assert!((day.cash - -750.0).abs() < 1e-9, "cash {}", day.cash);
        assert!((day.flow - 750.0).abs() < 1e-9, "the US$500 deposit in AUD");
    }

    /// An account marked out of the portfolio contributes neither value nor flow.
    #[test]
    fn history_excludes_accounts_not_in_the_portfolio() {
        let (_file, db_path) = setup_test_db();
        let conn = open_db(&db_path).unwrap();
        seed_cash_account(&db_path, 1, "Invested AUD", "AUD");
        conn.execute(
            "INSERT INTO cash_accounts (id, name, currency, include_in_portfolio, created_at)
             VALUES (2, 'Everyday AUD', 'AUD', 0, 'x')",
            [],
        )
        .unwrap();
        conn.execute_batch(
            "INSERT INTO cash_transactions (account_id, date, amount, kind, created_at) VALUES (1, '2026-03-02', 1000.0, 'deposit', 'x');
             INSERT INTO cash_transactions (account_id, date, amount, kind, created_at) VALUES (2, '2026-03-02', 9999.0, 'deposit', 'x');",
        )
        .unwrap();

        let day = history_on(&build_portfolio_history(&conn, None, Some("2026-03-02")).unwrap(), "2026-03-02");
        assert!((day.cash - 1000.0).abs() < 1e-9, "excluded account leaked in: {}", day.cash);
        assert!((day.flow - 1000.0).abs() < 1e-9);
    }

    /// Transactions before `from` set the opening position rather than being
    /// dropped, so a windowed request still values what is actually held.
    #[test]
    fn history_window_carries_the_opening_position() {
        let (_file, db_path) = setup_test_db();
        let conn = open_db(&db_path).unwrap();
        let mut buy = trade_payload("BHP.AX", "purchase", 10.0, 40.0);
        buy.date = "2026-03-01".to_string();
        insert_holding_transaction(&db_path, "BHP.AX", buy).unwrap();
        seed_close(&conn, "BHP.AX", "2026-03-01", 40.0);
        seed_close(&conn, "BHP.AX", "2026-03-05", 50.0);

        // The anchor (4 Mar) plus the requested day (5 Mar)
        let series = build_portfolio_history(&conn, Some("2026-03-05"), Some("2026-03-05")).unwrap();
        assert_eq!(series.len(), 2);
        assert_eq!(series[0].date, "2026-03-04", "element 0 is the anchor day");
        assert!((series[1].stocks - 500.0).abs() < 1e-9, "shares bought before the window still count");
        assert!(series[1].flow.abs() < 1e-9, "an earlier purchase is not a flow inside this window");
    }

    /// The books must balance: opening + contributions + gain = end value.
    /// Before the anchor existed the first day's purchase was counted twice —
    /// once in the opening value and again as a contribution.
    #[test]
    fn history_accounting_reconciles_from_an_empty_start() {
        let (_file, db_path) = setup_test_db();
        let conn = open_db(&db_path).unwrap();
        let mut buy = trade_payload("BHP.AX", "purchase", 10.0, 40.0);
        buy.date = "2026-03-02".to_string();
        insert_holding_transaction(&db_path, "BHP.AX", buy).unwrap();
        seed_close(&conn, "BHP.AX", "2026-03-02", 40.0);
        seed_close(&conn, "BHP.AX", "2026-03-03", 44.0);

        let series = build_portfolio_history(&conn, None, Some("2026-03-03")).unwrap();
        let opening = series[0].total();
        let window = &series[1..];
        let contributions = portfolio::net_contributions(window);
        let end = window.last().unwrap().total();
        let gain = end - opening - contributions;

        assert!(opening.abs() < 1e-9, "nothing was held before the first trade");
        assert!((contributions - 400.0).abs() < 1e-9);
        assert!((gain - 40.0).abs() < 1e-9, "the 10% rise on $400");
        assert!((opening + contributions + gain - end).abs() < 1e-9, "books must balance");
    }

    /// A delisted holding keeps its last close forever otherwise, quietly
    /// misstating the portfolio from the day it stopped trading. The manual
    /// price only applies once real bars run out — history stays real.
    #[test]
    fn history_uses_a_manual_price_once_the_bars_stop() {
        let (_file, db_path) = setup_test_db();
        let conn = open_db(&db_path).unwrap();
        let mut buy = trade_payload("DEAD.AX", "purchase", 10.0, 100.0);
        buy.date = "2026-03-01".to_string();
        insert_holding_transaction(&db_path, "DEAD.AX", buy).unwrap();
        seed_close(&conn, "DEAD.AX", "2026-03-01", 100.0);
        seed_close(&conn, "DEAD.AX", "2026-03-02", 90.0); // last ever bar
        conn.execute(
            "INSERT INTO app_config (key, value) VALUES ('manual_price_DEAD.AX', '120.5')",
            [],
        )
        .unwrap();

        let series = build_portfolio_history(&conn, None, Some("2026-03-04")).unwrap();
        // Real bars are untouched
        assert!((history_on(&series, "2026-03-01").stocks - 1000.0).abs() < 1e-9);
        assert!((history_on(&series, "2026-03-02").stocks - 900.0).abs() < 1e-9);
        // Past the last bar the manual valuation takes over
        assert!((history_on(&series, "2026-03-03").stocks - 1205.0).abs() < 1e-9);
        assert!((history_on(&series, "2026-03-04").stocks - 1205.0).abs() < 1e-9);
    }

    #[test]
    fn history_is_empty_without_any_transactions() {
        let (_file, db_path) = setup_test_db();
        let conn = open_db(&db_path).unwrap();
        assert!(build_portfolio_history(&conn, None, None).unwrap().is_empty());
    }

    #[test]
    fn history_rejects_a_backwards_range() {
        let (_file, db_path) = setup_test_db();
        let conn = open_db(&db_path).unwrap();
        let mut buy = trade_payload("BHP.AX", "purchase", 1.0, 1.0);
        buy.date = "2026-03-01".to_string();
        insert_holding_transaction(&db_path, "BHP.AX", buy).unwrap();
        assert!(build_portfolio_history(&conn, Some("2026-03-05"), Some("2026-03-01")).is_err());
    }

    // -------------------------------------------------------------------------
    // Cash ledger write paths
    // -------------------------------------------------------------------------

    /// Break one table so the queries that read it fail, without disturbing the
    /// rows the code under test is supposed to protect. This is what a transient
    /// database error looks like from inside a handler.
    fn drop_table(db_path: &PathBuf, table: &str) {
        open_db(db_path).unwrap().execute(&format!("DROP TABLE {}", table), []).unwrap();
    }

    /// The balance is the input to two guards and one reported figure. Returning
    /// a wrong zero instead of an error is what made all three unsafe.
    #[test]
    fn a_balance_that_cannot_be_read_is_an_error_not_a_zero() {
        let (_file, db_path) = setup_test_db();
        seed_cash_account(&db_path, 1, "Everyday", "AUD");
        let conn = open_db(&db_path).unwrap();
        // An empty ledger is a legitimate zero, and stays one.
        assert_eq!(cash_balance(&conn, 1, None).unwrap(), 0.0);
        drop(conn);

        drop_table(&db_path, "cash_transactions");
        let conn = open_db(&db_path).unwrap();
        assert!(cash_balance(&conn, 1, None).is_err(), "a failed read must not look like an empty account");
    }

    /// The guard exists because SQLite's foreign keys are not enforced here, so
    /// failing it open orphans the whole ledger.
    #[actix_web::test]
    async fn a_cash_account_is_kept_when_its_transaction_count_cannot_be_read() {
        let (_file, db_path) = setup_test_db();
        seed_cash_account(&db_path, 1, "Everyday", "AUD");
        drop_table(&db_path, "cash_transactions");

        let app = actix_web::test::init_service(
            App::new().app_data(web::Data::new(db_path.clone())).service(delete_cash_account),
        )
        .await;
        let req = actix_web::test::TestRequest::delete().uri("/api/cash/accounts/1").to_request();
        let resp = actix_web::test::call_service(&app, req).await;
        assert_eq!(resp.status(), actix_web::http::StatusCode::INTERNAL_SERVER_ERROR);

        let still_there: i64 = open_db(&db_path)
            .unwrap()
            .query_row("SELECT COUNT(*) FROM cash_accounts WHERE id = 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(still_there, 1, "the account must survive a check that could not run");
    }

    /// Changing the currency reinterprets every amount already in the ledger, so
    /// an unreadable balance has to block the change rather than wave it through.
    #[actix_web::test]
    async fn a_currency_change_is_refused_when_the_balance_cannot_be_read() {
        let (_file, db_path) = setup_test_db();
        seed_cash_account(&db_path, 1, "Everyday", "AUD");
        drop_table(&db_path, "cash_transactions");

        let app = actix_web::test::init_service(
            App::new().app_data(web::Data::new(db_path.clone())).service(update_cash_account),
        )
        .await;
        let body = serde_json::json!({ "name": "Everyday", "currency": "USD", "include_in_portfolio": true });
        let req = actix_web::test::TestRequest::put()
            .uri("/api/cash/accounts/1")
            .set_json(&body)
            .to_request();
        let resp = actix_web::test::call_service(&app, req).await;
        assert_eq!(resp.status(), actix_web::http::StatusCode::INTERNAL_SERVER_ERROR);

        let currency: String = open_db(&db_path)
            .unwrap()
            .query_row("SELECT currency FROM cash_accounts WHERE id = 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(currency, "AUD", "the amounts already recorded stay in the currency they were entered in");
    }

    fn seed_cash_account(db_path: &PathBuf, id: i64, name: &str, currency: &str) {
        let conn = open_db(db_path).unwrap();
        conn.execute(
            "INSERT INTO cash_accounts (id, name, currency, include_in_portfolio, created_at)
             VALUES (?1, ?2, ?3, 1, 'x')",
            params![id, name, currency],
        )
        .unwrap();
    }

    fn trade_payload(symbol: &str, tx_type: &str, qty: f64, price: f64) -> NewHoldingTransaction {
        NewHoldingTransaction {
            symbol: symbol.to_string(),
            transaction_type: tx_type.to_string(),
            date: "2026-02-02".to_string(),
            quantity: Some(qty),
            price: Some(price),
            amount: None,
            brokerage: None,
            notes: None,
            currency: None,
            original_price: None,
            fx_rate: None,
            custom_fields: None,
            cash_account_id: None,
            withholding_amount: None,
            confirm: None,
        }
    }

    fn cash_legs(db_path: &PathBuf, holding_tx_id: i64) -> Vec<(f64, String, i64)> {
        let conn = open_db(db_path).unwrap();
        let mut stmt = conn
            .prepare("SELECT amount, kind, account_id FROM cash_transactions WHERE holding_tx_id = ?1")
            .unwrap();
        let rows = stmt
            .query_map(params![holding_tx_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap();
        rows.filter_map(|r| r.ok()).collect()
    }

    /// A buy takes cash out including brokerage; a sale puts it back net of it.
    #[test]
    fn trade_writes_a_settlement_leg_in_the_right_direction() {
        let (_file, db_path) = setup_test_db();
        seed_cash_account(&db_path, 1, "Settlement AUD", "AUD");

        let mut buy = trade_payload("BHP.AX", "purchase", 100.0, 10.0);
        buy.brokerage = Some(9.5);
        buy.cash_account_id = Some(1);
        let bought = insert_holding_transaction(&db_path, "BHP.AX", buy).unwrap();
        assert_eq!(cash_legs(&db_path, bought.id), vec![(-1009.5, "trade_buy".to_string(), 1)]);

        let mut sell = trade_payload("BHP.AX", "sale", 50.0, 12.0);
        sell.brokerage = Some(9.5);
        sell.cash_account_id = Some(1);
        let sold = insert_holding_transaction(&db_path, "BHP.AX", sell).unwrap();
        assert_eq!(cash_legs(&db_path, sold.id), vec![(590.5, "trade_sell".to_string(), 1)]);

        // Balance is derived, never stored
        let conn = open_db(&db_path).unwrap();
        assert!((cash_balance(&conn, 1, None).unwrap() - (-419.0)).abs() < 1e-9);
    }

    /// A trade naming no account leaves the ledger alone — which is every one
    /// of the transactions recorded before the ledger existed.
    #[test]
    fn trade_without_an_account_writes_no_leg() {
        let (_file, db_path) = setup_test_db();
        let record = insert_holding_transaction(&db_path, "BHP.AX", trade_payload("BHP.AX", "purchase", 10.0, 5.0)).unwrap();
        assert!(cash_legs(&db_path, record.id).is_empty());
    }

    /// Editing a trade must move its cash with it, and clearing the account
    /// must remove the leg rather than strand a stale one.
    #[test]
    fn editing_a_trade_rewrites_its_leg_and_clearing_the_account_removes_it() {
        let (_file, db_path) = setup_test_db();
        seed_cash_account(&db_path, 1, "Settlement AUD", "AUD");
        seed_cash_account(&db_path, 2, "Other AUD", "AUD");

        let mut buy = trade_payload("BHP.AX", "purchase", 100.0, 10.0);
        buy.cash_account_id = Some(1);
        let record = insert_holding_transaction(&db_path, "BHP.AX", buy).unwrap();
        assert_eq!(cash_legs(&db_path, record.id), vec![(-1000.0, "trade_buy".to_string(), 1)]);

        // Re-price and move to the other account
        let mut edit = trade_payload("BHP.AX", "purchase", 100.0, 11.0);
        edit.cash_account_id = Some(2);
        modify_holding_transaction(&db_path, record.id, "BHP.AX", edit).unwrap();
        assert_eq!(cash_legs(&db_path, record.id), vec![(-1100.0, "trade_buy".to_string(), 2)]);

        // Detaching the account withdraws the trade from the ledger entirely
        let detach = trade_payload("BHP.AX", "purchase", 100.0, 11.0);
        modify_holding_transaction(&db_path, record.id, "BHP.AX", detach).unwrap();
        assert!(cash_legs(&db_path, record.id).is_empty());
    }

    /// Deleting the trade takes its cash movement with it; leaving one behind
    /// would silently misstate every later balance.
    #[test]
    fn deleting_a_trade_removes_its_leg() {
        let (_file, db_path) = setup_test_db();
        seed_cash_account(&db_path, 1, "Settlement AUD", "AUD");
        let mut buy = trade_payload("BHP.AX", "purchase", 100.0, 10.0);
        buy.cash_account_id = Some(1);
        let record = insert_holding_transaction(&db_path, "BHP.AX", buy).unwrap();

        assert!(remove_holding_transaction(&db_path, record.id).unwrap());
        assert!(cash_legs(&db_path, record.id).is_empty());
        let conn = open_db(&db_path).unwrap();
        assert_eq!(cash_balance(&conn, 1, None).unwrap(), 0.0);
    }

    /// Brokerage is stored in AUD, so a foreign settlement has to convert it
    /// back or the leg would mix two currencies in one number.
    #[test]
    fn foreign_trade_settles_in_native_currency_including_brokerage() {
        let (_file, db_path) = setup_test_db();
        seed_cash_account(&db_path, 1, "Settlement USD", "USD");

        let mut buy = trade_payload("AAPL", "purchase", 10.0, 150.0);
        buy.currency = Some("USD".to_string());
        buy.original_price = Some(100.0); // USD
        buy.fx_rate = Some(1.5); // 1 USD = 1.5 AUD
        buy.brokerage = Some(15.0); // AUD
        buy.cash_account_id = Some(1);
        let record = insert_holding_transaction(&db_path, "AAPL", buy).unwrap();

        // 10 × US$100 plus US$10 of brokerage (A$15 ÷ 1.5)
        assert_eq!(cash_legs(&db_path, record.id), vec![(-1010.0, "trade_buy".to_string(), 1)]);
    }

    /// A dividend credits the account it is paid into, and does so on the
    /// payment date rather than the ex-date it is filed under — crediting on
    /// the ex-date would show the money weeks before it arrived.
    #[test]
    fn dividend_settles_on_its_payment_date() {
        let (_file, db_path) = setup_test_db();
        seed_cash_account(&db_path, 1, "CBA Invest", "AUD");
        {
            let conn = open_db(&db_path).unwrap();
            conn.execute(
                "INSERT INTO dividend_events (symbol, ex_date, payment_date, amount, fetched_at)
                 VALUES ('SUL.AX', '2026-03-12', '2026-04-08', 0.64, 'x')",
                [],
            )
            .unwrap();
        }

        let mut div = trade_payload("SUL.AX", "dividend", 200.0, 0.64);
        div.amount = Some(128.0);
        div.date = "2026-03-12".to_string();
        div.cash_account_id = Some(1);

        let record = insert_holding_transaction(&db_path, "SUL.AX", div).expect("dividend is allowed");
        assert_eq!(cash_legs(&db_path, record.id), vec![(128.0, "dividend".to_string(), 1)]);

        let conn = open_db(&db_path).unwrap();
        let date: String = conn
            .query_row("SELECT date FROM cash_transactions WHERE holding_tx_id = ?1", params![record.id], |r| r.get(0))
            .unwrap();
        assert_eq!(date, "2026-04-08", "cash should land on the payment date, not the ex-date");
    }

    /// US-domiciled funds withhold tax before the cash arrives, and Yahoo
    /// reports the gross distribution, so crediting Yahoo's figure banks money
    /// that never landed. The tax is booked as its own leg rather than netted
    /// away, because the amount withheld is needed at tax time.
    #[test]
    fn dividend_withholding_is_recorded_as_its_own_leg() {
        let (_file, db_path) = setup_test_db();
        seed_cash_account(&db_path, 1, "CBA Invest", "AUD");
        {
            let conn = open_db(&db_path).unwrap();
            conn.execute(
                "INSERT INTO symbol_info (symbol, dividend_withholding_pct, updated_at)
                 VALUES ('VEU.AX', 30.0, 'x')",
                [],
            )
            .unwrap();
        }

        let mut div = trade_payload("VEU.AX", "dividend", 22.0, 0.5644);
        div.amount = Some(12.42); // gross, as Yahoo reports it
        div.cash_account_id = Some(1);
        let record = insert_holding_transaction(&db_path, "VEU.AX", div).expect("dividend is allowed");

        let mut legs = cash_legs(&db_path, record.id);
        legs.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
        assert_eq!(
            legs,
            vec![(12.42, "dividend".to_string(), 1), (-3.73, "fee".to_string(), 1)],
            "gross credited, withholding taken out separately"
        );
        // The bank credited 8.69 — the two legs must sum to exactly that.
        assert!((legs.iter().map(|l| l.0).sum::<f64>() - 8.69).abs() < 0.005);
    }

    fn insert_dividend_event_for(db_path: &PathBuf, symbol: &str, ex_date: &str, amount: f64) {
        let conn = open_db(db_path).unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO dividend_events (symbol, ex_date, amount, fetched_at)
             VALUES (?1, ?2, ?3, 'x')",
            params![symbol, ex_date, amount],
        )
        .unwrap();
    }

    fn seed_dividend_setup(db_path: &PathBuf, currency: &str, account: i64) {
        seed_cash_account(db_path, account, "Dividend Account", currency);
        let conn = open_db(db_path).unwrap();
        conn.execute(
            "INSERT INTO app_config (key, value) VALUES (?1, ?2)",
            params![format!("dividend_account_{}", currency), account.to_string()],
        )
        .unwrap();
    }

    fn seed_holding(db_path: &PathBuf, symbol: &str, date: &str, qty: f64, currency: &str) {
        let conn = open_db(db_path).unwrap();
        conn.execute(
            "INSERT INTO holdings_transactions (symbol, transaction_type, date, quantity, price, currency, created_at)
             VALUES (?1, 'purchase', ?2, ?3, 10.0, ?4, 'x')",
            params![symbol, date, qty, currency],
        )
        .unwrap();
    }

    fn dividend_rows(db_path: &PathBuf, symbol: &str) -> Vec<(String, f64)> {
        let conn = open_db(db_path).unwrap();
        let mut stmt = conn
            .prepare("SELECT date, amount FROM holdings_transactions WHERE symbol = ?1 AND transaction_type = 'dividend' ORDER BY date")
            .unwrap();
        let rows = stmt.query_map(params![symbol], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        rows.filter_map(|r| r.ok()).collect()
    }

    /// A fetched event should reach the cash ledger by itself. Before this, it
    /// sat in the Transactions screen as a derived row with no account until
    /// someone remembered to run a script.
    #[test]
    fn a_fetched_dividend_is_recorded_and_settled_automatically() {
        let (_file, db_path) = setup_test_db();
        seed_dividend_setup(&db_path, "AUD", 1);
        seed_holding(&db_path, "VAS.AX", "2026-01-05", 40.0, "AUD");
        insert_dividend_event_for(&db_path, "VAS.AX", "2026-04-01", 0.65);

        let result = record_new_dividends(&db_path).unwrap();
        assert_eq!(result.recorded, 1);
        assert_eq!(dividend_rows(&db_path, "VAS.AX"), vec![("2026-04-01".to_string(), 26.0)]);

        let conn = open_db(&db_path).unwrap();
        let leg: (f64, String) = conn
            .query_row(
                "SELECT c.amount, c.kind FROM cash_transactions c
                   JOIN holdings_transactions h ON h.id = c.holding_tx_id
                  WHERE h.symbol = 'VAS.AX'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(leg, (26.0, "dividend".to_string()));
    }

    /// Running twice must not pay the holder twice.
    #[test]
    fn recording_dividends_is_idempotent() {
        let (_file, db_path) = setup_test_db();
        seed_dividend_setup(&db_path, "AUD", 1);
        seed_holding(&db_path, "VAS.AX", "2026-01-05", 40.0, "AUD");
        insert_dividend_event_for(&db_path, "VAS.AX", "2026-04-01", 0.65);

        assert_eq!(record_new_dividends(&db_path).unwrap().recorded, 1);
        let second = record_new_dividends(&db_path).unwrap();
        assert_eq!(second.recorded, 0);
        assert_eq!(second.already_present, 0, "the query filters them out before counting");
        assert_eq!(dividend_rows(&db_path, "VAS.AX").len(), 1);
    }

    /// Deleting a dividend means "not this one". Automatic recording would
    /// otherwise reinstate it on the next refresh and the deletion would look
    /// like it had silently failed.
    #[test]
    fn a_deleted_dividend_is_not_recreated_by_the_next_refresh() {
        let (_file, db_path) = setup_test_db();
        seed_dividend_setup(&db_path, "AUD", 1);
        seed_holding(&db_path, "NDQ.AX", "2025-01-02", 60.0, "AUD");
        insert_dividend_event_for(&db_path, "NDQ.AX", "2025-01-02", 0.028393);

        record_new_dividends(&db_path).unwrap();
        let id: i64 = {
            let conn = open_db(&db_path).unwrap();
            conn.query_row(
                "SELECT id FROM holdings_transactions WHERE symbol = 'NDQ.AX' AND transaction_type = 'dividend'",
                [],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert!(remove_holding_transaction(&db_path, id).unwrap());

        let after = record_new_dividends(&db_path).unwrap();
        assert_eq!(after.recorded, 0, "a declined dividend must stay declined");
        assert_eq!(after.excluded, 1);
        assert!(dividend_rows(&db_path, "NDQ.AX").is_empty());
    }

    /// Entitlement follows the shares held at the ex-date.
    #[test]
    fn dividends_before_the_holding_existed_are_not_recorded() {
        let (_file, db_path) = setup_test_db();
        seed_dividend_setup(&db_path, "AUD", 1);
        seed_holding(&db_path, "VAS.AX", "2026-01-05", 40.0, "AUD");
        insert_dividend_event_for(&db_path, "VAS.AX", "2025-06-01", 0.65);

        assert_eq!(record_new_dividends(&db_path).unwrap().recorded, 0);
        assert!(dividend_rows(&db_path, "VAS.AX").is_empty());
    }

    /// With no destination configured, guessing would move real money to the
    /// wrong account — so nothing is recorded and the currency is reported.
    #[test]
    fn dividends_in_an_unconfigured_currency_are_left_alone() {
        let (_file, db_path) = setup_test_db();
        seed_dividend_setup(&db_path, "AUD", 1); // AUD configured, USD not
        seed_holding(&db_path, "NSC", "2026-01-05", 4.0, "USD");
        insert_dividend_event_for(&db_path, "NSC", "2026-04-01", 1.35);

        let result = record_new_dividends(&db_path).unwrap();
        assert_eq!(result.recorded, 0);
        assert_eq!(result.unconfigured_currencies, vec!["USD".to_string()]);
        assert!(dividend_rows(&db_path, "NSC").is_empty());
    }

    /// EXPD 2026-09-17: the bar opened at its high of 190.92, but the chart
    /// meta reported a day high of 190.255. Taking high from meta and open from
    /// the bar stored an open above its own high.
    #[test]
    fn session_range_contains_the_open_when_meta_misses_the_opening_print() {
        let (high, low) = session_range(
            [Some(190.9199981689453), Some(190.255)],
            [Some(187.89999389648438), Some(187.9)],
            [Some(190.9199981689453), Some(189.08)],
        );
        let (high, low) = (high.unwrap(), low.unwrap());
        assert_eq!(high, 190.9199981689453);
        assert_eq!(low, 187.89999389648438);
        for trade in [190.9199981689453, 189.08] {
            assert!(low <= trade && trade <= high, "{trade} outside {low}..{high}");
        }
    }

    #[test]
    fn session_range_widens_to_a_price_past_a_lagging_bar() {
        let (high, low) = session_range([Some(10.0), None], [Some(9.0), None], [Some(9.5), Some(10.2)]);
        assert_eq!((high, low), (Some(10.2), Some(9.0)));
    }

    #[test]
    fn session_range_without_highs_or_lows_invents_nothing() {
        let (high, low) = session_range([None, None], [None, Some(0.0)], [Some(9.5), Some(10.2)]);
        assert_eq!((high, low), (None, None), "open and price alone are not a range; zero is not a low");
    }

    /// Yahoo reports a delisted symbol as `regularMarketPrice: 0.0`, not null,
    /// so a presence check accepts it. Cached as a real quote it marks the
    /// holding worthless, and written to history it overwrites a good close
    /// with zero — losing the last price the symbol ever traded at.
    #[test]
    fn a_delisted_symbols_zero_quote_never_replaces_a_real_price() {
        let (_file, db_path) = setup_test_db();
        let conn = open_db(&db_path).unwrap();
        conn.execute(
            "INSERT INTO prices (symbol, date, close, fetched_at) VALUES ('DEAD.AX', '2026-07-17', 355.89, 'x')",
            [],
        )
        .unwrap();

        let good = CurrentPrice {
            symbol: "DEAD.AX".to_string(),
            price: Some(355.89),
            change: None,
            change_percent: None,
            volume: None,
            day_open: None,
            day_high: None,
            day_low: None,
            last_updated: "2026-07-17T00:00:00Z".to_string(),
            price_date: Some("2026-07-17".to_string()),
            error: None,
        };
        cache_current_price(&conn, &good).unwrap();
        // The symbol stops trading; every later fetch answers zero.
        let dead = CurrentPrice { price: Some(0.0), ..good };
        cache_current_price(&conn, &dead).unwrap();
        persist_price_to_history(&conn, "DEAD.AX", &dead, "y");

        let cached: f64 = conn
            .query_row("SELECT price FROM cached_current_prices WHERE symbol = 'DEAD.AX'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(cached, 355.89, "the last good quote must survive a zero");

        let close: f64 = conn
            .query_row("SELECT close FROM prices WHERE symbol = 'DEAD.AX' AND date = '2026-07-17'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(close, 355.89, "a zero must not overwrite a real close");

        let warnings: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM event_log WHERE symbol = 'DEAD.AX' AND level = 'warn'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(warnings >= 2, "both the cache and the history refusal must be logged, got {warnings}");
    }

    /// Only a positive, finite number is a price.
    #[test]
    fn usable_quotes_exclude_zero_negative_and_nan() {
        assert!(is_usable_quote(Some(0.01)));
        assert!(!is_usable_quote(Some(0.0)), "a delisted symbol's zero");
        assert!(!is_usable_quote(Some(-1.0)));
        assert!(!is_usable_quote(Some(f64::NAN)));
        assert!(!is_usable_quote(None));
    }

    /// A table keyed by ticker that the rename does not know about strands its
    /// rows under the old name — price history stops, dividends vanish, the
    /// watchlist entry orphans. Nothing fails loudly when that happens, so the
    /// schema is enumerated here and every symbol-keyed table must be either
    /// migrated or deliberately excluded.
    #[test]
    fn every_symbol_keyed_table_is_either_migrated_or_excluded() {
        // Rewriting a historical log would falsify what it recorded.
        const EXCLUDED: &[&str] = &["event_log"];

        let (_file, db_path) = setup_test_db();
        let conn = open_db(&db_path).unwrap();

        let tables: Vec<String> = {
            let mut stmt = conn
                .prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'")
                .unwrap();
            let rows = stmt.query_map([], |r| r.get::<_, String>(0)).unwrap();
            rows.filter_map(|r| r.ok()).collect()
        };

        for table in tables {
            let has_symbol = {
                let mut stmt = conn.prepare(&format!("PRAGMA table_info({})", table)).unwrap();
                let cols = stmt.query_map([], |r| r.get::<_, String>(1)).unwrap();
                cols.filter_map(|r| r.ok()).any(|c| c == "symbol")
            };
            if !has_symbol {
                continue;
            }
            assert!(
                SYMBOL_KEYED_TABLES.contains(&table.as_str()) || EXCLUDED.contains(&table.as_str()),
                "`{}` has a symbol column but a rename neither moves it nor documents why not — \
                 add it to SYMBOL_KEYED_TABLES, or to the exclusions with a reason",
                table
            );
        }

        // The reverse direction catches a typo'd or dropped table name. Legacy
        // tables are exempt: `watchlist_prices` still exists in databases from
        // older versions, holding rows that must follow a rename, but `init_db`
        // no longer creates it — so it is absent here and skipped at runtime.
        const LEGACY: &[&str] = &["watchlist_prices"];
        for table in SYMBOL_KEYED_TABLES {
            if LEGACY.contains(table) {
                continue;
            }
            let exists: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                    params![table],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(exists, 1, "SYMBOL_KEYED_TABLES names `{}`, which does not exist", table);
        }
    }

    /// The rename has to carry rows in every symbol-keyed table, not just the
    /// holdings ones — a stranded price series or watchlist entry is invisible
    /// until someone notices the chart is empty.
    #[test]
    fn renaming_a_symbol_moves_rows_in_every_table() {
        let (_file, db_path) = setup_test_db();
        {
            let conn = open_db(&db_path).unwrap();
            conn.execute_batch(
                "INSERT INTO holdings_transactions (symbol, transaction_type, date, quantity, price, currency, created_at)
                     VALUES ('OLD.AX', 'purchase', '2026-01-05', 10.0, 5.0, 'AUD', 'x');
                 INSERT INTO symbol_info (symbol, currency, dividend_withholding_pct, updated_at)
                     VALUES ('OLD.AX', 'AUD', 30.0, 'x');
                 INSERT INTO prices (symbol, date, close, fetched_at) VALUES ('OLD.AX', '2026-01-05', 5.0, 'x');
                 INSERT INTO dividend_events (symbol, ex_date, amount, fetched_at)
                     VALUES ('OLD.AX', '2026-02-01', 0.25, 'x');
                 INSERT INTO cached_current_prices (symbol, price, last_updated) VALUES ('OLD.AX', 6.0, 'x');
                 INSERT INTO watchlist_symbols (symbol, updated_at) VALUES ('OLD.AX', 'x');
                 INSERT INTO watchlist_memberships (symbol, list_name, added_at) VALUES ('OLD.AX', 'Main', 'x');",
            )
            .unwrap();
        }

        let moved = rename_holdings_symbol(&db_path, "OLD.AX", "NEW.AX").expect("rename succeeds");
        assert_eq!(moved, 1, "should report the holdings transactions moved");

        let conn = open_db(&db_path).unwrap();
        for table in SYMBOL_KEYED_TABLES {
            // Legacy tables are absent from a fresh schema — see the migration.
            let present: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                    params![table],
                    |r| r.get(0),
                )
                .unwrap();
            if present == 0 {
                continue;
            }
            let left: i64 = conn
                .query_row(&format!("SELECT COUNT(*) FROM {} WHERE symbol = 'OLD.AX'", table), [], |r| r.get(0))
                .unwrap();
            assert_eq!(left, 0, "`{}` still holds rows under the old symbol", table);
        }
        for (table, expected) in [
            ("holdings_transactions", 1),
            ("symbol_info", 1),
            ("prices", 1),
            ("dividend_events", 1),
            ("cached_current_prices", 1),
            ("watchlist_symbols", 1),
            ("watchlist_memberships", 1),
        ] {
            let found: i64 = conn
                .query_row(&format!("SELECT COUNT(*) FROM {} WHERE symbol = 'NEW.AX'", table), [], |r| r.get(0))
                .unwrap();
            assert_eq!(found, expected, "`{}` did not receive the renamed rows", table);
        }
    }

    /// Where the target symbol already holds a row under the same key, its own
    /// row stands and the stale duplicate is dropped, rather than the rename
    /// failing on a constraint violation halfway through.
    #[test]
    fn renaming_onto_an_existing_symbol_keeps_the_targets_rows() {
        let (_file, db_path) = setup_test_db();
        {
            let conn = open_db(&db_path).unwrap();
            conn.execute_batch(
                "INSERT INTO prices (symbol, date, close, fetched_at) VALUES ('OLD.AX', '2026-01-05', 5.0, 'x');
                 INSERT INTO prices (symbol, date, close, fetched_at) VALUES ('OLD.AX', '2026-01-06', 5.5, 'x');
                 INSERT INTO prices (symbol, date, close, fetched_at) VALUES ('NEW.AX', '2026-01-05', 9.9, 'x');",
            )
            .unwrap();
        }

        rename_holdings_symbol(&db_path, "OLD.AX", "NEW.AX").expect("a clashing rename still succeeds");

        let conn = open_db(&db_path).unwrap();
        let kept: f64 = conn
            .query_row("SELECT close FROM prices WHERE symbol = 'NEW.AX' AND date = '2026-01-05'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(kept, 9.9, "the target's own row wins on a clash");
        let moved: f64 = conn
            .query_row("SELECT close FROM prices WHERE symbol = 'NEW.AX' AND date = '2026-01-06'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(moved, 5.5, "the non-clashing row still moves");
        let left: i64 = conn
            .query_row("SELECT COUNT(*) FROM prices WHERE symbol = 'OLD.AX'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(left, 0, "nothing is stranded under the old symbol");
    }

    /// A rename copies `symbol_info` column by column, so a column added later
    /// is silently left behind — no error, just a new symbol quietly missing
    /// settings the old one had. Losing `dividend_withholding_pct` this way
    /// would credit every later distribution gross again.
    ///
    /// Enumerated from the schema rather than listed, so the next column added
    /// fails here instead of going missing in production.
    #[test]
    fn renaming_a_symbol_carries_every_symbol_info_column() {
        let (_file, db_path) = setup_test_db();
        {
            let conn = open_db(&db_path).unwrap();
            conn.execute(
                "INSERT INTO symbol_info (symbol, instrument_type, long_name, currency, dividend_withholding_pct, updated_at)
                 VALUES ('OLD.AX', 'EQUITY', 'Old Ltd', 'AUD', 30.0, 'x')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO holdings_transactions (symbol, transaction_type, date, quantity, price, currency, created_at)
                 VALUES ('OLD.AX', 'purchase', '2026-01-05', 10.0, 5.0, 'AUD', 'x')",
                [],
            )
            .unwrap();
        }

        let conn = open_db(&db_path).unwrap();
        let columns: Vec<String> = {
            let mut stmt = conn.prepare("PRAGMA table_info(symbol_info)").unwrap();
            let rows = stmt.query_map([], |r| r.get::<_, String>(1)).unwrap();
            rows.filter_map(|r| r.ok()).filter(|c| c != "symbol").collect()
        };
        assert!(
            columns.iter().any(|c| c == "dividend_withholding_pct"),
            "the column under test should exist"
        );
        let read = |symbol: &str, col: &str| -> Option<String> {
            conn.query_row(
                &format!("SELECT CAST({} AS TEXT) FROM symbol_info WHERE symbol = ?1", col),
                params![symbol],
                |r| r.get(0),
            )
            .unwrap()
        };

        // Captured first: the rename moves the row rather than copying it, so
        // afterwards there is no old row left to compare against.
        let before: Vec<(String, Option<String>)> =
            columns.iter().map(|c| (c.clone(), read("OLD.AX", c))).collect();

        rename_holdings_symbol(&db_path, "OLD.AX", "NEW.AX").expect("rename succeeds");

        for (col, was) in before {
            assert_eq!(
                was,
                read("NEW.AX", &col),
                "column `{}` did not survive the rename",
                col
            );
        }
    }

    /// TFN withholding applies to the unfranked portion of one distribution, so
    /// it varies per payment and stops once a TFN is quoted. A per-symbol rate
    /// cannot express that — DMP was docked 43% on its first payment and
    /// nothing on the next — so an amount recorded against the payment wins.
    #[test]
    fn a_payments_own_withholding_amount_overrides_the_symbol_rate() {
        let (_file, db_path) = setup_test_db();
        seed_cash_account(&db_path, 1, "CBA Invest", "AUD");
        {
            // A standing rate that must NOT be used when the payment carries
            // its own figure.
            let conn = open_db(&db_path).unwrap();
            conn.execute(
                "INSERT INTO symbol_info (symbol, dividend_withholding_pct, updated_at) VALUES ('DMP.AX', 30.0, 'x')",
                [],
            )
            .unwrap();
        }

        let mut div = trade_payload("DMP.AX", "dividend", 50.0, 0.555);
        div.amount = Some(27.75);
        div.withholding_amount = Some(12.00);
        div.cash_account_id = Some(1);
        let record = insert_holding_transaction(&db_path, "DMP.AX", div).expect("dividend is allowed");

        let mut legs = cash_legs(&db_path, record.id);
        legs.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
        assert_eq!(
            legs,
            vec![(27.75, "dividend".to_string(), 1), (-12.00, "fee".to_string(), 1)],
            "the payment's own amount is used, not 30% of the gross"
        );
        // The bank credited 15.75.
        assert!((legs.iter().map(|l| l.0).sum::<f64>() - 15.75).abs() < 0.005);
    }

    /// A symbol with no withholding configured keeps a single leg, so the
    /// ordinary Australian dividend is untouched.
    #[test]
    fn dividend_without_withholding_keeps_one_leg() {
        let (_file, db_path) = setup_test_db();
        seed_cash_account(&db_path, 1, "CBA Invest", "AUD");

        let mut div = trade_payload("VAS.AX", "dividend", 0.0, 0.0);
        div.quantity = None;
        div.price = None;
        div.amount = Some(26.01);
        div.cash_account_id = Some(1);
        let record = insert_holding_transaction(&db_path, "VAS.AX", div).expect("dividend is allowed");
        assert_eq!(cash_legs(&db_path, record.id), vec![(26.01, "dividend".to_string(), 1)]);
    }

    /// The dividend form only requires a total — share count and per-share rate
    /// are optional — so the leg has to be derivable from the total alone.
    /// Deriving it as `quantity * price`, the way a trade works, left a
    /// hand-entered dividend with no cash movement at all.
    #[test]
    fn dividend_recorded_as_a_total_alone_still_settles() {
        let (_file, db_path) = setup_test_db();
        seed_cash_account(&db_path, 1, "CBA Invest", "AUD");

        let mut div = trade_payload("VAS.AX", "dividend", 0.0, 0.0);
        div.quantity = None;
        div.price = None;
        div.amount = Some(185.22);
        div.cash_account_id = Some(1);

        let record = insert_holding_transaction(&db_path, "VAS.AX", div).expect("dividend is allowed");
        assert_eq!(cash_legs(&db_path, record.id), vec![(185.22, "dividend".to_string(), 1)]);
    }

    /// A foreign dividend paid into an account of the same currency books
    /// natively, converting the AUD total back when no per-share native figure
    /// was recorded.
    #[test]
    fn foreign_dividend_settles_natively_from_the_recorded_total() {
        let (_file, db_path) = setup_test_db();
        seed_cash_account(&db_path, 1, "IBKR", "USD");

        let mut div = trade_payload("NSC", "dividend", 0.0, 0.0);
        div.quantity = None;
        div.price = None;
        div.currency = Some("USD".to_string());
        div.fx_rate = Some(1.42178702354431);
        div.amount = Some(7.68); // AUD total
        div.cash_account_id = Some(1);

        let record = insert_holding_transaction(&db_path, "NSC", div).expect("dividend is allowed");
        let legs = cash_legs(&db_path, record.id);
        assert_eq!(legs.len(), 1);
        assert!(
            (legs[0].0 - 7.68 / 1.42178702354431).abs() < 0.01,
            "USD account should be credited in USD, got {}",
            legs[0].0
        );
    }

    /// Without a fetched event there is no payment date to use, so the ex-date
    /// stands rather than the leg being dropped.
    #[test]
    fn dividend_without_a_known_payment_date_settles_on_its_own_date() {
        let (_file, db_path) = setup_test_db();
        seed_cash_account(&db_path, 1, "CBA Invest", "AUD");

        let mut div = trade_payload("XRF.AX", "dividend", 100.0, 0.5);
        div.amount = Some(50.0);
        div.date = "2025-09-11".to_string();
        div.cash_account_id = Some(1);

        let record = insert_holding_transaction(&db_path, "XRF.AX", div).expect("dividend is allowed");
        let conn = open_db(&db_path).unwrap();
        let date: String = conn
            .query_row("SELECT date FROM cash_transactions WHERE holding_tx_id = ?1", params![record.id], |r| r.get(0))
            .unwrap();
        assert_eq!(date, "2025-09-11");
    }

    /// A dividend leg shares its kind with a hand-entered dividend, so the
    /// manual endpoints have to tell them apart by their link, not their kind,
    /// or an edit would be silently undone the next time the transaction saves.
    #[actix_web::test]
    async fn a_dividend_leg_cannot_be_edited_as_a_manual_entry() {
        let (_file, db_path) = setup_test_db();
        seed_cash_account(&db_path, 1, "CBA Invest", "AUD");

        let mut div = trade_payload("SUL.AX", "dividend", 200.0, 0.64);
        div.amount = Some(128.0);
        div.cash_account_id = Some(1);
        let record = insert_holding_transaction(&db_path, "SUL.AX", div).expect("dividend is allowed");

        let leg_id: i64 = {
            let conn = open_db(&db_path).unwrap();
            conn.query_row(
                "SELECT id FROM cash_transactions WHERE holding_tx_id = ?1",
                params![record.id],
                |r| r.get(0),
            )
            .unwrap()
        };

        let app = actix_web::test::init_service(
            App::new()
                .app_data(web::Data::new(db_path.clone()))
                .service(update_cash_transaction)
                .service(delete_cash_transaction),
        )
        .await;

        let req = actix_web::test::TestRequest::put()
            .uri(&format!("/api/cash/transactions/{}", leg_id))
            .set_json(serde_json::json!({
                "account_id": 1, "date": "2026-03-12", "amount": 999.0, "kind": "dividend"
            }))
            .to_request();
        let resp = actix_web::test::call_service(&app, req).await;
        assert_eq!(resp.status(), 400, "editing a linked dividend leg must be refused");

        let req = actix_web::test::TestRequest::delete()
            .uri(&format!("/api/cash/transactions/{}", leg_id))
            .to_request();
        let resp = actix_web::test::call_service(&app, req).await;
        assert_eq!(resp.status(), 400, "deleting a linked dividend leg must be refused");
    }

    /// A USD trade settling from an AUD account is not a modelling error — it
    /// is what CommSec International does, converting per trade because the
    /// account never holds USD. The leg is the AUD that actually left, and the
    /// rate is preserved on the trade rather than lost in the conversion.
    #[test]
    fn foreign_trade_settles_from_an_aud_account_by_converting() {
        let (_file, db_path) = setup_test_db();
        seed_cash_account(&db_path, 1, "CommSec International", "AUD");

        let mut buy = trade_payload("AAPL", "purchase", 10.0, 150.0);
        buy.currency = Some("USD".to_string());
        buy.original_price = Some(100.0);
        buy.fx_rate = Some(1.5);
        buy.brokerage = Some(11.95);
        buy.cash_account_id = Some(1);

        let record = insert_holding_transaction(&db_path, "AAPL", buy).expect("converted settlement is allowed");
        // 10 x 150.00 AUD plus 11.95 AUD brokerage. Both are already AUD, so
        // neither is converted a second time.
        assert_eq!(cash_legs(&db_path, record.id), vec![(-1511.95, "trade_buy".to_string(), 1)]);

        let conn = open_db(&db_path).unwrap();
        let notes: String = conn
            .query_row("SELECT notes FROM cash_transactions WHERE holding_tx_id = ?1", params![record.id], |r| r.get(0))
            .unwrap();
        assert!(notes.contains("USD 1000.00"), "note should state the foreign amount: {notes}");
        assert!(notes.contains("1.5000"), "note should state the rate used: {notes}");
    }

    /// The matching-currency path must keep booking natively — an IBKR USD
    /// account holds a real USD balance and must not be handed AUD.
    #[test]
    fn foreign_trade_settles_natively_from_a_matching_account() {
        let (_file, db_path) = setup_test_db();
        seed_cash_account(&db_path, 1, "IBKR", "USD");

        let mut buy = trade_payload("AAPL", "purchase", 10.0, 150.0);
        buy.currency = Some("USD".to_string());
        buy.original_price = Some(100.0);
        buy.fx_rate = Some(1.5);
        buy.brokerage = Some(15.0);
        buy.cash_account_id = Some(1);

        let record = insert_holding_transaction(&db_path, "AAPL", buy).expect("native settlement is allowed");
        // 10 x 100.00 USD plus 15.00 AUD brokerage expressed as 10.00 USD.
        assert_eq!(cash_legs(&db_path, record.id), vec![(-1010.0, "trade_buy".to_string(), 1)]);
    }

    /// Converting is only defined toward AUD, where the trade already carries
    /// the rate. Any other mismatch must still be refused rather than silently
    /// inventing a rate.
    #[test]
    fn cross_currency_without_an_aud_leg_is_still_refused() {
        let (_file, db_path) = setup_test_db();
        seed_cash_account(&db_path, 1, "Settlement USD", "USD");

        let mut buy = trade_payload("BHP.AX", "purchase", 10.0, 40.0);
        buy.currency = Some("AUD".to_string());
        buy.cash_account_id = Some(1);

        let err = match insert_holding_transaction(&db_path, "BHP.AX", buy) {
            Err(e) => e,
            Ok(_) => panic!("an AUD trade must not draw on a USD account"),
        };
        assert!(err.contains("settles in AUD"), "unhelpful error: {err}");
        assert!(err.contains("USD"), "error should name the account currency: {err}");
    }

    /// Balances are as-at a date, which is what the value graph will walk.
    #[test]
    fn cash_balance_is_derived_as_at_a_date() {
        let (_file, db_path) = setup_test_db();
        seed_cash_account(&db_path, 1, "Savings AUD", "AUD");
        let conn = open_db(&db_path).unwrap();
        for (date, amount, kind) in [
            ("2026-01-01", 1000.0, "opening_balance"),
            ("2026-02-01", 1000.0, "deposit"),
            ("2026-03-01", 4.10, "interest"),
        ] {
            conn.execute(
                "INSERT INTO cash_transactions (account_id, date, amount, kind, created_at) VALUES (1, ?1, ?2, ?3, 'x')",
                params![date, amount, kind],
            )
            .unwrap();
        }

        assert_eq!(cash_balance(&conn, 1, Some("2026-01-15")).unwrap(), 1000.0);
        assert_eq!(cash_balance(&conn, 1, Some("2026-02-01")).unwrap(), 2000.0);
        assert!((cash_balance(&conn, 1, None).unwrap() - 2004.10).abs() < 1e-9);
    }

    /// Editing is allowed for hand-entered rows, but not for the two kinds the
    /// ledger owns rather than the user: a trade's settlement leg, and either
    /// side of a conversion — changing one leg alone would invent money in one
    /// currency and destroy it in the other.
    #[actix_web::test]
    async fn editing_a_cash_transaction_is_blocked_for_trade_and_transfer_legs() {
        let (_file, db_path) = setup_test_db();
        seed_cash_account(&db_path, 1, "Settlement AUD", "AUD");
        seed_cash_account(&db_path, 2, "Settlement USD", "USD");

        // A plain deposit, a trade leg and a transfer pair
        let conn = open_db(&db_path).unwrap();
        conn.execute(
            "INSERT INTO cash_transactions (id, account_id, date, amount, kind, created_at)
             VALUES (10, 1, '2026-01-01', 1000.0, 'deposit', 'x')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO cash_transactions (id, account_id, date, amount, kind, transfer_group_id, created_at)
             VALUES (11, 1, '2026-01-02', -500.0, 'fx_out', 'g1', 'x')",
            [],
        )
        .unwrap();
        let mut buy = trade_payload("BHP.AX", "purchase", 10.0, 40.0);
        buy.cash_account_id = Some(1);
        let trade = insert_holding_transaction(&db_path, "BHP.AX", buy).unwrap();
        let leg_id: i64 = conn
            .query_row("SELECT id FROM cash_transactions WHERE holding_tx_id = ?1", params![trade.id], |r| r.get(0))
            .unwrap();
        drop(conn);

        let app = actix_web::test::init_service(
            App::new()
                .app_data(web::Data::new(db_path.clone()))
                .service(update_cash_transaction),
        )
        .await;
        let edit = |id: i64| {
            actix_web::test::TestRequest::put()
                .uri(&format!("/api/cash/transactions/{}", id))
                .set_json(serde_json::json!({
                    "account_id": 1, "date": "2026-01-05", "amount": 1234.0, "kind": "deposit"
                }))
                .to_request()
        };

        // A hand-entered row edits fine
        let resp = actix_web::test::call_service(&app, edit(10)).await;
        assert!(resp.status().is_success(), "deposit should be editable");

        let resp = actix_web::test::call_service(&app, edit(11)).await;
        assert_eq!(resp.status(), 400, "a transfer leg must not be editable alone");

        let resp = actix_web::test::call_service(&app, edit(leg_id)).await;
        assert_eq!(resp.status(), 400, "a trade's settlement leg must not be editable");

        // Only the deposit actually changed
        let conn = open_db(&db_path).unwrap();
        let (amount, date): (f64, String) = conn
            .query_row("SELECT amount, date FROM cash_transactions WHERE id = 10", [], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        assert_eq!((amount, date.as_str()), (1234.0, "2026-01-05"));
        let leg: f64 = conn
            .query_row("SELECT amount FROM cash_transactions WHERE id = 11", [], |r| r.get(0))
            .unwrap();
        assert_eq!(leg, -500.0, "the transfer leg is untouched");
    }

    #[test]
    fn cash_tx_validation_rejects_bad_input_and_trade_owned_kinds() {
        let (_file, db_path) = setup_test_db();
        seed_cash_account(&db_path, 1, "Settlement AUD", "AUD");
        let conn = open_db(&db_path).unwrap();
        let payload = |kind: &str, date: &str, amount: f64, account: i64| CashTxPayload {
            account_id: account,
            date: date.to_string(),
            amount,
            kind: kind.to_string(),
            notes: None,
        };

        assert!(validate_cash_tx(&conn, &payload("deposit", "2026-01-01", 100.0, 1)).is_ok());
        // A trade's leg is owned by the trade
        assert!(validate_cash_tx(&conn, &payload("trade_buy", "2026-01-01", -100.0, 1))
            .unwrap_err()
            .contains("created from the trade"));
        assert!(validate_cash_tx(&conn, &payload("nonsense", "2026-01-01", 100.0, 1)).is_err());
        assert!(validate_cash_tx(&conn, &payload("deposit", "01/01/2026", 100.0, 1)).is_err());
        assert!(validate_cash_tx(&conn, &payload("deposit", "2026-01-01", 0.0, 1)).is_err());
        assert!(validate_cash_tx(&conn, &payload("deposit", "2026-01-01", 100.0, 99))
            .unwrap_err()
            .contains("not found"));
    }

    /// The cash ledger is hand-entered money data, so every field has to reach
    /// audit_log — the static coverage check proves the trigger *names* the
    /// columns; this proves the values actually land, including across a
    /// re-init, which is where the `IF NOT EXISTS` trap used to bite.
    #[test]
    fn cash_ledger_changes_are_audited() {
        let (_file, db_path) = setup_test_db();
        init_db(&db_path).unwrap(); // a restart must not leave a stale trigger
        let conn = open_db(&db_path).unwrap();

        conn.execute(
            "INSERT INTO cash_accounts (id, name, currency, interest_rate, include_in_portfolio, notes, created_at)
             VALUES (1, 'CommSec AUD', 'AUD', 4.35, 1, 'settlement', '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO cash_transactions (id, account_id, date, amount, kind, notes, created_at)
             VALUES (7, 1, '2026-01-02', 1000.0, 'deposit', 'monthly', '2026-01-02T00:00:00Z')",
            [],
        )
        .unwrap();
        conn.execute("UPDATE cash_transactions SET amount = 1500.0 WHERE id = 7", []).unwrap();

        let account_new: String = conn
            .query_row(
                "SELECT new_values FROM audit_log WHERE table_name='cash_accounts' AND action='INSERT'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(account_new.contains("\"currency\":\"AUD\""));
        assert!(account_new.contains("\"interest_rate\":4.35"));
        assert!(account_new.contains("\"include_in_portfolio\":1"));

        let (old_values, new_values): (String, String) = conn
            .query_row(
                "SELECT old_values, new_values FROM audit_log
                  WHERE table_name='cash_transactions' AND action='UPDATE'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert!(old_values.contains("\"amount\":1000.0"), "pre-change amount: {old_values}");
        assert!(new_values.contains("\"amount\":1500.0"), "post-change amount: {new_values}");
        assert!(new_values.contains("\"kind\":\"deposit\""));
    }

    /// A trade records which account funded it, and that link is auditable.
    #[test]
    fn holdings_transactions_record_their_cash_account() {
        let (_file, db_path) = setup_test_db();
        let conn = open_db(&db_path).unwrap();
        conn.execute(
            "INSERT INTO holdings_transactions (id, symbol, transaction_type, date, cash_account_id, created_at)
             VALUES (3, 'BHP.AX', 'purchase', '2026-01-05', 42, 'x')",
            [],
        )
        .unwrap();

        let new_values: String = conn
            .query_row(
                "SELECT new_values FROM audit_log WHERE table_name='holdings_transactions' AND action='INSERT'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(new_values.contains("\"cash_account_id\":42"), "got {new_values}");
    }

    #[test]
    fn fx_pair_symbol_builds_yahoo_pairs() {
        assert_eq!(fx_pair_symbol("usd"), "USDAUD=X");
        assert_eq!(fx_pair_symbol(" GBP "), "GBPAUD=X");
    }

    /// Pairs are derived from the data, so a new foreign holding starts its
    /// rate history without anyone editing a config list.
    #[test]
    fn fx_currencies_come_from_holdings_and_symbol_info() {
        let (_file, db_path) = setup_test_db();
        let conn = open_db(&db_path).unwrap();
        conn.execute_batch(
            "INSERT INTO symbol_info (symbol, currency, updated_at) VALUES ('AAPL', 'USD', 'x');
             INSERT INTO symbol_info (symbol, currency, updated_at) VALUES ('BHP.AX', 'AUD', 'x');
             INSERT INTO symbol_info (symbol, currency, updated_at) VALUES ('NONE.AX', NULL, 'x');
             INSERT INTO holdings_transactions (symbol, transaction_type, date, currency, created_at)
                 VALUES ('SHEL.L', 'purchase', '2026-01-01', 'gbp', 'x');",
        )
        .unwrap();

        // AUD is the base and needs no pair; case and blanks are normalised away
        assert_eq!(fx_currencies_in_use(&conn), vec!["GBP".to_string(), "USD".to_string()]);
    }

    /// FX does not trade at weekends, but holdings still need valuing then, so
    /// the lookup takes the most recent rate on or before the date.
    #[test]
    fn fx_rate_on_reads_stored_rates_as_of_a_date() {
        let (_file, db_path) = setup_test_db();
        let conn = open_db(&db_path).unwrap();
        for (date, close) in [("2026-08-06", 1.50), ("2026-08-07", 1.55)] {
            conn.execute(
                "INSERT INTO prices (symbol, date, close, fetched_at) VALUES ('USDAUD=X', ?1, ?2, 'x')",
                params![date, close],
            )
            .unwrap();
        }

        assert_eq!(fx_rate_on(&conn, "USD", "2026-08-07"), Some(1.55));
        // Saturday and Sunday fall back to Friday's rate
        assert_eq!(fx_rate_on(&conn, "USD", "2026-08-09"), Some(1.55));
        assert_eq!(fx_rate_on(&conn, "USD", "2026-08-06"), Some(1.50));
        // Before any stored rate there is nothing to report
        assert_eq!(fx_rate_on(&conn, "USD", "2026-08-05"), None);
        // AUD is the base
        assert_eq!(fx_rate_on(&conn, "AUD", "2026-08-05"), Some(1.0));
        assert_eq!(fx_rate_on(&conn, "GBP", "2026-08-07"), None);
    }

    /// A 40-week EMA is not a 200-day EMA. The weekly figure steps once a week
    /// from one close per week, so it is far less sensitive to a single day's
    /// move — using daily bars would quietly report a different indicator.
    ///
    /// It must also match `calculateEMA` in the web client, or the Analysis
    /// table and the chart's overlay would disagree for the same symbol.
    #[test]
    fn weekly_ema_collapses_to_one_close_per_week() {
        let (_file, db_path) = setup_test_db();
        {
            let conn = open_db(&db_path).unwrap();
            // Two full weeks. Each week's *last* close is the one that counts,
            // so the EMA(2) seed is the mean of 105 and 205 — not of any
            // mid-week value.
            for (date, close) in [
                ("2026-01-05", 100.0), // Mon
                ("2026-01-07", 101.0),
                ("2026-01-09", 105.0), // Fri — week 1 close
                ("2026-01-12", 200.0), // Mon
                ("2026-01-16", 205.0), // Fri — week 2 close
            ] {
                conn.execute(
                    "INSERT INTO prices (symbol, date, close, fetched_at) VALUES ('TST.AX', ?1, ?2, 'x')",
                    params![date, close],
                )
                .unwrap();
            }
        }
        let conn = open_db(&db_path).unwrap();
        // Exactly the mean of the two week-closing values. A daily EMA(2) over
        // the same five bars would land near 192, so this figure alone rules
        // out the collapse silently going away.
        let ema = stored_weekly_ema(&conn, "TST.AX", 2).expect("two weeks is enough for period 2");
        assert!((ema - 155.0).abs() < 1e-9, "expected the mean of 105 and 205, got {ema}");
    }

    /// A week with only one trading day is still a week.
    #[test]
    fn weekly_ema_handles_short_weeks_and_insufficient_history() {
        let (_file, db_path) = setup_test_db();
        {
            let conn = open_db(&db_path).unwrap();
            for (date, close) in [
                ("2026-01-09", 10.0), // a lone Friday
                ("2026-01-12", 20.0), // Mon
                ("2026-01-13", 30.0), // Tue — week 2 close
            ] {
                conn.execute(
                    "INSERT INTO prices (symbol, date, close, fetched_at) VALUES ('TST.AX', ?1, ?2, 'x')",
                    params![date, close],
                )
                .unwrap();
            }
        }
        let conn = open_db(&db_path).unwrap();
        assert!((stored_weekly_ema(&conn, "TST.AX", 2).unwrap() - 20.0).abs() < 1e-9);
        // Only two weeks exist, so a 40-week average has nothing to report.
        assert_eq!(stored_weekly_ema(&conn, "TST.AX", 40), None);
    }

    /// Bars the OHLC backfill could not reach have a NULL high; those must fall
    /// back to their close rather than dropping out of the window.
    #[actix_web::test]
    async fn high30d_falls_back_to_close_when_high_is_missing() {
        let (_file, db_path) = seed_portfolio_fixture();
        {
            let conn = open_db(&db_path).unwrap();
            // Close-only bar (no high) above every other close in the window
            conn.execute(
                "INSERT INTO prices (symbol, date, close, fetched_at)
                 VALUES ('MAN.AX', '2026-07-03', 14.25, '2026-07-03T00:00:00Z')",
                [],
            )
            .unwrap();
        }

        let body = get_json(&db_path, "/api/portfolio/risk").await;
        let man = find(&body["rows"], "MAN.AX");
        assert!(
            close_to(&man["high30d"], 14.25),
            "close-only bar should still set the high, got {:?}",
            man["high30d"]
        );
    }

    // -------------------------------------------------------------------------
    // Holding-transaction write handlers (P2.1)
    //
    // These guard the source-of-truth ledger: over-sell confirmation,
    // foreign-currency rules, validation-rejects-without-writing, delete
    // recalculation, and the atomic move-from-watchlist.
    // -------------------------------------------------------------------------

    /// Send a write request to the holdings handlers and return (status, body).
    async fn call_write(
        db_path: &std::path::Path,
        method: &str,
        uri: &str,
        body: Option<serde_json::Value>,
    ) -> (actix_web::http::StatusCode, serde_json::Value) {
        let app = actix_web::test::init_service(
            App::new()
                .app_data(web::Data::new(db_path.to_path_buf()))
                .service(add_holding_transaction)
                .service(update_holding_transaction)
                .service(delete_holding_transaction)
                .service(add_holding_from_watchlist)
                .service(get_portfolio_holdings),
        )
        .await;
        let mut req = match method {
            "POST" => actix_web::test::TestRequest::post(),
            "PUT" => actix_web::test::TestRequest::put(),
            "DELETE" => actix_web::test::TestRequest::delete(),
            _ => actix_web::test::TestRequest::get(),
        }
        .uri(uri);
        if let Some(b) = body {
            req = req.set_json(b);
        }
        let resp = actix_web::test::call_service(&app, req.to_request()).await;
        let status = resp.status();
        let bytes = actix_web::test::read_body(resp).await;
        let json = if bytes.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
        };
        (status, json)
    }

    fn purchase_json(symbol: &str, qty: f64, price: f64) -> serde_json::Value {
        serde_json::json!({
            "symbol": symbol, "transaction_type": "purchase",
            "date": "2026-01-05", "quantity": qty, "price": price
        })
    }

    fn tx_count(db_path: &PathBuf, symbol: &str) -> i64 {
        let conn = open_db(db_path).unwrap();
        conn.query_row(
            "SELECT COUNT(*) FROM holdings_transactions WHERE symbol = ?1",
            params![symbol],
            |row| row.get(0),
        )
        .unwrap()
    }

    #[actix_web::test]
    async fn oversell_without_confirm_is_409_and_writes_nothing() {
        let (_file, db_path) = setup_test_db();
        let (status, _) = call_write(&db_path, "POST", "/api/holdings", Some(purchase_json("TST.AX", 10.0, 5.0))).await;
        assert_eq!(status, 200);

        let sale = serde_json::json!({
            "symbol": "TST.AX", "transaction_type": "sale",
            "date": "2026-06-01", "quantity": 15.0, "price": 6.0
        });
        let (status, body) = call_write(&db_path, "POST", "/api/holdings", Some(sale)).await;
        assert_eq!(status, 409);
        assert_eq!(body["error"]["code"], "oversell_confirmation_required");
        assert!(close_to(&body["error"]["held"], 10.0));
        assert_eq!(tx_count(&db_path, "TST.AX"), 1, "the rejected sale must not be written");
    }

    #[actix_web::test]
    async fn oversell_with_confirm_is_recorded() {
        let (_file, db_path) = setup_test_db();
        call_write(&db_path, "POST", "/api/holdings", Some(purchase_json("TST.AX", 10.0, 5.0))).await;

        let sale = serde_json::json!({
            "symbol": "TST.AX", "transaction_type": "sale",
            "date": "2026-06-01", "quantity": 15.0, "price": 6.0, "confirm": true
        });
        let (status, body) = call_write(&db_path, "POST", "/api/holdings", Some(sale)).await;
        assert_eq!(status, 200);
        assert_eq!(body["transaction_type"], "sale");
        assert_eq!(tx_count(&db_path, "TST.AX"), 2);
    }

    #[actix_web::test]
    async fn sale_within_holding_needs_no_confirm() {
        let (_file, db_path) = setup_test_db();
        call_write(&db_path, "POST", "/api/holdings", Some(purchase_json("TST.AX", 10.0, 5.0))).await;

        let sale = serde_json::json!({
            "symbol": "TST.AX", "transaction_type": "sale",
            "date": "2026-06-01", "quantity": 5.0, "price": 6.0
        });
        let (status, _) = call_write(&db_path, "POST", "/api/holdings", Some(sale)).await;
        assert_eq!(status, 200);
    }

    #[actix_web::test]
    async fn invalid_transactions_rejected_with_envelope_and_nothing_written() {
        let (_file, db_path) = setup_test_db();

        for body in [
            purchase_json("TST.AX", 0.0, 5.0),   // zero quantity
            purchase_json("TST.AX", -3.0, 5.0),  // negative quantity
            purchase_json("TST.AX", 10.0, 0.0),  // zero price
            purchase_json("TST.AX", 10.0, -1.0), // negative price
            serde_json::json!({ "symbol": "TST.AX", "transaction_type": "dividend", "date": "2026-01-05", "amount": 0.0 }),
            serde_json::json!({ "symbol": "TST.AX", "transaction_type": "gift", "date": "2026-01-05", "quantity": 1.0, "price": 1.0 }),
            serde_json::json!({ "symbol": "TST.AX", "transaction_type": "purchase", "date": "05/01/2026", "quantity": 1.0, "price": 1.0 }),
        ] {
            let (status, resp) = call_write(&db_path, "POST", "/api/holdings", Some(body.clone())).await;
            assert_eq!(status, 400, "expected 400 for {body}");
            assert_eq!(resp["error"]["code"], "bad_request", "error envelope for {body}");
        }
        assert_eq!(tx_count(&db_path, "TST.AX"), 0, "no rejected payload may be written");
    }

    #[actix_web::test]
    async fn foreign_purchase_without_price_or_original_is_rejected() {
        let (_file, db_path) = setup_test_db();
        let body = serde_json::json!({
            "symbol": "TSM", "transaction_type": "purchase",
            "date": "2026-01-05", "quantity": 10.0, "currency": "USD"
        });
        let (status, resp) = call_write(&db_path, "POST", "/api/holdings", Some(body)).await;
        assert_eq!(status, 400);
        assert!(
            resp["error"]["message"].as_str().unwrap().contains("original_price"),
            "message should say what is missing: {resp}"
        );
        assert_eq!(tx_count(&db_path, "TSM"), 0);
    }

    #[actix_web::test]
    async fn foreign_purchase_with_supplied_fx_persists_native_and_aud() {
        let (_file, db_path) = setup_test_db();
        // The client converts: price (AUD) = original_price (USD) × fx_rate
        let body = serde_json::json!({
            "symbol": "TSM", "transaction_type": "purchase",
            "date": "2026-01-05", "quantity": 10.0,
            "currency": "USD", "original_price": 100.0, "fx_rate": 1.5, "price": 150.0
        });
        let (status, record) = call_write(&db_path, "POST", "/api/holdings", Some(body)).await;
        assert_eq!(status, 200);
        assert_eq!(record["currency"], "USD");
        assert!(close_to(&record["original_price"], 100.0));
        assert!(close_to(&record["fx_rate"], 1.5));
        assert!(close_to(&record["price"], 150.0), "AUD price = native × rate");
    }

    #[actix_web::test]
    async fn delete_recalculates_holdings_and_404s_on_missing_id() {
        let (_file, db_path) = setup_test_db();
        let (_, first) = call_write(&db_path, "POST", "/api/holdings", Some(purchase_json("TST.AX", 10.0, 5.0))).await;
        let (_, second) = call_write(&db_path, "POST", "/api/holdings", Some(purchase_json("TST.AX", 4.0, 6.0))).await;
        // A cached price so the holding shows up in the portfolio
        open_db(&db_path)
            .unwrap()
            .execute(
                "INSERT INTO cached_current_prices (symbol, price, last_updated) VALUES ('TST.AX', 7.0, '2026-07-11T00:00:00Z')",
                [],
            )
            .unwrap();

        let (status, _) = call_write(&db_path, "DELETE", &format!("/api/holdings/{}", first["id"]), None).await;
        assert_eq!(status, 204);
        let body = call_write(&db_path, "GET", "/api/portfolio/holdings", None).await.1;
        assert!(close_to(&find(&body["holdings"], "TST.AX")["shares"], 4.0), "holdings recalculate after delete");

        let (status, _) = call_write(&db_path, "DELETE", &format!("/api/holdings/{}", second["id"]), None).await;
        assert_eq!(status, 204);
        let body = call_write(&db_path, "GET", "/api/portfolio/holdings", None).await.1;
        assert!(body["holdings"].as_array().unwrap().is_empty(), "symbol disappears with its last transaction");

        let (status, resp) = call_write(&db_path, "DELETE", "/api/holdings/99999", None).await;
        assert_eq!(status, 404);
        assert_eq!(resp["error"]["code"], "not_found");
    }

    /// The normal path, so the fix is not just "never deletes anything": the
    /// symbol row goes when its last membership goes, and stays while another
    /// list still holds it.
    #[test]
    fn the_symbol_row_follows_its_last_membership() {
        let (_file, db_path) = setup_test_db();
        seed_watchlist_entry(&db_path, "TST.AX", &["BuyList", "Watching"]);
        let ids: Vec<i64> = {
            let conn = open_db(&db_path).unwrap();
            let mut stmt = conn
                .prepare("SELECT id FROM watchlist_memberships WHERE symbol = 'TST.AX' ORDER BY id")
                .unwrap();
            let v = stmt.query_map([], |r| r.get(0)).unwrap().flatten().collect();
            v
        };

        assert!(remove_watchlist_symbol(&db_path, ids[0]).unwrap());
        assert_eq!(watchlist_rows(&db_path, "TST.AX"), (1, 1), "one list left, so the symbol stays");

        assert!(remove_watchlist_symbol(&db_path, ids[1]).unwrap());
        assert_eq!(watchlist_rows(&db_path, "TST.AX"), (0, 0), "last one gone, so the symbol goes too");
    }

    /// The cleanup delete used to be `let _ = …`, so a failure to remove the
    /// symbol row was reported as success. Dropping `watchlist_symbols` reaches
    /// that exact line: everything before it succeeds, and only the final delete
    /// fails.
    ///
    /// The sibling guard — the remaining-membership count, which used to default
    /// to zero and so read a failed query as "none left" — cannot be isolated the
    /// same way: every query before it reads the same table, so any fault that
    /// reaches the count has already failed the function earlier. It is fixed and
    /// covered by inspection rather than by this test.
    #[test]
    fn a_failed_cleanup_is_reported_rather_than_swallowed() {
        let (_file, db_path) = setup_test_db();
        seed_watchlist_entry(&db_path, "TST.AX", &["BuyList"]);
        let id: i64 = open_db(&db_path)
            .unwrap()
            .query_row("SELECT id FROM watchlist_memberships WHERE symbol = 'TST.AX'", [], |r| r.get(0))
            .unwrap();
        drop_table(&db_path, "watchlist_symbols");

        let result = remove_watchlist_symbol(&db_path, id);
        assert!(result.is_err(), "a cleanup that could not run must not report success");
    }

    fn seed_watchlist_entry(db_path: &PathBuf, symbol: &str, lists: &[&str]) {
        let conn = open_db(db_path).unwrap();
        conn.execute(
            "INSERT INTO watchlist_symbols (symbol, updated_at) VALUES (?1, '2026-01-01T00:00:00Z')",
            params![symbol],
        )
        .unwrap();
        for list in lists {
            conn.execute(
                "INSERT INTO watchlist_memberships (symbol, list_name, added_at) VALUES (?1, ?2, '2026-01-01T00:00:00Z')",
                params![symbol, list],
            )
            .unwrap();
        }
    }

    fn watchlist_rows(db_path: &PathBuf, symbol: &str) -> (i64, i64) {
        let conn = open_db(db_path).unwrap();
        let memberships: i64 = conn
            .query_row("SELECT COUNT(*) FROM watchlist_memberships WHERE symbol = ?1", params![symbol], |r| r.get(0))
            .unwrap();
        let symbols: i64 = conn
            .query_row("SELECT COUNT(*) FROM watchlist_symbols WHERE symbol = ?1", params![symbol], |r| r.get(0))
            .unwrap();
        (memberships, symbols)
    }

    #[actix_web::test]
    async fn from_watchlist_records_transaction_and_removes_memberships() {
        let (_file, db_path) = setup_test_db();
        seed_watchlist_entry(&db_path, "TST.AX", &["Default", "Growth"]);

        let (status, body) =
            call_write(&db_path, "POST", "/api/holdings/from-watchlist", Some(purchase_json("TST.AX", 10.0, 5.0))).await;
        assert_eq!(status, 200);
        assert_eq!(body["removed_memberships"], 2);
        assert_eq!(body["transaction"]["symbol"], "TST.AX");
        assert_eq!(tx_count(&db_path, "TST.AX"), 1);
        assert_eq!(watchlist_rows(&db_path, "TST.AX"), (0, 0), "both watchlist tables cleaned up");
    }

    #[actix_web::test]
    async fn from_watchlist_rejection_leaves_watchlist_untouched() {
        let (_file, db_path) = setup_test_db();
        seed_watchlist_entry(&db_path, "TST.AX", &["Default", "Growth"]);

        let (status, _) =
            call_write(&db_path, "POST", "/api/holdings/from-watchlist", Some(purchase_json("TST.AX", 0.0, 5.0))).await;
        assert_eq!(status, 400);
        assert_eq!(tx_count(&db_path, "TST.AX"), 0, "no transaction on validation failure");
        assert_eq!(watchlist_rows(&db_path, "TST.AX"), (2, 1), "watchlist must survive a failed move");
    }

    // -------------------------------------------------------------------------
    // Watchlist CRUD + two-table invariants (P2.2)
    //
    // The watchlist is a two-table design (watchlist_symbols holds per-symbol
    // data once; watchlist_memberships one row per list). The invariants
    // below are documented in CLAUDE.md and enforced only by this code.
    // The add handler spawns a background Yahoo fetch, so tests exercise
    // insert_watchlist_symbol (the function it wraps) plus the network-free
    // handlers.
    // -------------------------------------------------------------------------

    async fn call_watchlist(
        db_path: &std::path::Path,
        method: &str,
        uri: &str,
        body: Option<serde_json::Value>,
    ) -> (actix_web::http::StatusCode, serde_json::Value) {
        let app = actix_web::test::init_service(
            App::new()
                .app_data(web::Data::new(db_path.to_path_buf()))
                .service(delete_watchlist_symbol)
                .service(update_watchlist_symbol_lists),
        )
        .await;
        let mut req = match method {
            "PUT" => actix_web::test::TestRequest::put(),
            "DELETE" => actix_web::test::TestRequest::delete(),
            _ => actix_web::test::TestRequest::get(),
        }
        .uri(uri);
        if let Some(b) = body {
            req = req.set_json(b);
        }
        let resp = actix_web::test::call_service(&app, req.to_request()).await;
        let status = resp.status();
        let bytes = actix_web::test::read_body(resp).await;
        let json = if bytes.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
        };
        (status, json)
    }

    fn membership_ids(db_path: &PathBuf, symbol: &str) -> Vec<i64> {
        let conn = open_db(db_path).unwrap();
        let mut stmt = conn
            .prepare("SELECT id FROM watchlist_memberships WHERE symbol = ?1 ORDER BY id")
            .unwrap();
        stmt.query_map(params![symbol], |r| r.get(0)).unwrap().flatten().collect()
    }

    fn membership_lists(db_path: &PathBuf, symbol: &str) -> Vec<String> {
        let conn = open_db(db_path).unwrap();
        let mut stmt = conn
            .prepare("SELECT list_name FROM watchlist_memberships WHERE symbol = ?1 ORDER BY list_name")
            .unwrap();
        stmt.query_map(params![symbol], |r| r.get(0)).unwrap().flatten().collect()
    }

    #[actix_web::test]
    async fn deleting_last_membership_removes_symbol_row() {
        let (_file, db_path) = setup_test_db();
        insert_watchlist_symbol(&db_path, "TST.AX", "Default", Some("keep"), None, None, None).unwrap();
        insert_watchlist_symbol(&db_path, "TST.AX", "Growth", None, None, None, None).unwrap();
        assert_eq!(watchlist_rows(&db_path, "TST.AX"), (2, 1));

        let ids = membership_ids(&db_path, "TST.AX");
        let (status, _) = call_watchlist(&db_path, "DELETE", &format!("/api/watchlist/{}", ids[0]), None).await;
        assert_eq!(status, 204);
        assert_eq!(watchlist_rows(&db_path, "TST.AX"), (1, 1), "symbol row survives while a membership remains");

        let (status, _) = call_watchlist(&db_path, "DELETE", &format!("/api/watchlist/{}", ids[1]), None).await;
        assert_eq!(status, 204);
        assert_eq!(watchlist_rows(&db_path, "TST.AX"), (0, 0), "last membership must cascade the symbol row");

        let (status, resp) = call_watchlist(&db_path, "DELETE", "/api/watchlist/99999", None).await;
        assert_eq!(status, 404);
        assert_eq!(resp["error"]["code"], "not_found");
    }

    #[test]
    fn adding_to_second_list_preserves_symbol_data() {
        let (_file, db_path) = setup_test_db();
        insert_watchlist_symbol(&db_path, "TST.AX", "Default", Some("watch me"), Some(5.0), Some(4.0), None).unwrap();
        // Second list, no symbol data supplied — COALESCE must keep the originals
        let row = insert_watchlist_symbol(&db_path, "TST.AX", "Growth", None, None, None, None).unwrap();
        assert_eq!(row.notes.as_deref(), Some("watch me"));
        assert_eq!(row.breakthrough_price, Some(5.0));
        assert_eq!(row.stop_loss_price, Some(4.0));
        assert_eq!(watchlist_rows(&db_path, "TST.AX"), (2, 1), "one symbol row, two memberships");
    }

    #[test]
    fn duplicate_membership_is_idempotent() {
        let (_file, db_path) = setup_test_db();
        insert_watchlist_symbol(&db_path, "TST.AX", "Default", None, None, None, None).unwrap();
        insert_watchlist_symbol(&db_path, "TST.AX", "Default", None, None, None, None).unwrap();
        assert_eq!(watchlist_rows(&db_path, "TST.AX"), (1, 1), "same symbol+list twice must not duplicate");
    }

    #[test]
    fn custom_field_merge_deletes_empty_keeps_absent_upserts_rest() {
        let (_file, db_path) = setup_test_db();
        let conn = open_db(&db_path).unwrap();
        save_custom_fields(&conn, "TST.AX", &sym_fields(&[("target", "10"), ("conviction", "high")])).unwrap();

        // Partial map: empty deletes, absent untouched, non-empty upserts
        save_custom_fields(&conn, "TST.AX", &sym_fields(&[("target", ""), ("thesis", " breakout ")])).unwrap();

        let fields = load_custom_fields(&conn, "TST.AX");
        assert!(!fields.contains_key("target"), "empty value must delete the key");
        assert_eq!(fields.get("conviction").map(String::as_str), Some("high"), "absent key must be untouched");
        assert_eq!(fields.get("thesis").map(String::as_str), Some("breakout"), "values are trimmed on upsert");
    }

    #[test]
    fn update_by_membership_id_has_partial_semantics() {
        let (_file, db_path) = setup_test_db();
        let row = insert_watchlist_symbol(&db_path, "TST.AX", "Default", Some("original"), Some(5.0), Some(4.0), None).unwrap();

        // Absent fields (None) keep current values; explicit Some(None) clears
        let updated = update_watchlist_symbol_notes(&db_path, row.id, Some(Some("edited".into())), None, Some(None), None).unwrap();
        assert_eq!(updated.notes.as_deref(), Some("edited"));
        assert_eq!(updated.breakthrough_price, Some(5.0), "absent field keeps its value");
        assert_eq!(updated.stop_loss_price, None, "explicit null clears the value");

        let err = match update_watchlist_symbol_notes(&db_path, 99999, None, None, None, None) {
            Err(e) => e,
            Ok(_) => panic!("expected an error for an unknown membership id"),
        };
        assert!(err.contains("not found"), "unknown membership id: {err}");
    }

    #[actix_web::test]
    async fn symbol_lists_update_replaces_memberships_transactionally() {
        let (_file, db_path) = setup_test_db();
        insert_watchlist_symbol(&db_path, "TST.AX", "Default", Some("notes"), None, None, None).unwrap();
        insert_watchlist_symbol(&db_path, "TST.AX", "Growth", None, None, None, None).unwrap();
        let conn = open_db(&db_path).unwrap();
        save_custom_fields(&conn, "TST.AX", &sym_fields(&[("target", "10")])).unwrap();
        drop(conn);

        // Lowercase path exercises symbol normalisation; the field map deletes
        // `target`; Default is dropped, Value added, Growth kept.
        let body = serde_json::json!({
            "lists": ["Growth", "Value"],
            "notes": "rewritten",
            "breakthrough_price": 6.5,
            "stop_loss_price": null,
            "custom_fields": { "target": "" }
        });
        let (status, resp) = call_watchlist(&db_path, "PUT", "/api/watchlist/symbol/tst.ax", Some(body)).await;
        assert_eq!(status, 200);

        assert_eq!(membership_lists(&db_path, "TST.AX"), vec!["Growth", "Value"]);
        let returned: Vec<&str> = resp.as_array().unwrap().iter().filter_map(|r| r["list_name"].as_str()).collect();
        assert!(returned.contains(&"Value"), "response reflects the new memberships: {resp}");

        let conn = open_db(&db_path).unwrap();
        let (notes, bp): (Option<String>, Option<f64>) = conn
            .query_row(
                "SELECT notes, breakthrough_price FROM watchlist_symbols WHERE symbol = 'TST.AX'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(notes.as_deref(), Some("rewritten"));
        assert_eq!(bp, Some(6.5));
        assert!(load_custom_fields(&conn, "TST.AX").is_empty(), "empty field value deletes the key");
    }

    #[actix_web::test]
    async fn symbol_lists_update_requires_at_least_one_list() {
        let (_file, db_path) = setup_test_db();
        insert_watchlist_symbol(&db_path, "TST.AX", "Default", None, None, None, None).unwrap();

        let body = serde_json::json!({ "lists": ["  "], "notes": null, "breakthrough_price": null, "stop_loss_price": null });
        let (status, resp) = call_watchlist(&db_path, "PUT", "/api/watchlist/symbol/TST.AX", Some(body)).await;
        assert_eq!(status, 400);
        assert_eq!(resp["error"]["code"], "bad_request");
        assert_eq!(watchlist_rows(&db_path, "TST.AX"), (1, 1), "rejected update must change nothing");
    }

    // -------------------------------------------------------------------------
    // Auth middleware + /api/v1 alias (P2.3)
    //
    // The test app uses the same two wrap_fn layers as main(), in the same
    // registration order (auth first, v1 rewrite second — actix runs the
    // later-registered wrap first, so the alias normalises the path before
    // the auth exemption check sees it).
    // -------------------------------------------------------------------------

    #[test]
    fn constant_time_eq_cases() {
        assert!(constant_time_eq("secret-token", "secret-token"));
        assert!(constant_time_eq("", ""));
        assert!(!constant_time_eq("secret-token", "secret-tokeX"), "same length, different bytes");
        assert!(!constant_time_eq("secret", "secret-token"), "different lengths");
        assert!(!constant_time_eq("secret-token", ""), "empty guess");
    }

    /// Build the middleware stack from main() around real handlers and
    /// return (status, body) for one request.
    async fn call_with_auth(
        db_path: &std::path::Path,
        token: Option<&str>,
        method: &str,
        uri: &str,
        auth_header: Option<&str>,
    ) -> (actix_web::http::StatusCode, serde_json::Value) {
        use actix_web::dev::Service as _;
        let token: Option<String> = token.map(str::to_string);
        let app = actix_web::test::init_service(
            App::new()
                .app_data(web::Data::new(db_path.to_path_buf()))
                .wrap_fn(move |req, srv| {
                    let authorized = is_request_authorized(&req, token.as_deref());
                    let fut = if authorized { Some(srv.call(req)) } else { None };
                    async move {
                        match fut {
                            Some(f) => f.await,
                            None => Err(actix_web::error::InternalError::from_response(
                                "unauthorized",
                                api_error(
                                    actix_web::http::StatusCode::UNAUTHORIZED,
                                    "unauthorized",
                                    "Missing or invalid API token",
                                ),
                            )
                            .into()),
                        }
                    }
                })
                .wrap_fn(|mut req, srv| {
                    rewrite_v1_alias(&mut req);
                    srv.call(req)
                })
                .service(health)
                .service(get_events)
                .service(get_watchlist_lists),
        )
        .await;
        let mut req = match method {
            "OPTIONS" => actix_web::test::TestRequest::with_uri(uri).method(actix_web::http::Method::OPTIONS),
            _ => actix_web::test::TestRequest::get().uri(uri),
        };
        if let Some(h) = auth_header {
            req = req.insert_header(("authorization", h));
        }
        // A denied request surfaces as a service Err (the HttpServer boundary
        // renders it in production) — try_call_service lets the test read it.
        let (status, bytes) = match actix_web::test::try_call_service(&app, req.to_request()).await {
            Ok(resp) => {
                let status = resp.status();
                (status, actix_web::test::read_body(resp).await)
            }
            Err(err) => {
                let resp = err.error_response();
                let status = resp.status();
                let bytes = actix_web::body::to_bytes(resp.into_body())
                    .await
                    .unwrap_or_else(|_| actix_web::web::Bytes::new());
                (status, bytes)
            }
        };
        let json = if bytes.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
        };
        (status, json)
    }

    #[actix_web::test]
    async fn auth_gate_rejects_missing_wrong_and_malformed_tokens() {
        let (_file, db_path) = setup_test_db();
        const TOKEN: Option<&str> = Some("secret-token");

        let (status, body) = call_with_auth(&db_path, TOKEN, "GET", "/api/watchlist/lists", None).await;
        assert_eq!(status, 401, "no header");
        assert_eq!(body["error"]["code"], "unauthorized");

        let (status, _) = call_with_auth(&db_path, TOKEN, "GET", "/api/watchlist/lists", Some("Bearer wrong-token")).await;
        assert_eq!(status, 401, "wrong token");

        let (status, _) = call_with_auth(&db_path, TOKEN, "GET", "/api/watchlist/lists", Some("secret-token")).await;
        assert_eq!(status, 401, "missing Bearer prefix");

        let (status, _) = call_with_auth(&db_path, TOKEN, "GET", "/api/watchlist/lists", Some("Bearer secret-token")).await;
        assert_eq!(status, 200, "correct token");
    }

    #[actix_web::test]
    async fn auth_gate_exemptions_and_disabled_mode() {
        let (_file, db_path) = setup_test_db();
        const TOKEN: Option<&str> = Some("secret-token");

        let (status, body) = call_with_auth(&db_path, TOKEN, "GET", "/api/health", None).await;
        assert_eq!(status, 200, "health is reachable without a token");
        assert_eq!(body["status"], "ok");

        let (status, _) = call_with_auth(&db_path, TOKEN, "GET", "/api/v1/health", None).await;
        assert_eq!(status, 200, "the v1 alias is rewritten before the exemption check");

        let (status, _) = call_with_auth(&db_path, TOKEN, "OPTIONS", "/api/watchlist/lists", None).await;
        assert_ne!(status, 401, "CORS preflights must pass the gate");

        let (status, _) = call_with_auth(&db_path, None, "GET", "/api/watchlist/lists", None).await;
        assert_eq!(status, 200, "no configured token disables auth");
    }

    #[actix_web::test]
    async fn v1_alias_rewrites_path_and_preserves_query() {
        let (_file, db_path) = setup_test_db();
        let _ = insert_event_log(&db_path, "info", "test", "test", None, "first");
        let _ = insert_event_log(&db_path, "info", "test", "test", None, "second");

        let (status, body) = call_with_auth(&db_path, None, "GET", "/api/v1/events?size=1", None).await;
        assert_eq!(status, 200, "v1 path reaches the /api handler");
        assert_eq!(body["items"].as_array().unwrap().len(), 1, "query string survives the rewrite");
        assert!(body["total"].as_i64().unwrap() >= 2);

        let (status, _) = call_with_auth(&db_path, None, "GET", "/api/v1/no-such-endpoint", None).await;
        assert_eq!(status, 404, "unknown v1 path rewrites and then 404s normally");
    }

    // -------------------------------------------------------------------------
    // Price-history supplement heuristics (P3.1)
    // -------------------------------------------------------------------------

    /// 2026-01-05 is a Monday; 2026-01-02 the preceding Friday.
    #[test]
    fn last_expected_trading_day_table() {
        let cases: &[(u32, u32, &str, &str)] = &[
            // Plain weekdays expect the same day's bar regardless of hour
            (6, 0, "TST.AX", "2026-01-06"), // Tuesday 00:00
            (9, 23, "MSFT", "2026-01-09"),  // Friday 23:00
            // Weekends map back to Friday
            (3, 12, "TST.AX", "2026-01-02"), // Saturday
            (4, 12, "MSFT", "2026-01-02"),   // Sunday
            // Monday around the ASX cutoff (07:00 UTC)
            (5, 6, "TST.AX", "2026-01-02"),
            (5, 7, "TST.AX", "2026-01-05"),
            // Monday around the US cutoff (10:00 UTC)
            (5, 9, "MSFT", "2026-01-02"),
            (5, 10, "MSFT", "2026-01-05"),
            // 08:00 UTC Monday: the two markets diverge — this window was
            // the ASX Monday-staleness bug. Suffix match is case-insensitive.
            (5, 8, "tst.ax", "2026-01-05"),
            (5, 8, "MSFT", "2026-01-02"),
        ];
        for (day, hour, symbol, expected) in cases {
            let now = Utc.with_ymd_and_hms(2026, 1, *day, *hour, 0, 0).unwrap();
            assert_eq!(
                last_expected_trading_day(now, symbol),
                *expected,
                "2026-01-{day:02} {hour:02}:00 UTC for {symbol}"
            );
        }
    }

    #[test]
    fn history_check_debounces_within_ttl() {
        assert!(!history_recently_checked("DEB1.TEST"), "unknown symbol is not debounced");
        mark_history_checked("DEB1.TEST");
        assert!(history_recently_checked("DEB1.TEST"), "a fresh mark suppresses refetching");
    }

    #[test]
    fn history_check_expires_after_ttl() {
        // Inject a stale timestamp directly — waiting out the real TTL is not viable
        let stale = std::time::Instant::now() - std::time::Duration::from_secs(HISTORY_CHECK_TTL_SECS + 1);
        HISTORY_CHECKED
            .get_or_init(Default::default)
            .lock()
            .unwrap()
            .insert("DEB2.TEST".to_string(), stale);
        assert!(!history_recently_checked("DEB2.TEST"), "an expired mark allows refetching");
    }

    #[test]
    fn history_check_map_prunes_stale_entries_past_capacity() {
        let stale = std::time::Instant::now() - std::time::Duration::from_secs(HISTORY_CHECK_TTL_SECS + 1);
        {
            let mut map = HISTORY_CHECKED.get_or_init(Default::default).lock().unwrap();
            for i in 0..2100 {
                map.insert(format!("PRUNE{i}.TEST"), stale);
            }
        }
        mark_history_checked("PRUNE-TRIGGER.TEST");
        let map = HISTORY_CHECKED.get_or_init(Default::default).lock().unwrap();
        assert!(map.get("PRUNE0.TEST").is_none(), "stale entries are pruned once the map exceeds capacity");
        assert!(map.get("PRUNE-TRIGGER.TEST").is_some(), "the fresh entry survives the prune");
    }

    // -------------------------------------------------------------------------
    // refresh_all debounce semantics (P3.2)
    // -------------------------------------------------------------------------

    /// The stamp decision: a total failure must NOT stamp (so the next call
    /// retries), success or an empty portfolio must (so clients debounce).
    #[test]
    fn refresh_stamp_decision_table() {
        assert!(!refresh_should_stamp(true, false), "attempted but everything failed → no stamp, retry allowed");
        assert!(refresh_should_stamp(true, true), "attempted and something succeeded → stamp");
        assert!(refresh_should_stamp(false, false), "nothing to do → stamp (an empty portfolio is 'done')");
        assert!(refresh_should_stamp(false, true), "dividends-only work still counts");
    }

    fn last_refresh_stamp(db_path: &PathBuf) -> Option<String> {
        let conn = open_db(db_path).unwrap();
        conn.query_row(
            "SELECT value FROM app_config WHERE key = 'last_full_refresh_at'",
            [],
            |r| r.get(0),
        )
        .optional()
        .unwrap()
    }

    async fn post_refresh(db_path: &std::path::Path, uri: &str) -> serde_json::Value {
        let app = actix_web::test::init_service(
            App::new().app_data(web::Data::new(db_path.to_path_buf())).service(refresh_all),
        )
        .await;
        let req = actix_web::test::TestRequest::post().uri(uri).to_request();
        actix_web::test::call_and_read_body_json(&app, req).await
    }

    /// One sequential scenario: REFRESH_IN_FLIGHT is process-global, so the
    /// debounce cases must not run as parallel tests. The DB is empty (no
    /// watchlist, no holdings), so no request touches the network.
    #[actix_web::test]
    async fn refresh_debounce_scenario() {
        let (_file, db_path) = setup_test_db();

        // Fresh DB: runs, and an empty portfolio still stamps the debounce
        let body = post_refresh(&db_path, "/api/refresh").await;
        assert_eq!(body["skipped"], false, "first call runs: {body}");
        let first_stamp = last_refresh_stamp(&db_path).expect("empty-portfolio run must stamp");
        assert!(
            !REFRESH_IN_FLIGHT.load(std::sync::atomic::Ordering::SeqCst),
            "the in-flight flag must clear after a run"
        );

        // Second call inside the window is debounced and reports the stamp
        let body = post_refresh(&db_path, "/api/refresh").await;
        assert_eq!(body["skipped"], true);
        assert_eq!(body["last_refreshed_at"], first_stamp.as_str());

        // force=true bypasses the debounce
        let body = post_refresh(&db_path, "/api/refresh?force=true").await;
        assert_eq!(body["skipped"], false, "force bypasses the window: {body}");

        // A stale stamp (older than the 600s window) no longer debounces
        upsert_config(&db_path, "last_full_refresh_at", &(Utc::now() - chrono::Duration::seconds(601)).to_rfc3339()).unwrap();
        let body = post_refresh(&db_path, "/api/refresh").await;
        assert_eq!(body["skipped"], false, "expired window runs again: {body}");
        let new_stamp = last_refresh_stamp(&db_path).unwrap();
        assert!(new_stamp > first_stamp, "a completed run re-stamps");

        // While a refresh is in flight, even force is skipped — and the
        // caller is told why
        REFRESH_IN_FLIGHT.store(true, std::sync::atomic::Ordering::SeqCst);
        let body = post_refresh(&db_path, "/api/refresh?force=true").await;
        assert_eq!(body["skipped"], true);
        assert_eq!(body["reason"], "refresh_in_progress");
        REFRESH_IN_FLIGHT.store(false, std::sync::atomic::Ordering::SeqCst);
    }

    #[actix_web::test]
    async fn update_transaction_rewrites_fields() {
        let (_file, db_path) = setup_test_db();
        let (_, created) = call_write(&db_path, "POST", "/api/holdings", Some(purchase_json("TST.AX", 10.0, 5.0))).await;

        let updated = serde_json::json!({
            "symbol": "TST.AX", "transaction_type": "purchase",
            "date": "2026-02-01", "quantity": 12.0, "price": 5.5
        });
        let (status, record) =
            call_write(&db_path, "PUT", &format!("/api/holdings/{}", created["id"]), Some(updated)).await;
        assert_eq!(status, 200);
        assert_eq!(record["date"], "2026-02-01");
        assert!(close_to(&record["quantity"], 12.0));
        assert!(close_to(&record["price"], 5.5));
        assert_eq!(tx_count(&db_path, "TST.AX"), 1, "update must not duplicate the row");
    }

    /// Levels are stored per symbol in that symbol's own currency, and come
    /// back highest first so the chart draws them top to bottom.
    #[actix_web::test]
    async fn chart_levels_round_trip_and_delete() {
        let (_file, db_path) = setup_test_db();
        let app = actix_web::test::init_service(
            App::new()
                .app_data(web::Data::new(db_path.clone()))
                .service(get_chart_drawings)
                .service(add_chart_drawing)
                .service(delete_chart_drawing),
        )
        .await;

        let post = |price: f64, label: &str| {
            actix_web::test::TestRequest::post()
                .uri("/api/chart-drawings/BHP.AX")
                .set_json(serde_json::json!({ "price": price, "label": label }))
                .to_request()
        };
        let _ = actix_web::test::call_service(&app, post(38.5, "support")).await;
        let body: serde_json::Value =
            actix_web::test::call_and_read_body_json(&app, post(45.0, "resistance")).await;
        let drawings = body["drawings"].as_array().unwrap();
        assert_eq!(drawings.len(), 2);
        assert_eq!(drawings[0]["price"], 45.0, "highest level first");
        assert_eq!(drawings[1]["label"], "support");
        assert_eq!(drawings[0]["kind"], "horizontal");

        // Another symbol's chart must not inherit them.
        let req = actix_web::test::TestRequest::get().uri("/api/chart-drawings/RIO.AX").to_request();
        let other: serde_json::Value = actix_web::test::call_and_read_body_json(&app, req).await;
        assert_eq!(other["drawings"].as_array().unwrap().len(), 0);

        let id = drawings[0]["id"].as_i64().unwrap();
        let req = actix_web::test::TestRequest::delete()
            .uri(&format!("/api/chart-drawings/id/{}", id))
            .to_request();
        assert_eq!(actix_web::test::call_service(&app, req).await.status(), 204);

        let req = actix_web::test::TestRequest::get().uri("/api/chart-drawings/BHP.AX").to_request();
        let left: serde_json::Value = actix_web::test::call_and_read_body_json(&app, req).await;
        assert_eq!(left["drawings"].as_array().unwrap().len(), 1);
    }

    /// A trendline is defined by two anchors. A row missing one can be read
    /// back but never drawn — a line with no slope and no end.
    #[actix_web::test]
    async fn a_trendline_needs_both_of_its_anchors() {
        let (_file, db_path) = setup_test_db();
        let app = actix_web::test::init_service(
            App::new()
                .app_data(web::Data::new(db_path.clone()))
                .service(add_chart_drawing)
                .service(get_chart_drawings),
        )
        .await;
        let post = |body: serde_json::Value| {
            actix_web::test::TestRequest::post()
                .uri("/api/chart-drawings/BHP.AX")
                .set_json(body)
                .to_request()
        };

        for (case, body) in [
            ("no end anchor", serde_json::json!({ "kind": "trend", "price": 30.0, "start_date": "2026-01-05" })),
            ("no end price", serde_json::json!({ "kind": "trend", "price": 30.0, "start_date": "2026-01-05", "end_date": "2026-03-05" })),
            ("zero end price", serde_json::json!({ "kind": "trend", "price": 30.0, "start_date": "2026-01-05", "end_date": "2026-03-05", "end_price": 0.0 })),
            ("same date twice", serde_json::json!({ "kind": "trend", "price": 30.0, "start_date": "2026-01-05", "end_date": "2026-01-05", "end_price": 40.0 })),
            ("unknown kind", serde_json::json!({ "kind": "squiggle", "price": 30.0 })),
        ] {
            let status = actix_web::test::call_service(&app, post(body)).await.status();
            assert_eq!(status, 400, "{case} should be refused");
        }

        let good = serde_json::json!({
            "kind": "trend", "price": 30.0, "label": "uptrend",
            "start_date": "2026-01-05", "end_date": "2026-03-05", "end_price": 42.5
        });
        let body: serde_json::Value = actix_web::test::call_and_read_body_json(&app, post(good)).await;
        let d = &body["drawings"][0];
        assert_eq!(d["kind"], "trend");
        assert_eq!(d["start_date"], "2026-01-05");
        assert_eq!(d["end_price"], 42.5);
    }

    /// Horizontal levels predate the anchor columns, so they must still store
    /// and read back with those columns simply absent.
    #[actix_web::test]
    async fn a_horizontal_level_leaves_the_trend_anchors_empty() {
        let (_file, db_path) = setup_test_db();
        let app = actix_web::test::init_service(
            App::new().app_data(web::Data::new(db_path.clone())).service(add_chart_drawing),
        )
        .await;
        let req = actix_web::test::TestRequest::post()
            .uri("/api/chart-drawings/BHP.AX")
            .set_json(serde_json::json!({ "price": 38.5 }))
            .to_request();
        let body: serde_json::Value = actix_web::test::call_and_read_body_json(&app, req).await;
        let d = &body["drawings"][0];
        assert_eq!(d["kind"], "horizontal");
        assert!(d["start_date"].is_null());
        assert!(d["end_price"].is_null());
    }

    /// Dragging a level saves its new price in place: same row, same symbol,
    /// and the audit log records both prices so a mis-drag can be undone.
    #[actix_web::test]
    async fn a_level_moves_to_a_new_price_and_a_trendline_does_not() {
        let (_file, db_path) = setup_test_db();
        let app = actix_web::test::init_service(
            App::new()
                .app_data(web::Data::new(db_path.clone()))
                .service(add_chart_drawing)
                .service(move_chart_drawing),
        )
        .await;
        let add = |body: serde_json::Value| {
            actix_web::test::TestRequest::post().uri("/api/chart-drawings/BHP.AX").set_json(body).to_request()
        };
        let body: serde_json::Value =
            actix_web::test::call_and_read_body_json(&app, add(serde_json::json!({ "price": 38.5, "label": "support" }))).await;
        let id = body["drawings"][0]["id"].as_i64().unwrap();

        let mv = |id: i64, price: f64| {
            actix_web::test::TestRequest::patch()
                .uri(&format!("/api/chart-drawings/id/{id}"))
                .set_json(serde_json::json!({ "price": price }))
                .to_request()
        };
        let body: serde_json::Value = actix_web::test::call_and_read_body_json(&app, mv(id, 41.25)).await;
        let rows = body["drawings"].as_array().unwrap();
        assert_eq!(rows.len(), 1, "moving must not create a second level");
        assert_eq!(rows[0]["id"], id);
        assert_eq!(rows[0]["price"], 41.25);
        assert_eq!(rows[0]["label"], "support", "the label travels with the line");

        let audited: (String, String) = open_db(&db_path).unwrap()
            .query_row(
                "SELECT json_extract(old_values, '$.price'), json_extract(new_values, '$.price')
                   FROM audit_log WHERE table_name = 'chart_drawings' AND action = 'UPDATE'",
                [],
                |r| Ok((r.get::<_, f64>(0)?.to_string(), r.get::<_, f64>(1)?.to_string())),
            )
            .unwrap();
        assert_eq!(audited, ("38.5".to_string(), "41.25".to_string()));

        for bad in [0.0, -1.0] {
            assert_eq!(actix_web::test::call_service(&app, mv(id, bad)).await.status(), 400, "price {bad}");
        }
        assert_eq!(actix_web::test::call_service(&app, mv(9999, 40.0)).await.status(), 404);

        let body: serde_json::Value = actix_web::test::call_and_read_body_json(&app, add(serde_json::json!({
            "kind": "trend", "price": 30.0, "start_date": "2026-01-05", "end_date": "2026-03-05", "end_price": 42.5
        }))).await;
        let trend_id = body["drawings"].as_array().unwrap().iter()
            .find(|d| d["kind"] == "trend").unwrap()["id"].as_i64().unwrap();
        assert_eq!(actix_web::test::call_service(&app, mv(trend_id, 31.0)).await.status(), 400);
    }

    /// A stray drag must not write a level that can never be seen.
    #[actix_web::test]
    async fn a_chart_level_must_be_a_positive_price() {
        let (_file, db_path) = setup_test_db();
        let app = actix_web::test::init_service(
            App::new().app_data(web::Data::new(db_path.clone())).service(add_chart_drawing),
        )
        .await;
        for bad in [0.0, -5.0] {
            let req = actix_web::test::TestRequest::post()
                .uri("/api/chart-drawings/BHP.AX")
                .set_json(serde_json::json!({ "price": bad }))
                .to_request();
            assert_eq!(actix_web::test::call_service(&app, req).await.status(), 400, "price {bad}");
        }
    }

    /// The export carries a running balance, so the order and the arithmetic
    /// have to be settled server-side — a client that re-sorted the rows would
    /// otherwise render a balance column that means nothing.
    #[actix_web::test]
    async fn the_cash_export_runs_a_balance_and_escapes_its_fields() {
        let (_file, db_path) = setup_test_db();
        seed_cash_account(&db_path, 1, "CBA CDIA", "AUD");
        {
            let conn = open_db(&db_path).unwrap();
            conn.execute_batch(
                "INSERT INTO cash_transactions (account_id, date, amount, kind, notes, created_at)
                     VALUES (1, '2026-06-05', 10000.0, 'deposit', 'Opening funds', 'x');
                 INSERT INTO cash_transactions (account_id, date, amount, kind, notes, created_at)
                     VALUES (1, '2026-06-25', -1509.0, 'trade_buy', 'purchase SPCX, at 1.4189', 'x');
                 INSERT INTO cash_transactions (account_id, date, amount, kind, notes, created_at)
                     VALUES (1, '2026-07-01', 11.37, 'interest', NULL, 'x');",
            )
            .unwrap();
        }

        let app = actix_web::test::init_service(
            App::new()
                .app_data(web::Data::new(db_path.clone()))
                .service(export_cash_account_csv),
        )
        .await;
        let req = actix_web::test::TestRequest::get()
            .uri("/api/cash/accounts/1/transactions.csv")
            .to_request();
        let resp = actix_web::test::call_service(&app, req).await;
        assert_eq!(resp.status(), 200);
        let disposition = resp
            .headers()
            .get("Content-Disposition")
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        assert!(disposition.contains("cash-cba-cdia.csv"), "unhelpful filename: {disposition}");

        let body = actix_web::test::read_body(resp).await;
        let text = String::from_utf8(body.to_vec()).unwrap();
        let lines: Vec<&str> = text.lines().collect();

        assert_eq!(lines[0], "Date,Transaction,Description,Amount (AUD),Balance (AUD)");
        assert_eq!(lines[1], "2026-06-05,deposit,Opening funds,10000.00,10000.00");
        // The comma inside the note must not split the row into extra columns.
        assert_eq!(lines[2], "2026-06-25,trade_buy,\"purchase SPCX, at 1.4189\",-1509.00,8491.00");
        assert_eq!(lines[2].matches(',').count() - 1, 4, "the quoted comma is not a delimiter");
        // Balance keeps running across a row with no description.
        assert_eq!(lines[3], "2026-07-01,interest,,11.37,8502.37");
    }

    /// An account that does not exist is a 404, not an empty file that looks
    /// like an account with no transactions.
    #[actix_web::test]
    async fn exporting_an_unknown_cash_account_is_not_found() {
        let (_file, db_path) = setup_test_db();
        let app = actix_web::test::init_service(
            App::new()
                .app_data(web::Data::new(db_path.clone()))
                .service(export_cash_account_csv),
        )
        .await;
        let req = actix_web::test::TestRequest::get()
            .uri("/api/cash/accounts/99/transactions.csv")
            .to_request();
        let resp = actix_web::test::call_service(&app, req).await;
        assert_eq!(resp.status(), 404);
    }

    /// A dividend declared after the holding was sold is not the holder's. The
    /// ledger used to test only "on or after the first purchase", which guarded
    /// the wrong end: those events appeared as rows that could never be
    /// recorded, because there was no entitlement to record.
    #[actix_web::test]
    async fn the_ledger_hides_dividends_from_outside_the_holding_period() {
        let (_file, db_path) = setup_test_db();
        {
            let conn = open_db(&db_path).unwrap();
            conn.execute_batch(
                "INSERT INTO holdings_transactions (symbol, transaction_type, date, quantity, price, currency, created_at)
                     VALUES ('VSO.AX', 'purchase', '2025-01-09', 15.0, 60.0, 'AUD', 'x');
                 INSERT INTO holdings_transactions (symbol, transaction_type, date, quantity, price, currency, created_at)
                     VALUES ('VSO.AX', 'sale', '2026-06-24', 15.0, 70.0, 'AUD', 'x');
                 -- before the purchase, while held, and after the sale
                 INSERT INTO dividend_events (symbol, ex_date, amount, fetched_at) VALUES ('VSO.AX', '2024-07-01', 0.76, 'x');
                 INSERT INTO dividend_events (symbol, ex_date, amount, fetched_at) VALUES ('VSO.AX', '2026-01-02', 1.38, 'x');
                 INSERT INTO dividend_events (symbol, ex_date, amount, fetched_at) VALUES ('VSO.AX', '2026-07-01', 2.19, 'x');",
            )
            .unwrap();
        }

        let app = actix_web::test::init_service(
            App::new()
                .app_data(web::Data::new(db_path.clone()))
                .service(get_transactions_ledger),
        )
        .await;
        let req = actix_web::test::TestRequest::get().uri("/api/transactions/ledger").to_request();
        let body: serde_json::Value = actix_web::test::call_and_read_body_json(&app, req).await;

        let derived: Vec<String> = body["rows"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["symbol"] == "VSO.AX" && r["transaction_type"] == "dividend" && r["id"].is_null())
            .map(|r| r["date"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            derived,
            vec!["2026-01-02".to_string()],
            "only the dividend declared while the shares were held belongs on the ledger"
        );
    }

    /// The ledger builds its rows as hand-written `json!` objects rather than
    /// serialising `HoldingTransaction`, so a field added to the struct reaches
    /// `/api/holdings` and silently skips this endpoint. That is how the
    /// Transactions screen's "Settles from" picker read empty on trades that
    /// were correctly linked in the database.
    #[actix_web::test]
    async fn ledger_rows_carry_the_settlement_account() {
        let (_file, db_path) = setup_test_db();
        {
            let conn = open_db(&db_path).unwrap();
            conn.execute(
                "INSERT INTO cash_accounts (name, currency, include_in_portfolio, created_at)
                 VALUES ('Test Loan', 'AUD', 1, '2026-01-01')",
                [],
            )
            .unwrap();
        }
        let (_, linked) = call_write(&db_path, "POST", "/api/holdings", Some(serde_json::json!({
            "symbol": "TST.AX", "transaction_type": "purchase",
            "date": "2026-01-05", "quantity": 10.0, "price": 5.0,
            "cash_account_id": 1
        }))).await;
        let (_, unlinked) = call_write(&db_path, "POST", "/api/holdings", Some(purchase_json("OTH.AX", 3.0, 2.0))).await;

        let app = actix_web::test::init_service(
            App::new()
                .app_data(web::Data::new(db_path.clone()))
                .service(get_transactions_ledger),
        )
        .await;
        let req = actix_web::test::TestRequest::get().uri("/api/transactions/ledger").to_request();
        let body: serde_json::Value = actix_web::test::call_and_read_body_json(&app, req).await;
        let rows = body["rows"].as_array().expect("ledger returns rows");

        let find = |id: &serde_json::Value| {
            rows.iter().find(|r| r["id"] == *id).unwrap_or_else(|| panic!("row {} missing from ledger", id))
        };
        assert_eq!(
            find(&linked["id"])["cash_account_id"], 1,
            "a linked trade must expose its settlement account to the ledger"
        );
        assert!(
            find(&unlinked["id"])["cash_account_id"].is_null(),
            "an unlinked trade reports null, not a missing key"
        );
        assert!(
            rows.iter().all(|r| r.get("cash_account_id").is_some()),
            "every ledger row must carry the key so the UI can distinguish absent from unset"
        );
    }
}

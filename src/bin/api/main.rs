use actix_cors::Cors;
use actix_web::{delete, get, patch, post, put, web, App, HttpResponse, HttpServer, Responder};
use chrono::{Datelike, NaiveDate, TimeZone, Timelike, Utc};
use reqwest::Client;
use rusqlite::{params, types::Type, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, env, path::{Path, PathBuf}};
use stocks::hindsight;
use stocks::portfolio::{self, PortfolioTx, TxType};

mod market;
mod schema;
mod config;
mod watchlist;
mod quotes;
mod fx;
mod holdings;
mod dividend;
mod refresh;
mod analysis;
mod cash;
mod charts;
mod valuation;
use market::{
    fetch_current_price, fetch_histories, http_client, fetch_price_history, fx_pair_symbol, fx_rate_on,
    fetch_price_history_from_yahoo, persist_price_history, resolve_fx_rates, yahoo_local_date,
    YahooMeta,
};
#[cfg(test)]
use market::{
    history_recently_checked, mark_history_checked, session_range, HISTORY_CHECKED,
    HISTORY_CHECK_TTL_SECS,
};
use schema::init_db;
use config::*;
use watchlist::*;
use quotes::*;
use fx::*;
use holdings::*;
use dividend::*;
use refresh::*;
use analysis::*;
use cash::*;
use charts::*;
use valuation::*;
use stocks::db::{insert_event_log, open_db};

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
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

use stocks::dividends::DividendEvent;
#[cfg(test)]
use stocks::dividends::dedupe as dedupe_dividend_events;

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

/// Run `work` in one transaction on a fresh connection, committing only if it
/// returns `Ok`. On `Err` — or a panic — the transaction is dropped and rolls
/// back, so a write that must be all-or-nothing is written as one closure.
fn with_tx<T>(db_path: &std::path::Path, work: impl FnOnce(&rusqlite::Transaction) -> Result<T, String>) -> Result<T, String> {
    let mut conn = open_db(db_path).map_err(|err| err.to_string())?;
    let tx = conn.transaction().map_err(|err| err.to_string())?;
    let value = work(&tx)?;
    tx.commit().map_err(|err| err.to_string())?;
    Ok(value)
}

/// Collect query rows, failing on the first unreadable one.
///
/// Handlers used to `filter_map(|r| r.ok())`, which dropped a row that could
/// not be read and returned the rest as though the list were complete. An
/// unreadable row is logged and the request fails instead.
fn collect_rows<T>(
    db_path: &std::path::Path,
    event_type: &str,
    rows: impl Iterator<Item = rusqlite::Result<T>>,
) -> Result<Vec<T>, HttpResponse> {
    rows.collect::<rusqlite::Result<Vec<T>>>().map_err(|err| {
        let message = format!("Could not read a row: {}", err);
        let _ = insert_event_log(db_path, "error", event_type, "api", None, &message);
        err_internal(message)
    })
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

#[utoipa::path(get, path = "/api/v1/health", tag = "system", responses((status = 200, description = "Health")))]
#[get("/api/health")]
async fn health() -> impl Responder {
    HttpResponse::Ok().json(HealthResponse { status: "ok" })
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
            // So the web client can read the CSV export's filename from a
            // fetched response.
            .expose_headers(vec![actix_web::http::header::CONTENT_DISPOSITION])
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

/// Today, as a calendar date where the portfolio is held.
///
/// The server runs alongside the user, in Sydney. `Utc::now()` is still the
/// previous day until 10:00 (11:00 in daylight saving), so every "today"
/// taken from it — the value chart's last point, AUD cash balances, the
/// hindsight windows — was a day behind for the first part of each morning.
fn today_local() -> NaiveDate {
    chrono::Local::now().date_naive()
}

fn today_local_str() -> String {
    today_local().format("%Y-%m-%d").to_string()
}

fn normalize_symbol(symbol: &str) -> String {
    symbol.trim().to_uppercase()
}

#[cfg(test)]
mod tests;

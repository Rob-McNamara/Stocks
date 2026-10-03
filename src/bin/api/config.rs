//! App configuration, metadata, the event log and change stamps: the settings
//! every screen reads, and the validation that keeps a stored setting parseable.

use super::*;

#[derive(Serialize)]
pub(crate) struct EventLogEntry {
    pub(crate) id: i64,
    pub(crate) timestamp: String,
    pub(crate) level: String,
    pub(crate) source: String,
    pub(crate) event_type: String,
    pub(crate) symbol: Option<String>,
    pub(crate) details: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct EventQuery {
    pub(crate) page: Option<u32>,
    pub(crate) size: Option<u32>,
    pub(crate) level: Option<String>,
    pub(crate) source: Option<String>,
    pub(crate) event_type: Option<String>,
    pub(crate) symbol: Option<String>,
}

#[derive(Serialize)]
pub(crate) struct ConfigItem {
    pub(crate) key: String,
    pub(crate) value: String,
}

#[derive(Deserialize)]
pub(crate) struct UpdateConfig {
    pub(crate) key: String,
    pub(crate) value: String,
}

/// The vocabulary a dashboard list definition may use. Kept beside the
/// validator rather than inline in the engine so the two cannot drift: a value
/// the engine would silently treat as "matches nothing" is rejected on write.
pub(crate) const LIST_SOURCES: [&str; 3] = ["holdings", "watchlist", "both"];

pub(crate) const LIST_OPERATORS: [&str; 7] = [
    "above", "below", "pct_above", "pct_below", "days_above", "days_below", "volume_cross_pct",
];

pub(crate) const LIST_COMPARES: [&str; 2] = ["price", "volume"];

pub(crate) const LIST_SORTS: [&str; 2] = ["asc", "desc"];

pub(crate) const LIST_INDICATORS: [&str; 3] = ["sma50", "sma150", "ema40w"];

/// Marks a symbol the market no longer trades. One key per symbol, holding the
/// last date it traded; `manual_price_<symbol>` holds what it was worth.
///
/// Two keys rather than one because they answer different questions and are
/// consumed in different places: the price feeds the existing valuation chain
/// untouched, while the date bounds how far forward any "since" window can
/// honestly be read.
pub(crate) const DEAD_SYMBOL_PREFIX: &str = "dead_symbol_";

/// Reject a config value the server would later fail to parse.
///
/// These keys hold JSON that other endpoints read back. Without this a typo is
/// accepted silently and only shows up as an empty dashboard, with nothing
/// pointing at the cause.
pub(crate) fn validate_config_value(key: &str, value: &str) -> Result<(), String> {
    match key {
        "dashboard_custom_lists" => validate_dashboard_custom_lists(value),
        "holdings_custom_fields" | "watchlist_custom_fields" => validate_custom_fields(value),
        PORTFOLIO_HISTORY_START => validate_optional_date(value),
        DIVIDEND_RECORD_FROM => validate_optional_date(value),
        // A delisting date is compared against bar dates as text, so a value in
        // any other shape would order wrongly rather than fail.
        key if key.starts_with(DEAD_SYMBOL_PREFIX) => validate_optional_date(value),
        key if key.starts_with("manual_price_") => validate_optional_price(value),
        _ => Ok(()),
    }
}

/// A date setting that may be cleared. Stored as text and compared as text, so
/// anything but YYYY-MM-DD would silently order wrongly rather than fail.
pub(crate) fn validate_optional_date(value: &str) -> Result<(), String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(());
    }
    NaiveDate::parse_from_str(trimmed, "%Y-%m-%d")
        .map(|_| ())
        .map_err(|_| format!("'{trimmed}' is not a date in YYYY-MM-DD form"))
}

/// A price setting that may be cleared: empty, or a positive number.
pub(crate) fn validate_optional_price(value: &str) -> Result<(), String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(());
    }
    match trimmed.parse::<f64>() {
        Ok(p) if p.is_finite() && p > 0.0 => Ok(()),
        _ => Err(format!("'{trimmed}' is not a positive price")),
    }
}

pub(crate) fn validate_dashboard_custom_lists(value: &str) -> Result<(), String> {
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
pub(crate) fn validate_custom_fields(value: &str) -> Result<(), String> {
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
pub(crate) fn config_json<T: serde::de::DeserializeOwned + Default>(
    db_path: &std::path::Path,
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

#[utoipa::path(get, path = "/api/v1/config", tag = "config", responses((status = 200, description = "Get config")))]
#[get("/api/config")]
pub(crate) async fn get_config(db_path: web::Data<PathBuf>) -> impl Responder {
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
pub(crate) async fn update_config(
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
pub(crate) fn drop_cached_quote(db_path: &PathBuf, symbol: &str) {
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

#[utoipa::path(get, path = "/api/v1/events", tag = "events", responses((status = 200, description = "Get events")))]
#[get("/api/events")]
pub(crate) async fn get_events(db_path: web::Data<PathBuf>, query: web::Query<EventQuery>) -> impl Responder {
    match fetch_event_log(&db_path, &query.into_inner()) {
        Ok((items, total)) => HttpResponse::Ok().json(serde_json::json!({"items": items, "total": total})),
        Err(err) => err_internal(err),
    }
}

pub(crate) fn load_config(db_path: &PathBuf) -> Result<Vec<ConfigItem>, String> {
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

pub(crate) fn upsert_config(db_path: &PathBuf, key: &str, value: &str) -> Result<(), String> {
    let conn = open_db(db_path).map_err(|err| err.to_string())?;
    conn.execute(
        "INSERT INTO app_config (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )
    .map_err(|err| err.to_string())?;
    Ok(())
}

pub(crate) fn fetch_event_log(db_path: &PathBuf, q: &EventQuery) -> Result<(Vec<EventLogEntry>, i64), String> {
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

pub(crate) const DEFAULT_SECTORS_JSON: &str = r#"["Energy","Materials","Industrials","Consumer Discretionary","Consumer Staples","Health Care","Financials","Information Technology","Communication Services","Utilities","Real Estate","Others"]"#;

/// Earliest date the portfolio value chart will report, empty for no floor.
///
/// A holding cannot be valued before its first stored price bar, so the years
/// before that are drawn with the stock line flat at zero — cash alone, dressed
/// up as portfolio history. The floor cuts that stretch off rather than
/// inviting it to be read as a real drawdown.
pub(crate) const PORTFOLIO_HISTORY_START: &str = "portfolio_history_start";

/// Symbol-level keys for the profit/loss baseline. `_price` is derived from
/// `_date` when the date is saved, and both live in `holdings_symbol_fields`,
/// which is already audited — so this needs no schema change and no new
/// triggers.
pub(crate) const PL_BASIS_DATE: &str = "pl_basis_date";

pub(crate) const PL_BASIS_PRICE: &str = "pl_basis_price";

pub(crate) const SUPPORTED_CURRENCIES: [&str; 9] = ["AUD", "USD", "GBP", "EUR", "JPY", "CAD", "HKD", "SGD", "NZD"];

/// Cheap change-detection for polling clients: last-modified stamps per data
/// domain, sourced from the audit log (every tracked table has triggers) and
/// the price-refresh timestamps. A mobile app polls this one tiny endpoint
/// and refetches a domain's payload only when its stamp moves.
#[utoipa::path(get, path = "/api/v1/sync-state", tag = "system", responses((status = 200, description = "Get sync state")))]
#[get("/api/sync-state")]
pub(crate) async fn get_sync_state(db_path: web::Data<PathBuf>) -> impl Responder {
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
        // dividend_events is no longer audited (it is fetched data), and its
        // rows are only rewritten when a fetch changes them, so the newest
        // fetched_at is the change stamp.
        "dividends": open_db(db_path.as_ref())
            .ok()
            .and_then(|conn| conn.query_row("SELECT MAX(fetched_at) FROM dividend_events", [], |r| r.get::<_, Option<String>>(0)).ok())
            .flatten(),
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
pub(crate) async fn get_meta(db_path: web::Data<PathBuf>) -> impl Responder {
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

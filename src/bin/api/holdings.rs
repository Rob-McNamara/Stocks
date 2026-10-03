//! Holding transactions: recording, editing and deleting trades and dividends,
//! renaming a symbol, per-symbol fields, and the merged transactions ledger.

use super::*;

#[derive(Serialize, Clone)]
pub(crate) struct HoldingTransaction {
    pub(crate) id: i64,
    pub(crate) symbol: String,
    pub(crate) transaction_type: String,
    pub(crate) date: String,
    pub(crate) quantity: Option<f64>,
    pub(crate) price: Option<f64>,
    pub(crate) amount: Option<f64>,
    pub(crate) brokerage: Option<f64>,
    pub(crate) notes: Option<String>,
    pub(crate) created_at: String,
    #[serde(default)]
    pub(crate) dividends_total: f64,
    pub(crate) currency: String,
    pub(crate) original_price: Option<f64>,
    pub(crate) fx_rate: Option<f64>,
    /// Cash account this trade settles against, when it has one.
    pub(crate) cash_account_id: Option<i64>,
    #[serde(default)]
    pub(crate) custom_fields: std::collections::HashMap<String, String>,
}

#[derive(Deserialize)]
pub(crate) struct NewHoldingTransaction {
    pub(crate) symbol: String,
    pub(crate) transaction_type: String,
    pub(crate) date: String,
    pub(crate) quantity: Option<f64>,
    pub(crate) price: Option<f64>,
    pub(crate) amount: Option<f64>,
    pub(crate) brokerage: Option<f64>,
    pub(crate) notes: Option<String>,
    pub(crate) currency: Option<String>,
    pub(crate) original_price: Option<f64>,
    pub(crate) fx_rate: Option<f64>,
    pub(crate) custom_fields: Option<std::collections::HashMap<String, String>>,
    /// Cash account this trade settles against. Omit to record the trade
    /// without touching the cash ledger, as every pre-ledger trade does.
    pub(crate) cash_account_id: Option<i64>,
    /// Tax withheld from this payment, overriding the symbol's standing rate.
    ///
    /// On an edit: absent keeps the stored amount, `null` clears it — so a
    /// TFN-withheld payment can be corrected back to none.
    #[serde(default, deserialize_with = "deserialize_explicit_null")]
    pub(crate) withholding_amount: Option<Option<f64>>,
    /// Set true to record a sale of more shares than currently held
    /// (the API responds 409 with a warning otherwise).
    pub(crate) confirm: Option<bool>,
}

/// Which over-sell check a holding write needs.
#[derive(Clone, Copy)]
pub(crate) enum OversellCheck {
    NewTrade,
    /// Editing transaction `id`, whose current quantity is being replaced.
    Editing(i64),
}

/// Shared pre-processing for holding create/update:
/// - Server-side FX: a foreign-currency payload may send just `original_price`
///   and `currency`; the AUD price and rate are resolved here so thin clients
///   never do currency math.
/// - Over-sell guard: selling more than held returns 409 unless
///   `confirm: true` is supplied. Applies to edits too — raising a sale's
///   quantity used to oversell without a word.
pub(crate) async fn prepare_holding_payload(
    db_path: &PathBuf,
    symbol: &str,
    payload: &mut NewHoldingTransaction,
    oversell: OversellCheck,
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

    if payload.transaction_type == "sale" && payload.confirm != Some(true) {
        // On an edit the sale being edited is left out: its old quantity is
        // what the new one replaces, not shares already gone.
        let excluding = match oversell {
            OversellCheck::NewTrade => None,
            OversellCheck::Editing(id) => Some(id),
        };
        // A failed read used to count as zero shares held, so every sale was
        // refused with a misleading over-sell warning. It is an error.
        let held: f64 = match open_db(db_path).and_then(|conn| {
            conn.query_row(
                "SELECT COALESCE(SUM(CASE WHEN transaction_type = 'purchase' THEN quantity ELSE -quantity END), 0)
                 FROM holdings_transactions
                 WHERE symbol = ?1 AND transaction_type IN ('purchase', 'sale')
                   AND (?2 IS NULL OR id <> ?2)",
                params![symbol, excluding],
                |row| row.get(0),
            )
        }) {
            Ok(held) => held,
            Err(err) => {
                let message = format!("Could not read shares held for {}: {}", symbol, err);
                let _ = insert_event_log(db_path, "error", "holding_oversell_check", "api", Some(symbol), &message);
                return Err(err_internal(message));
            }
        };
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

#[utoipa::path(get, path = "/api/v1/holdings", tag = "holdings", responses((status = 200, description = "Get holdings")))]
#[get("/api/holdings")]
pub(crate) async fn get_holdings(db_path: web::Data<PathBuf>) -> impl Responder {
    match fetch_holdings(&db_path) {
        Ok(history) => HttpResponse::Ok().json(history),
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "holdings_fetch", "api", None, &err);
            err_internal(err)
        }
    }
}

#[utoipa::path(post, path = "/api/v1/holdings", tag = "holdings", responses((status = 200, description = "Add holding transaction")))]
#[post("/api/holdings")]
pub(crate) async fn add_holding_transaction(
    db_path: web::Data<PathBuf>,
    payload: web::Json<NewHoldingTransaction>,
) -> impl Responder {
    let mut payload = payload.into_inner();
    let symbol = normalize_symbol(&payload.symbol);
    if let Err(response) = prepare_holding_payload(&db_path, &symbol, &mut payload, OversellCheck::NewTrade).await {
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
pub(crate) async fn update_holding_transaction(
    db_path: web::Data<PathBuf>,
    path: web::Path<i64>,
    payload: web::Json<NewHoldingTransaction>,
) -> impl Responder {
    let id = path.into_inner();
    let mut payload = payload.into_inner();
    let symbol = normalize_symbol(&payload.symbol);
    if let Err(response) = prepare_holding_payload(&db_path, &symbol, &mut payload, OversellCheck::Editing(id)).await {
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
pub(crate) async fn delete_holding_transaction(
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
pub(crate) struct RenameHoldingSymbol {
    pub(crate) new_symbol: String,
}

// Two-segment path so it can never collide with `PUT /api/holdings/{id}`
// (which would otherwise try to parse "rename-symbol" as an i64).
#[utoipa::path(put, path = "/api/v1/holdings/rename-symbol/{old_symbol}", tag = "holdings", params(("old_symbol" = String, Path, description = "old_symbol")), responses((status = 200, description = "Rename holding symbol")))]
#[put("/api/holdings/rename-symbol/{old_symbol}")]
pub(crate) async fn rename_holding_symbol(
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

/// Every table that keys rows by ticker, and so has to follow a symbol when it
/// is renamed.
///
/// `event_log` is deliberately absent. It records what happened at the time,
/// under the name in use then; rewriting it would falsify the history it exists
/// to preserve. That is the same reasoning that keeps the log tables out of the
/// audit triggers — see the Audit Logging section of CLAUDE.md.
pub(crate) const SYMBOL_KEYED_TABLES: &[&str] = &[
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

/// app_config keys that end in a symbol, and so follow it on a rename.
pub(crate) const SYMBOL_KEYED_CONFIG_PREFIXES: &[&str] = &["manual_price_", DEAD_SYMBOL_PREFIX, "instrument_type_"];

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
pub(crate) fn migrate_symbol_rows(tx: &Connection, old_symbol: &str, new_symbol: &str) -> Result<usize, String> {
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

    // Settings keyed by symbol name in app_config. Left behind, a renamed
    // holding lost its manual valuation and its delisted marker — so the
    // refresh started asking Yahoo for it again — and an ETF override stopped
    // applying. Same rule as the tables: the new name's own setting wins.
    for prefix in SYMBOL_KEYED_CONFIG_PREFIXES {
        tx.execute(
            "UPDATE OR IGNORE app_config SET key = ?1 WHERE key = ?2",
            params![format!("{prefix}{new_symbol}"), format!("{prefix}{old_symbol}")],
        )
        .map_err(|e| format!("Renaming {}{} in app_config: {}", prefix, old_symbol, e))?;
        tx.execute("DELETE FROM app_config WHERE key = ?1", params![format!("{prefix}{old_symbol}")])
            .map_err(|e| format!("Clearing {}{} from app_config: {}", prefix, old_symbol, e))?;
    }
    Ok(holdings_moved)
}

/// Rename a holding's symbol across all of its transactions and symbol-level
/// metadata, in a single transaction. Per-transaction custom fields are keyed
/// by transaction id, so they follow automatically. Returns the number of
/// holdings_transactions rows updated.
pub(crate) fn rename_holdings_symbol(db_path: &Path, old_symbol: &str, new_symbol: &str) -> Result<usize, String> {
    with_tx(db_path, |tx| migrate_symbol_rows(tx, old_symbol, new_symbol))
}

#[derive(Deserialize)]
pub(crate) struct HoldingsSymbolFieldsPayload {
    pub(crate) notes: Option<String>,
    pub(crate) custom_fields: Option<std::collections::HashMap<String, String>>,
}

#[utoipa::path(put, path = "/api/v1/holdings/symbol-fields/{symbol}", tag = "holdings", params(("symbol" = String, Path, description = "symbol")), responses((status = 200, description = "Update holdings symbol fields")))]
#[put("/api/holdings/symbol-fields/{symbol}")]
pub(crate) async fn update_holdings_symbol_fields(
    db_path: web::Data<PathBuf>,
    path: web::Path<String>,
    payload: web::Json<HoldingsSymbolFieldsPayload>,
) -> impl Responder {
    let symbol = normalize_symbol(&path.into_inner());
    let db = match open_db(db_path.as_ref()) {
        Ok(c) => c,
        Err(err) => return err_internal(err.to_string()),
    };

    // The baseline price is resolved once, here, rather than derived on every
    // read: stored history is finite and gets trimmed, and a basis that
    // silently moves is not a record of anything. Clearing the date clears the
    // price with it.
    //
    // Resolved *before* anything is written. The fields used to be saved
    // first, so a date with no stored price was refused with a 422 after the
    // new date was already stored — beside the previous date's price.
    let mut fields = payload.custom_fields.clone();
    if let Some(fields) = fields.as_mut()
        && let Some(date) = fields.get(PL_BASIS_DATE).map(|d| d.trim().to_string())
    {
        let resolved = if date.is_empty() {
            String::new()
        } else {
            match close_on_or_after(&db, &symbol, &date) {
                Some(close) => close.to_string(),
                None => {
                    let message = format!("No stored price for {} on or after {}", symbol, date);
                    let _ = insert_event_log(&db_path, "warn", "holdings_symbol_fields_update", "api", Some(&symbol), &message);
                    return err_unprocessable(message);
                }
            }
        };
        fields.insert(PL_BASIS_PRICE.to_string(), resolved);
    }

    // Notes and fields commit together, so a failure part-way leaves neither.
    let result = with_tx(&db_path, |tx| {
        if let Some(ref notes) = payload.notes {
            tx.execute(
                "INSERT OR REPLACE INTO holdings_symbol_fields (symbol, field_key, value) VALUES (?1, '_notes', ?2)",
                params![symbol, notes],
            )
            .map_err(|e| format!("Failed to save notes: {}", e))?;
        }
        if let Some(ref fields) = fields {
            upsert_holdings_symbol_fields(tx, &symbol, fields)?;
        }
        Ok(())
    });
    if let Err(err) = result {
        let _ = insert_event_log(&db_path, "error", "holdings_symbol_fields_update", "api", Some(&symbol), &err);
        return err_internal(err);
    }
    HttpResponse::Ok().json("ok")
}

#[utoipa::path(get, path = "/api/v1/holdings/symbol-fields", tag = "holdings", responses((status = 200, description = "Get holdings symbol fields")))]
#[get("/api/holdings/symbol-fields")]
pub(crate) async fn get_holdings_symbol_fields(db_path: web::Data<PathBuf>) -> impl Responder {
    match load_holdings_symbol_fields(&db_path) {
        Ok(fields) => HttpResponse::Ok().json(fields),
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "holdings_symbol_fields_fetch", "api", None, &err);
            err_internal(err)
        }
    }
}

/// Move a watchlist stock into holdings atomically: record the transaction,
/// then remove every watchlist membership for the symbol. Replaces the
/// multi-request handshake the browser used to orchestrate.
#[utoipa::path(post, path = "/api/v1/holdings/from-watchlist", tag = "holdings", responses((status = 200, description = "Add holding from watchlist")))]
#[post("/api/holdings/from-watchlist")]
pub(crate) async fn add_holding_from_watchlist(
    db_path: web::Data<PathBuf>,
    payload: web::Json<NewHoldingTransaction>,
) -> impl Responder {
    let mut payload = payload.into_inner();
    let symbol = normalize_symbol(&payload.symbol);
    if let Err(response) = prepare_holding_payload(&db_path, &symbol, &mut payload, OversellCheck::NewTrade).await {
        return response;
    }
    let record = match insert_holding_transaction(&db_path, &symbol, payload) {
        Ok(record) => record,
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "holding_create", "api", Some(&symbol), &err);
            return err_bad_request(err);
        }
    };

    let removed = with_tx(&db_path, |tx| {
        let n = tx
            .execute("DELETE FROM watchlist_memberships WHERE symbol = ?1", params![symbol])
            .map_err(|e| e.to_string())?;
        tx.execute("DELETE FROM watchlist_symbols WHERE symbol = ?1", params![symbol])
            .map_err(|e| e.to_string())?;
        Ok(n)
    });

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

/// Unified transaction ledger: manual transactions merged with fetched
/// dividend events — deduped by (symbol, date), events filtered to on/after
/// the symbol's first purchase, sorted newest-first. Replaces the merge the
/// Transactions screen performed client-side.
#[utoipa::path(get, path = "/api/v1/transactions/ledger", tag = "transactions", responses((status = 200, description = "Get transactions ledger")))]
#[get("/api/transactions/ledger")]
pub(crate) async fn get_transactions_ledger(db_path: web::Data<PathBuf>) -> impl Responder {
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
        // Only from where recording starts: events before it are counted in
        // the holdings' dividend totals, but listing decades of them here as
        // unrecorded rows would bury the ledger in rows nobody can act on.
        let floor = dividend_record_floor(&conn);
        let mut stmt = conn
            .prepare(
                "SELECT e.symbol, e.ex_date, e.payment_date, e.amount,
                        COALESCE(NULLIF(UPPER(TRIM(si.currency)), ''),
                                 (SELECT UPPER(TRIM(t.currency)) FROM holdings_transactions t
                                   WHERE t.symbol = e.symbol AND UPPER(TRIM(t.currency)) NOT IN ('', 'AUD') LIMIT 1),
                                 'AUD')
                   FROM dividend_events e
                   LEFT JOIN symbol_info si ON si.symbol = e.symbol
                  WHERE e.ex_date >= ?1
                  ORDER BY e.ex_date DESC",
            )
            .map_err(|e| e.to_string())?;
        let event_rows = stmt
            .query_map(params![floor], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, Option<String>>(2)?, row.get::<_, f64>(3)?, row.get::<_, String>(4)?))
            })
            .map_err(|e| e.to_string())?;
        event_rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
    })();
    match events_result {
        Ok(events) => {
            for (symbol, ex_date, payment_date, amount, currency) in events {
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
                    // The per-share amount is in the listing currency — US
                    // dollars for a US stock — so it is labelled as such
                    // rather than passed off as AUD.
                    "currency": currency,
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

/// The columns `HoldingTransaction::from_row` reads, in its order.
pub(crate) const HOLDING_COLUMNS: &str = "id, symbol, transaction_type, date, quantity, price, amount, brokerage, notes, created_at, currency, original_price, fx_rate, cash_account_id";

impl HoldingTransaction {
    /// One row selected with `HOLDING_COLUMNS`. The one place the mapping is
    /// written: it used to be repeated in three queries, so a column added to
    /// one could quietly be missing from the others.
    pub(crate) fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Self> {
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
    }
}

/// One stored transaction by id, without its custom fields.
pub(crate) fn load_holding_transaction(conn: &Connection, id: i64) -> Result<HoldingTransaction, String> {
    conn.query_row(
        &format!("SELECT {HOLDING_COLUMNS} FROM holdings_transactions WHERE id = ?1"),
        params![id],
        HoldingTransaction::from_row,
    )
    .optional()
    .map_err(|err| err.to_string())?
    .ok_or_else(|| format!("Transaction {} not found", id))
}

pub(crate) fn fetch_holdings(db_path: &PathBuf) -> Result<Vec<HoldingTransaction>, String> {
    let conn = open_db(db_path).map_err(|err| err.to_string())?;
    let mut stmt = conn
        .prepare(&format!("SELECT {HOLDING_COLUMNS} FROM holdings_transactions ORDER BY date DESC, id DESC"))
        .map_err(|err| err.to_string())?;

    let rows = stmt
        .query_map([], HoldingTransaction::from_row)
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

pub(crate) fn insert_holding_transaction(
    db_path: &Path,
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

    // The row, its settlement leg and its custom fields commit together. Saved
    // one by one, a leg that failed (an account in the wrong currency, say)
    // returned an error *after* the trade was already stored, so the user
    // fixed the account, saved again, and got the trade twice.
    with_tx(db_path, |conn| {
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
                transaction.withholding_amount.flatten(),
            ],
        )
        .map_err(|err| err.to_string())?;

        let id = conn.last_insert_rowid();
        sync_trade_cash_leg(conn, id)?;

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
            upsert_holdings_symbol_fields(conn, symbol, fields)?;
        }

        let mut custom_fields = std::collections::HashMap::new();
        if let Some(fields) = transaction.custom_fields {
            for (k, v) in fields {
                if !v.is_empty() { custom_fields.insert(k, v); }
            }
        }

        let mut result = load_holding_transaction(conn, id)?;
        result.custom_fields = custom_fields;
        Ok(result)
    })
}

/// The figures that define what a stored transaction *is*, read before an
/// edit is applied.
pub(crate) struct StoredTradeFigures {
    pub(crate) currency: String,
    pub(crate) quantity: Option<f64>,
    pub(crate) price: Option<f64>,
    pub(crate) original_price: Option<f64>,
    pub(crate) fx_rate: Option<f64>,
    pub(crate) amount: Option<f64>,
}

/// Fill in what an edit did not send from the stored row.
///
/// An edit is a change to the fields it names. The Transactions form only sends
/// quantity, price and currency for trades, so editing a dividend's note or
/// account used to null its share count and per-share price, and the missing
/// currency defaulted to AUD — a USD dividend silently became an AUD one with
/// no rate. Absent now means "unchanged":
///
/// - `currency`, `quantity` and `price` keep their stored values.
/// - `original_price` and `fx_rate` only make sense in a foreign currency, so
///   they carry over while the currency is unchanged and foreign, and are
///   dropped when the edit moves the transaction to AUD.
/// - A dividend whose total was edited but whose per-share figures were not
///   has those figures scaled to match, so the share count survives and the
///   native total the cash leg settles from agrees with the new amount.
pub(crate) fn keep_unsent_trade_figures(tx: &mut NewHoldingTransaction, stored: &StoredTradeFigures) {
    let currency = tx.currency.clone().unwrap_or_else(|| stored.currency.clone());
    let same_foreign = currency != "AUD" && currency.eq_ignore_ascii_case(&stored.currency);
    let unit_price_sent = tx.price.is_some() || tx.original_price.is_some();

    if tx.quantity.is_none() {
        tx.quantity = stored.quantity;
    }
    if tx.price.is_none() {
        tx.price = stored.price;
    }
    if same_foreign {
        if tx.original_price.is_none() {
            tx.original_price = stored.original_price;
        }
        if tx.fx_rate.is_none() {
            tx.fx_rate = stored.fx_rate;
        }
    }

    if tx.transaction_type == "dividend" && !unit_price_sent
        && let (Some(new_total), Some(old_total)) = (tx.amount, stored.amount)
        && old_total > 0.0
        && (new_total - old_total).abs() > 1e-9
    {
        let scale = new_total / old_total;
        tx.price = tx.price.map(|p| p * scale);
        tx.original_price = tx.original_price.map(|p| p * scale);
    }
    tx.currency = Some(currency);
}

pub(crate) fn modify_holding_transaction(
    db_path: &Path,
    id: i64,
    symbol: &str,
    mut transaction: NewHoldingTransaction,
) -> Result<HoldingTransaction, String> {
    let parsed_date = NaiveDate::parse_from_str(&transaction.date, "%Y-%m-%d")
        .map_err(|_| "Invalid date format. Use YYYY-MM-DD.".to_string())?;

    // As on create: the edit, its rewritten settlement leg and its custom
    // fields commit together. Otherwise a leg that failed left the edit stored
    // and the old leg already deleted, so the ledger silently lost it.
    with_tx(db_path, |conn| {

        let stored = conn
            .query_row(
                "SELECT currency, quantity, price, original_price, fx_rate, amount FROM holdings_transactions WHERE id = ?1",
                params![id],
                |r| {
                    Ok(StoredTradeFigures {
                        currency: r.get::<_, Option<String>>(0)?.unwrap_or_else(|| "AUD".to_string()),
                        quantity: r.get(1)?,
                        price: r.get(2)?,
                        original_price: r.get(3)?,
                        fx_rate: r.get(4)?,
                        amount: r.get(5)?,
                    })
                },
            )
            .optional()
            .map_err(|err| err.to_string())?
            .ok_or_else(|| format!("Transaction {} not found", id))?;
        keep_unsent_trade_figures(&mut transaction, &stored);

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

        let currency = transaction.currency.as_deref().unwrap_or("AUD");
        conn.execute(
            "UPDATE holdings_transactions SET symbol = ?1, transaction_type = ?2, date = ?3, quantity = ?4, price = ?5, amount = ?6, brokerage = ?7, notes = ?8, currency = ?9, original_price = ?10, fx_rate = ?11, cash_account_id = ?13, withholding_amount = CASE WHEN ?15 THEN ?14 ELSE withholding_amount END WHERE id = ?12",
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
                transaction.withholding_amount.flatten(),
                transaction.withholding_amount.is_some(),
            ],
        )
        .map_err(|err| err.to_string())?;

        // Rewrite the settlement leg from the row as just stored: an edited price,
        // quantity or account has to flow through, and clearing the account removes
        // the leg entirely.
        sync_trade_cash_leg(conn, id)?;

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
            upsert_holdings_symbol_fields(conn, symbol, fields)?;
        }

        let mut result = load_holding_transaction(conn, id)?;
        result.custom_fields = custom_fields;
        Ok(result)
    })
}

pub(crate) fn remove_holding_transaction(db_path: &Path, id: i64) -> Result<bool, String> {
    // The trade, its cash legs, its custom fields and any exclusion go
    // together or not at all.
    with_tx(db_path, |conn| {
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
    })
}

pub(crate) fn load_holdings_symbol_fields(db_path: &PathBuf) -> Result<std::collections::HashMap<String, std::collections::HashMap<String, String>>, String> {
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
pub(crate) fn close_on_or_after(conn: &Connection, symbol: &str, date: &str) -> Option<f64> {
    conn.query_row(
        "SELECT close FROM prices
          WHERE symbol = ?1 AND date >= ?2 AND close IS NOT NULL
          ORDER BY date LIMIT 1",
        params![symbol, date],
        |row| row.get::<_, f64>(0),
    )
    .ok()
}

pub(crate) fn upsert_holdings_symbol_fields(conn: &Connection, symbol: &str, fields: &std::collections::HashMap<String, String>) -> Result<(), String> {
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

pub(crate) fn to_portfolio_txs(rows: &[HoldingTransaction]) -> Vec<PortfolioTx> {
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

//! Watchlist endpoints and storage: the two normalised tables
//! (`watchlist_symbols`, `watchlist_memberships`), their custom fields, and the
//! enriched view with prices and indicators.

use super::*;

#[derive(Serialize)]
pub(crate) struct WatchlistSymbol {
    pub(crate) id: i64,
    pub(crate) symbol: String,
    pub(crate) list_name: String,
    pub(crate) added_at: String,
    pub(crate) notes: Option<String>,
    pub(crate) breakthrough_price: Option<f64>,
    pub(crate) stop_loss_price: Option<f64>,
    pub(crate) custom_fields: std::collections::HashMap<String, String>,
}

#[derive(Deserialize)]
pub(crate) struct AddWatchlistSymbol {
    pub(crate) symbol: String,
    pub(crate) list_name: Option<String>,
    pub(crate) notes: Option<String>,
    pub(crate) breakthrough_price: Option<f64>,
    pub(crate) stop_loss_price: Option<f64>,
    pub(crate) custom_fields: Option<std::collections::HashMap<String, String>>,
}

#[derive(Deserialize)]
pub(crate) struct UpdateWatchlistSymbol {
    #[serde(default, deserialize_with = "deserialize_explicit_null")]
    pub(crate) notes: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_explicit_null")]
    pub(crate) breakthrough_price: Option<Option<f64>>,
    #[serde(default, deserialize_with = "deserialize_explicit_null")]
    pub(crate) stop_loss_price: Option<Option<f64>>,
    pub(crate) custom_fields: Option<std::collections::HashMap<String, String>>,
}

#[derive(Deserialize)]
pub(crate) struct WatchlistQuery {
    pub(crate) list: Option<String>,
}

#[utoipa::path(get, path = "/api/v1/watchlist", tag = "watchlist", responses((status = 200, description = "Get watchlist")))]
#[get("/api/watchlist")]
pub(crate) async fn get_watchlist(db_path: web::Data<PathBuf>, query: web::Query<WatchlistQuery>) -> impl Responder {
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
pub(crate) async fn get_watchlist_lists(db_path: web::Data<PathBuf>) -> impl Responder {
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
pub(crate) async fn add_watchlist_symbol(
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
                // Failures here were silent, and a missing symbol_info row is
                // what leaves a stock without its currency and full name.
                let stored = match fetch_current_price(http_client(), &sym_clone).await {
                    Ok(meta) => store_symbol_info(
                        &db_path_clone,
                        &sym_clone,
                        meta.instrument_type.as_deref(),
                        meta.long_name.as_deref(),
                        meta.currency.as_deref(),
                    ),
                    Err(err) => Err(format!("Quote fetch failed: {}", err)),
                };
                if let Err(err) = stored {
                    let _ = insert_event_log(&db_path_clone, "warn", "symbol_info", "api", Some(&sym_clone), &format!("Name and currency not stored: {}", err));
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
pub(crate) async fn update_watchlist_symbol(
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
pub(crate) async fn delete_watchlist_symbol(
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
pub(crate) struct RenameWatchlistList {
    pub(crate) old_name: String,
    pub(crate) new_name: String,
}

#[utoipa::path(put, path = "/api/v1/watchlist/lists/rename", tag = "watchlist", responses((status = 200, description = "Rename watchlist list")))]
#[put("/api/watchlist/lists/rename")]
pub(crate) async fn rename_watchlist_list(
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

#[utoipa::path(get, path = "/api/v1/watchlist/prices", tag = "watchlist", responses((status = 200, description = "Get watchlist prices")))]
#[get("/api/watchlist/prices")]
pub(crate) async fn get_watchlist_prices(db_path: web::Data<PathBuf>, query: web::Query<WatchlistQuery>) -> impl Responder {
    match fetch_watchlist_current_prices(&db_path, query.list.as_deref()).await {
        Ok(prices) => HttpResponse::Ok().json(prices),
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "price_fetch", "api", None, &err);
            err_internal(err)
        }
    }
}

#[utoipa::path(get, path = "/api/v1/watchlist/cached-prices", tag = "watchlist", responses((status = 200, description = "Get watchlist cached prices")))]
#[get("/api/watchlist/cached-prices")]
pub(crate) async fn get_watchlist_cached_prices(db_path: web::Data<PathBuf>, query: web::Query<WatchlistQuery>) -> impl Responder {
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

#[derive(Deserialize)]
pub(crate) struct WatchlistSymbolUpdate {
    pub(crate) lists: Vec<String>,
    // Absent keeps the stored value; `null` clears it — the same rule as
    // `PUT /watchlist/{id}`. These used to overwrite with null whenever a
    // client left them out, so a client sending only `lists` erased the
    // notes and price levels this project has lost before.
    #[serde(default, deserialize_with = "deserialize_explicit_null")]
    pub(crate) notes: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_explicit_null")]
    pub(crate) breakthrough_price: Option<Option<f64>>,
    #[serde(default, deserialize_with = "deserialize_explicit_null")]
    pub(crate) stop_loss_price: Option<Option<f64>>,
    pub(crate) custom_fields: Option<std::collections::HashMap<String, String>>,
}

/// Set a watchlist symbol's list memberships, notes and fields in one
/// transactional call — replaces the parallel add/remove/update fan-out the
/// browser used to perform.
#[utoipa::path(put, path = "/api/v1/watchlist/symbol/{symbol}", tag = "watchlist", params(("symbol" = String, Path, description = "symbol")), responses((status = 200, description = "Update watchlist symbol lists")))]
#[put("/api/watchlist/symbol/{symbol}")]
pub(crate) async fn update_watchlist_symbol_lists(
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

    let result = with_tx(&db_path, |tx| {
        let now = Utc::now().to_rfc3339();
        tx.execute(
            "INSERT INTO watchlist_symbols (symbol, notes, breakthrough_price, stop_loss_price, updated_at) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(symbol) DO UPDATE SET
               notes = CASE WHEN ?6 THEN excluded.notes ELSE notes END,
               breakthrough_price = CASE WHEN ?7 THEN excluded.breakthrough_price ELSE breakthrough_price END,
               stop_loss_price = CASE WHEN ?8 THEN excluded.stop_loss_price ELSE stop_loss_price END,
               updated_at = excluded.updated_at",
            params![
                symbol,
                payload.notes.clone().flatten(),
                payload.breakthrough_price.flatten(),
                payload.stop_loss_price.flatten(),
                now,
                payload.notes.is_some(),
                payload.breakthrough_price.is_some(),
                payload.stop_loss_price.is_some(),
            ],
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
        Ok(())
    });
    if let Err(err) = result {
        let _ = insert_event_log(&db_path, "error", "watchlist_update", "api", Some(&symbol), &err);
        return err_internal(err);
    }

    // Merge custom fields after the membership transaction (same semantics as
    // the single-row update endpoint)
    // A database that cannot be opened used to skip the fields silently and
    // still report success.
    if let Some(fields) = payload.custom_fields.as_ref()
        && let Err(err) = open_db(db_path.as_ref())
            .map_err(|e| e.to_string())
            .and_then(|conn| save_custom_fields(&conn, &symbol, fields))
    {
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

pub(crate) fn load_custom_fields(conn: &Connection, symbol: &str) -> std::collections::HashMap<String, String> {
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
pub(crate) fn save_custom_fields(conn: &Connection, symbol: &str, fields: &std::collections::HashMap<String, String>) -> Result<(), String> {
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

/// The watchlist query every reader extends with its own WHERE/ORDER BY: the
/// two normalised tables joined, as CLAUDE.md requires.
pub(crate) const WATCHLIST_SELECT: &str = "SELECT wm.id, ws.symbol, wm.list_name, wm.added_at, ws.notes, ws.breakthrough_price, ws.stop_loss_price
     FROM watchlist_memberships wm
     JOIN watchlist_symbols ws ON wm.symbol = ws.symbol";

impl WatchlistSymbol {
    /// One row of `WATCHLIST_SELECT`, custom fields still to be filled in.
    pub(crate) fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Self> {
        Ok(WatchlistSymbol {
            id: row.get(0)?,
            symbol: row.get(1)?,
            list_name: row.get(2)?,
            added_at: row.get(3)?,
            notes: row.get(4)?,
            breakthrough_price: row.get(5)?,
            stop_loss_price: row.get(6)?,
            custom_fields: Default::default(),
        })
    }
}

pub(crate) fn load_watchlist_symbols(db_path: &PathBuf, list: Option<&str>) -> Result<Vec<WatchlistSymbol>, String> {
    let conn = open_db(db_path).map_err(|err| err.to_string())?;
    let mut rows: Vec<WatchlistSymbol> = if let Some(list_name) = list {
        let mut stmt = conn
            .prepare(&format!("{WATCHLIST_SELECT} WHERE wm.list_name = ?1 ORDER BY ws.symbol"))
            .map_err(|err| err.to_string())?;
        stmt.query_map(params![list_name], WatchlistSymbol::from_row)
        .map_err(|err| err.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| err.to_string())?
    } else {
        let mut stmt = conn
            .prepare(&format!("{WATCHLIST_SELECT} ORDER BY wm.list_name, ws.symbol"))
            .map_err(|err| err.to_string())?;
        stmt.query_map([], WatchlistSymbol::from_row)
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

pub(crate) fn load_watchlist_lists(db_path: &PathBuf) -> Result<Vec<String>, String> {
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

pub(crate) fn insert_watchlist_symbol(db_path: &PathBuf, symbol: &str, list_name: &str, notes: Option<&str>, breakthrough_price: Option<f64>, stop_loss_price: Option<f64>, custom_fields: Option<&std::collections::HashMap<String, String>>) -> Result<WatchlistSymbol, String> {
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

    let mut result = conn
        .query_row(
            &format!("{WATCHLIST_SELECT} WHERE wm.symbol = ?1 AND wm.list_name = ?2"),
            params![symbol, list_name],
            WatchlistSymbol::from_row,
        )
        .optional()
        .map_err(|err| err.to_string())?
        .ok_or_else(|| "Failed to load inserted symbol".to_string())?;
    result.custom_fields = load_custom_fields(&conn, &result.symbol);
    Ok(result)
}

pub(crate) fn update_watchlist_symbol_notes(db_path: &PathBuf, id: i64, notes: Option<Option<String>>, breakthrough_price: Option<Option<f64>>, stop_loss_price: Option<Option<f64>>, custom_fields: Option<&std::collections::HashMap<String, String>>) -> Result<WatchlistSymbol, String> {
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
    let mut result = conn
        .query_row(&format!("{WATCHLIST_SELECT} WHERE wm.id = ?1"), params![id], WatchlistSymbol::from_row)
        .optional()
        .map_err(|err| err.to_string())?
        .ok_or_else(|| "Symbol not found after update".to_string())?;
    result.custom_fields = load_custom_fields(&conn, &result.symbol);
    Ok(result)
}

pub(crate) fn remove_watchlist_symbol(db_path: &Path, id: i64) -> Result<bool, String> {
    // The membership and, when it was the last, the symbol row go together.
    with_tx(db_path, |conn| {
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
    })
}

pub(crate) async fn fetch_watchlist_current_prices(db_path: &PathBuf, list: Option<&str>) -> Result<Vec<CurrentPrice>, String> {
    let symbols: Vec<String> = load_watchlist_symbols(db_path, list)?
        .into_iter()
        .map(|s| s.symbol)
        .collect();
    if symbols.is_empty() {
        return Ok(Vec::new());
    }
    fetch_and_cache_current_prices(db_path, &symbols, "watchlist_prices_updated_at").await
}

pub(crate) fn indicator_points(history: &[PriceHistoryPoint]) -> Vec<stocks::indicators::PricePoint> {
    history
        .iter()
        .map(|p| stocks::indicators::PricePoint { close: p.close, volume: p.volume })
        .collect()
}

/// Full indicator block for one symbol — the server-side equivalent of the
/// watchlist enrichment previously computed in the browser.
pub(crate) fn compute_symbol_indicators(history: &[PriceHistoryPoint], price: Option<f64>, volume: Option<i64>) -> serde_json::Value {
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
pub(crate) async fn get_watchlist_enriched(db_path: web::Data<PathBuf>, query: web::Query<WatchlistQuery>) -> impl Responder {
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
    // The 40-week EMA needs years of dailies to settle — far more than the
    // 300-day history above — so it is read from the stored bars, the same way
    // the holdings cards get it, and the two screens agree on the figure.
    let ema_conn = match open_db(db_path.as_ref()) {
        Ok(c) => Some(c),
        Err(err) => {
            let _ = insert_event_log(&db_path, "warn", "watchlist_fetch", "api", None, &format!("40-week EMA unavailable for enriched watchlist: {}", err));
            None
        }
    };
    let indicator_map: HashMap<&String, serde_json::Value> = unique
        .iter()
        .map(|sym| {
            let p = price_map.get(sym);
            let hist = histories.get(sym).unwrap_or(&empty);
            let mut indicators = compute_symbol_indicators(hist, p.and_then(|x| x.price), p.and_then(|x| x.volume));
            indicators["ema40w"] = serde_json::json!(ema_conn.as_ref().and_then(|c| stored_weekly_ema(c, sym, 40)));
            (sym, indicators)
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

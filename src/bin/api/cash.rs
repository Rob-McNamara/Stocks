//! The cash ledger: accounts, their transactions, transfers between them, the
//! settlement leg each trade writes, and the CSV export.

use super::*;

pub(crate) fn is_valid_cash_tx_kind(kind: &str) -> bool {
    CASH_TX_KINDS.contains(&kind)
}

/// Kinds that only ever originate from a transaction, so the manual endpoints
/// refuse to create them directly.
///
/// Note this is about *entry*, not ownership: `dividend` is absent because a
/// hand-entered dividend is legitimate (an account can be credited without the
/// app having fetched the event). Ownership is decided by `holding_tx_id`
/// instead — see `is_transaction_owned`.
pub(crate) fn is_trade_owned_kind(kind: &str) -> bool {
    kind == "trade_buy" || kind == "trade_sell"
}

/// Whether a cash row was written by `sync_trade_cash_leg` on behalf of a
/// transaction. Such a row is regenerated whenever that transaction is saved,
/// so editing it by hand is silently undone — the manual endpoints refuse
/// instead. Keyed on the link rather than the kind, because a dividend leg and
/// a hand-entered dividend share a kind but not an owner.
pub(crate) fn is_transaction_owned(holding_tx_id: Option<i64>) -> bool {
    holding_tx_id.is_some()
}

pub(crate) fn cash_account_currency(conn: &Connection, account_id: i64) -> Option<String> {
    conn.query_row(
        "SELECT currency FROM cash_accounts WHERE id = ?1",
        params![account_id],
        |row| row.get::<_, String>(0),
    )
    .optional()
    .ok()
    .flatten()
}

/// Ledger balance for one account, in the account's own currency, optionally
/// as at a date. Derived from the ledger every time — never stored — so a
/// back-dated entry is reflected immediately.
///
/// Fallible on purpose. The SQL already `COALESCE`s an empty ledger to zero, so
/// the only thing a swallowed error could add is a *wrong* zero — reported as a
/// balance, summed into the portfolio total, and used as a guard against
/// changing an account's currency. Each caller decides what to do instead.
pub(crate) fn cash_balance(conn: &Connection, account_id: i64, as_of: Option<&str>) -> Result<f64, String> {
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
pub(crate) struct CashAccountPayload {
    pub(crate) name: String,
    pub(crate) currency: String,
    pub(crate) interest_rate: Option<f64>,
    pub(crate) include_in_portfolio: Option<bool>,
    pub(crate) notes: Option<String>,
}

#[derive(Serialize)]
pub(crate) struct CashAccountRow {
    pub(crate) id: i64,
    pub(crate) name: String,
    pub(crate) currency: String,
    pub(crate) interest_rate: Option<f64>,
    pub(crate) include_in_portfolio: bool,
    pub(crate) notes: Option<String>,
    pub(crate) created_at: String,
    /// Balance in the account's own currency.
    pub(crate) balance: f64,
    /// The same balance in AUD at today's stored rate, or null when no rate is
    /// available yet for that currency.
    pub(crate) balance_aud: Option<f64>,
    pub(crate) transaction_count: i64,
}

/// A cash_accounts row as read for the listing: (id, name, currency,
/// interest_rate, include_in_portfolio, notes, created_at).
pub(crate) type CashAccountDbRow = (i64, String, String, Option<f64>, i64, Option<String>, String);

pub(crate) fn load_cash_accounts(conn: &Connection) -> Result<Vec<CashAccountRow>, String> {
    let today = today_local_str();
    let mut stmt = conn
        .prepare(
            "SELECT id, name, currency, interest_rate, include_in_portfolio, notes, created_at
               FROM cash_accounts ORDER BY name",
        )
        .map_err(|e| e.to_string())?;
    let rows: Vec<CashAccountDbRow> = stmt
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?))
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?
        .into_iter()
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
pub(crate) async fn get_cash_accounts(db_path: web::Data<PathBuf>) -> impl Responder {
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
pub(crate) async fn add_cash_account(db_path: web::Data<PathBuf>, payload: web::Json<CashAccountPayload>) -> impl Responder {
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
pub(crate) async fn update_cash_account(
    db_path: web::Data<PathBuf>,
    path: web::Path<i64>,
    payload: web::Json<CashAccountPayload>,
) -> impl Responder {
    let id = path.into_inner();
    let payload = payload.into_inner();
    // The same checks as creating one: an update used to accept an empty name
    // or a currency that isn't a code.
    if payload.name.trim().is_empty() {
        return err_bad_request("Account name is required");
    }
    if payload.currency.trim().len() != 3 {
        return err_bad_request("Currency must be a 3-letter code, e.g. AUD");
    }
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
        // To the cent: deposits and withdrawals that net to zero rarely sum to
        // exactly 0.0 in floating point, which used to block the change.
        if balance.abs() >= 0.005 {
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
pub(crate) async fn delete_cash_account(db_path: web::Data<PathBuf>, path: web::Path<i64>) -> impl Responder {
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
pub(crate) struct CashTxPayload {
    pub(crate) account_id: i64,
    pub(crate) date: String,
    pub(crate) amount: f64,
    pub(crate) kind: String,
    pub(crate) notes: Option<String>,
}

#[derive(Serialize)]
pub(crate) struct CashTxRow {
    pub(crate) id: i64,
    pub(crate) account_id: i64,
    pub(crate) account_name: String,
    pub(crate) currency: String,
    pub(crate) date: String,
    pub(crate) amount: f64,
    pub(crate) kind: String,
    pub(crate) holding_tx_id: Option<i64>,
    pub(crate) transfer_group_id: Option<String>,
    pub(crate) notes: Option<String>,
    pub(crate) created_at: String,
}

#[derive(Deserialize)]
pub(crate) struct CashTxQuery {
    pub(crate) account_id: Option<i64>,
    pub(crate) from: Option<String>,
    pub(crate) to: Option<String>,
}

/// Quote a CSV field only when it needs it, doubling any embedded quotes.
///
/// Notes are free text and routinely contain commas — an unquoted
/// "purchase SPCX — USD 810.00 at 1.4189" would split into two columns and
/// shift every field after it.
pub(crate) fn csv_field(value: &str) -> String {
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
#[utoipa::path(get, path = "/api/v1/cash/accounts/{id}/transactions.csv", tag = "cash",
    params(("id" = i64, Path, description = "id")),
    responses((status = 200, description = "Cash account ledger as CSV")))]
#[get("/api/cash/accounts/{id}/transactions.csv")]
pub(crate) async fn export_cash_account_csv(db_path: web::Data<PathBuf>, path: web::Path<i64>) -> impl Responder {
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
pub(crate) async fn get_cash_transactions(db_path: web::Data<PathBuf>, query: web::Query<CashTxQuery>) -> impl Responder {
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
        Ok(rows) => match collect_rows(&db_path, "cash_transactions_fetch", rows) {
            Ok(rows) => HttpResponse::Ok().json(rows),
            Err(response) => response,
        },
        Err(err) => err_internal(err.to_string()),
    }
}

/// Shared validation for a manually entered cash transaction.
pub(crate) fn validate_cash_tx(conn: &Connection, payload: &CashTxPayload) -> Result<String, String> {
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
pub(crate) async fn add_cash_transaction(db_path: web::Data<PathBuf>, payload: web::Json<CashTxPayload>) -> impl Responder {
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
pub(crate) async fn update_cash_transaction(
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
pub(crate) async fn delete_cash_transaction(db_path: web::Data<PathBuf>, path: web::Path<i64>) -> impl Responder {
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

/// What `sync_trade_cash_leg` reads of a trade: (cash_account_id,
/// transaction_type, date, quantity, price, original_price, fx_rate,
/// brokerage, symbol, amount).
pub(crate) type TradeLegRow = (Option<i64>, String, String, Option<f64>, Option<f64>, Option<f64>, Option<f64>, Option<f64>, String, Option<f64>);

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
pub(crate) fn sync_trade_cash_leg(conn: &Connection, holding_tx_id: i64) -> Result<(), String> {
    conn.execute("DELETE FROM cash_transactions WHERE holding_tx_id = ?1", params![holding_tx_id])
        .map_err(|e| e.to_string())?;

    let row: Option<TradeLegRow> = conn
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
            (Some(a), _) => {
                // The amount is in the trade's currency. When the leg was
                // converted into an AUD account it has to be converted the same
                // way — US$15 withheld used to be booked as A$15.
                let in_leg_currency = if settle_converted {
                    match fx_rate.filter(|r| *r > 0.0) {
                        Some(rate) => a * rate,
                        None => {
                            return Err(format!(
                                "{} dividend has {} withholding but no exchange rate to express it in {}",
                                symbol, trade_currency, account_currency
                            ))
                        }
                    }
                } else {
                    a
                };
                (
                    (in_leg_currency * 100.0).round() / 100.0,
                    format!("tax withheld on {} dividend", symbol),
                )
            }
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
pub(crate) struct CashTransferPayload {
    pub(crate) from_account_id: i64,
    pub(crate) to_account_id: i64,
    pub(crate) date: String,
    /// Amount leaving the source account, in the source account's currency.
    pub(crate) from_amount: f64,
    /// Amount arriving in the destination account, in its own currency.
    pub(crate) to_amount: f64,
    pub(crate) notes: Option<String>,
}

#[utoipa::path(post, path = "/api/v1/cash/transfer", tag = "cash", responses((status = 200, description = "Move cash between accounts, including across currencies")))]
#[post("/api/cash/transfer")]
pub(crate) async fn add_cash_transfer(db_path: web::Data<PathBuf>, payload: web::Json<CashTransferPayload>) -> impl Responder {
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
pub(crate) const CASH_TX_KINDS: &[&str] = &[
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

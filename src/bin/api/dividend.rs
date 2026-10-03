//! Dividends: fetching events, recording the ones the holder was entitled to,
//! and the per-symbol dividend totals the P/L figures use.

use super::*;

#[allow(dead_code)] // only total_payment is aggregated today; other fields document the calculation
#[derive(Debug)]
pub(crate) struct DividendPayment {
    pub(crate) symbol: String,
    pub(crate) ex_date: NaiveDate,
    pub(crate) payment_date: Option<NaiveDate>,
    pub(crate) amount_per_share: f64,
    pub(crate) shares_held: f64,
    pub(crate) total_payment: f64,
}

/// A dividend_events row: (symbol, ex_date, payment_date, amount, currency)
/// — the currency being the one the per-share amount is in.
pub(crate) type DividendEventRow = (String, String, Option<String>, f64, String);

#[utoipa::path(get, path = "/api/v1/dividends", tag = "dividends", responses((status = 200, description = "Get dividends")))]
#[get("/api/dividends")]
pub(crate) async fn get_dividends(db_path: web::Data<PathBuf>) -> impl Responder {
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
        Ok(mapped) => match collect_rows(&db_path, "dividends_fetch", mapped) {
            Ok(items) => HttpResponse::Ok().json(items),
            Err(response) => response,
        },
        Err(err) => err_internal(err.to_string()),
    }
}

#[derive(Serialize)]
pub(crate) struct DividendRefreshResult {
    pub(crate) updated: usize,
    pub(crate) errors: Vec<String>,
}

/// Thin wrappers over `stocks::dividends`, shared with the dividends daemon so
/// the two can never again store events differently.
pub(crate) async fn fetch_dividend_events_for_symbol(client: &Client, symbol: &str) -> Result<Vec<DividendEvent>, String> {
    stocks::dividends::fetch_events(client, symbol).await
}

pub(crate) fn store_dividend_events_for_symbol(db_path: &PathBuf, symbol: &str, events: &[DividendEvent]) -> Result<(), String> {
    let mut conn = open_db(db_path).map_err(|e| e.to_string())?;
    stocks::dividends::replace_events(&mut conn, symbol, events)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Symbols sold out of entirely — nothing held, but at least one sale.
///
/// Price collection otherwise follows what is held and what is watched, so an
/// exited position stops being priced the day it leaves the watchlist. The
/// Hindsight screen's whole question is what the stock did *after* the sale, so
/// these have to keep being fetched: the quote answers "current price", and the
/// daily bar each fetch also writes is what lets the +1 week, +6 week, +3 month
/// and peak-since windows fill in as time passes.
pub(crate) fn load_exited_symbols(db_path: &PathBuf) -> Result<Vec<String>, String> {
    let conn = open_db(db_path).map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare(
            "SELECT symbol,
                    SUM(CASE WHEN transaction_type='purchase' THEN quantity ELSE -quantity END) AS net_qty,
                    SUM(CASE WHEN transaction_type='sale' THEN 1 ELSE 0 END) AS sales
             FROM holdings_transactions
             WHERE transaction_type IN ('purchase', 'sale')
             GROUP BY symbol
             HAVING net_qty <= 1e-9 AND sales > 0
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

pub(crate) fn load_holding_symbols(db_path: &PathBuf) -> Result<Vec<String>, String> {
    let conn = open_db(db_path).map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare(
            "SELECT symbol, SUM(CASE WHEN transaction_type='purchase' THEN quantity ELSE -quantity END) as net_qty
             FROM holdings_transactions
             WHERE transaction_type IN ('purchase', 'sale')
             GROUP BY symbol
             HAVING net_qty > 1e-9
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

pub(crate) fn load_sold_symbols(db_path: &PathBuf) -> Result<Vec<String>, String> {
    let conn = open_db(db_path).map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare(
            "SELECT symbol
             FROM holdings_transactions
             WHERE transaction_type IN ('purchase', 'sale')
             GROUP BY symbol
             HAVING SUM(CASE WHEN transaction_type='purchase' THEN quantity ELSE -quantity END) <= 1e-9
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
pub(crate) async fn refresh_dividends_for_symbols(db_path: &PathBuf, symbols: Vec<String>) -> DividendRefreshResult {
    if symbols.is_empty() {
        return DividendRefreshResult { updated: 0, errors: vec![] };
    }

    let client = http_client();

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
pub(crate) struct DividendRecordResult {
    pub(crate) recorded: usize,
    /// Already had a transaction.
    pub(crate) already_present: usize,
    /// Declined by the user; see `dividend_exclusions`.
    pub(crate) excluded: usize,
    /// Currencies with no configured destination account, so nothing was
    /// recorded for them.
    pub(crate) unconfigured_currencies: Vec<String>,
    pub(crate) errors: Vec<String>,
}

/// Where dividends in `currency` are paid, from `app_config`.
///
/// Keyed by currency because that is how the accounts actually divide: AUD
/// distributions land in the everyday investment account, USD ones in the
/// foreign broker. A currency with no key configured is left alone rather than
/// guessed at — inventing a destination would move real money to the wrong
/// place.
pub(crate) fn dividend_account_for_currency(conn: &Connection, currency: &str) -> Option<i64> {
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

/// Config key: the earliest ex-date that is recorded automatically.
pub(crate) const DIVIDEND_RECORD_FROM: &str = "dividend_record_from";

/// The ex-date from which fetched dividends are recorded as transactions.
///
/// Dividend history now reaches back decades, but the cash ledger does not:
/// recording a 2003 payment would credit an account with twenty years of money
/// it never saw in this app. Recording starts from the configured date, or —
/// unset — from the earliest dividend already recorded, which is where it
/// effectively started when history only reached back five years. A database
/// with nothing recorded yet starts five years back, as it always has.
pub(crate) fn dividend_record_floor(conn: &Connection) -> String {
    let configured: Option<String> = conn
        .query_row("SELECT value FROM app_config WHERE key = ?1", params![DIVIDEND_RECORD_FROM], |r| r.get(0))
        .optional()
        .ok()
        .flatten()
        .map(|v: String| v.trim().to_string())
        .filter(|v| NaiveDate::parse_from_str(v, "%Y-%m-%d").is_ok());
    if let Some(date) = configured {
        return date;
    }
    let earliest_recorded: Option<String> = conn
        .query_row("SELECT MIN(date) FROM holdings_transactions WHERE transaction_type = 'dividend'", [], |r| r.get(0))
        .ok()
        .flatten();
    earliest_recorded.unwrap_or_else(|| {
        (today_local() - chrono::Duration::days(5 * 365)).format("%Y-%m-%d").to_string()
    })
}

/// Record every fetched dividend event the holder was entitled to and has not
/// already recorded or declined.
///
/// Runs after each dividend refresh so a newly fetched event reaches the cash
/// ledger on its own. Previously this was a one-off script, and anything
/// fetched afterwards sat in the Transactions screen as a derived row with no
/// account, invisible in the cash balance until someone remembered to re-run
/// it.
/// Serialises `record_new_dividends`. It checks for an existing transaction and
/// then inserts one, so two overlapping runs — the startup refresh and the
/// dividend button, or two open tabs — could both pass the check and book the
/// same dividend twice. Every caller is in this process, so a lock here closes
/// the gap.
pub(crate) static RECORD_DIVIDENDS_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub(crate) fn record_new_dividends(db_path: &PathBuf) -> Result<DividendRecordResult, String> {
    let _serialised = RECORD_DIVIDENDS_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut result = DividendRecordResult::default();
    let conn = open_db(db_path).map_err(|e| e.to_string())?;

    let events: Vec<(String, String, f64)> = {
        let mut stmt = conn
            .prepare(
                "SELECT e.symbol, e.ex_date, e.amount
                   FROM dividend_events e
                  WHERE e.ex_date >= ?1
                    AND NOT EXISTS (SELECT 1 FROM holdings_transactions h
                                     WHERE h.symbol = e.symbol AND h.date = e.ex_date
                                       AND h.transaction_type = 'dividend')
                  ORDER BY e.ex_date, e.symbol",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![dividend_record_floor(&conn)], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?
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
pub(crate) fn with_recorded_dividends(db_path: &PathBuf, fetched: DividendRefreshResult) -> serde_json::Value {
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
pub(crate) async fn refresh_dividends(db_path: web::Data<PathBuf>) -> impl Responder {
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
pub(crate) async fn refresh_sold_dividends(db_path: web::Data<PathBuf>) -> impl Responder {
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

/// Dividends each symbol has paid the holder, in AUD and net of withholding.
///
/// The figure the holdings and dashboard P/L add to market value, so it has to
/// be money in the same terms as that value. It used to be per-share event
/// amounts × shares held: gross of withholding, in whatever currency the stock
/// lists in (US dollars added straight into an AUD P/L), and in preference to
/// the dividends actually recorded.
///
/// Now each payment is counted once, from the best source for it:
///
/// - A recorded dividend transaction is what was received: its AUD `amount`,
///   less the tax withheld on it.
/// - A fetched event with no recorded transaction on its ex-date — history
///   from before recording began, or a currency with no destination account —
///   is valued as shares held × per-share amount, converted at that date's
///   stored rate and reduced by the symbol's standing withholding rate.
/// - An event the user deleted (`dividend_exclusions`) means "not this one"
///   and is not counted.
pub(crate) fn calculate_dividend_totals(db_path: &PathBuf, transactions: &[HoldingTransaction]) -> Result<std::collections::HashMap<String, f64>, String> {
    use std::collections::{HashMap, HashSet};

    let symbols: HashSet<String> = transactions.iter().map(|tx| tx.symbol.clone()).collect();
    if symbols.is_empty() {
        return Ok(HashMap::new());
    }

    let events = load_dividend_events(db_path, &symbols)?;
    let conn = open_db(db_path).map_err(|e| e.to_string())?;

    // Standing withholding rate and listing currency, per symbol.
    let mut withholding_pct: HashMap<String, f64> = HashMap::new();
    let mut listing_currency: HashMap<String, String> = HashMap::new();
    {
        let mut stmt = conn
            .prepare("SELECT symbol, dividend_withholding_pct, currency FROM symbol_info")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<f64>>(1)?, r.get::<_, Option<String>>(2)?)))
            .map_err(|e| e.to_string())?;
        for row in rows {
            let (symbol, pct, currency) = row.map_err(|e| e.to_string())?;
            if let Some(pct) = pct.filter(|p| *p > 0.0) {
                withholding_pct.insert(symbol.clone(), pct);
            }
            if let Some(ccy) = currency.map(|c| c.trim().to_uppercase()).filter(|c| !c.is_empty()) {
                listing_currency.insert(symbol, ccy);
            }
        }
    }

    // Tax withheld on individual recorded payments, in the trade's currency.
    let mut withheld_on: HashMap<i64, f64> = HashMap::new();
    {
        let mut stmt = conn
            .prepare("SELECT id, withholding_amount FROM holdings_transactions WHERE transaction_type = 'dividend' AND withholding_amount > 0")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, f64>(1)?)))
            .map_err(|e| e.to_string())?;
        for row in rows {
            let (id, amount) = row.map_err(|e| e.to_string())?;
            withheld_on.insert(id, amount);
        }
    }

    let mut excluded: HashSet<(String, String)> = HashSet::new();
    {
        let mut stmt = conn
            .prepare("SELECT symbol, ex_date FROM dividend_exclusions")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| e.to_string())?;
        for row in rows {
            excluded.insert(row.map_err(|e| e.to_string())?);
        }
    }

    let mut totals = HashMap::new();
    for symbol in symbols {
        let symbol_transactions: Vec<HoldingTransaction> = transactions
            .iter()
            .filter(|tx| tx.symbol == symbol)
            .cloned()
            .collect();
        let net_of = |gross: f64| match withholding_pct.get(&symbol) {
            Some(pct) => gross * (1.0 - pct / 100.0),
            None => gross,
        };

        let mut total = 0.0;
        let mut recorded_dates: HashSet<&str> = HashSet::new();
        for tx in symbol_transactions.iter().filter(|t| t.transaction_type == "dividend") {
            let Some(amount) = tx.amount else { continue };
            recorded_dates.insert(tx.date.as_str());
            total += match withheld_on.get(&tx.id) {
                // The explicit amount is in the trade's own currency.
                Some(withheld) => {
                    let rate = if tx.currency == "AUD" { 1.0 } else { tx.fx_rate.unwrap_or(1.0) };
                    amount - withheld * rate
                }
                None => net_of(amount),
            };
        }

        let currency = listing_currency
            .get(&symbol)
            .cloned()
            .or_else(|| symbol_transactions.iter().map(|t| t.currency.to_uppercase()).find(|c| !c.is_empty()))
            .unwrap_or_else(|| "AUD".to_string());
        let unrecorded: Vec<DividendEvent> = events
            .iter()
            .filter(|e| e.symbol == symbol)
            .filter(|e| {
                let date = e.ex_date.format("%Y-%m-%d").to_string();
                !recorded_dates.contains(date.as_str()) && !excluded.contains(&(symbol.clone(), date))
            })
            .cloned()
            .collect();
        for payment in calculate_dividend_payments(&symbol_transactions, &unrecorded) {
            let date = payment.ex_date.format("%Y-%m-%d").to_string();
            let Some(rate) = fx_rate_on(&conn, &currency, &date).or_else(|| earliest_fx_rate(&conn, &currency)) else {
                log_event_on_conn(
                    &conn,
                    "warn",
                    "dividend_totals",
                    Some(&symbol),
                    &format!("No {}/AUD rate for the {} dividend; left out of the total", currency, date),
                );
                continue;
            };
            total += net_of(payment.total_payment * rate);
        }

        totals.insert(symbol, total);
    }

    Ok(totals)
}

pub(crate) fn load_dividend_events(db_path: &PathBuf, symbols: &std::collections::HashSet<String>) -> Result<Vec<DividendEvent>, String> {
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
pub(crate) fn calculate_dividend_payments(
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
pub(crate) fn calculate_shares_on_date(transactions: &[HoldingTransaction], date: NaiveDate) -> f64 {
    portfolio::shares_on_date(&to_portfolio_txs(transactions), &date.format("%Y-%m-%d").to_string())
}

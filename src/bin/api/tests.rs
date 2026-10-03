//! The API's tests: handlers driven through a real actix app against a
//! temporary database, and the helpers they share.

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

fn audit_rows(db_path: &PathBuf, table: &str) -> Vec<(String, Option<String>)> {
    let conn = open_db(db_path).unwrap();
    let mut stmt = conn
        .prepare("SELECT action, new_values FROM audit_log WHERE table_name = ?1 ORDER BY id")
        .unwrap();
    stmt.query_map(params![table], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().flatten().collect()
}

/// Every quote upserts symbol_info; only an actual change belongs in the
/// audit log, or the withholding rate a user typed is buried in noise.
#[test]
fn a_symbol_info_refresh_that_changes_nothing_is_not_audited() {
    let (_file, db_path) = setup_test_db();
    store_symbol_info(&db_path, "BHP.AX", Some("EQUITY"), Some("BHP Group"), Some("AUD")).unwrap();
    store_symbol_info(&db_path, "BHP.AX", Some("EQUITY"), Some("BHP Group"), Some("AUD")).unwrap();
    assert_eq!(audit_rows(&db_path, "symbol_info").len(), 1, "insert only; the no-op update is skipped");

    let conn = open_db(&db_path).unwrap();
    conn.execute("UPDATE symbol_info SET dividend_withholding_pct = 15 WHERE symbol = 'BHP.AX'", []).unwrap();
    drop(conn);
    assert_eq!(audit_rows(&db_path, "symbol_info").len(), 2, "a real change is recorded");
}

/// The audit log keeps a year of values; the AI key must never be one.
#[test]
fn the_ai_key_is_redacted_in_the_audit_log() {
    let (_file, db_path) = setup_test_db();
    upsert_config(&db_path, "ai_api_key", "sk-secret-1").unwrap();
    upsert_config(&db_path, "ai_api_key", "sk-secret-2").unwrap();
    let conn = open_db(&db_path).unwrap();
    let leaked: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM audit_log WHERE old_values LIKE '%sk-secret%' OR new_values LIKE '%sk-secret%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(leaked, 0);
    assert!(!audit_rows(&db_path, "app_config").is_empty(), "the change itself is still recorded");
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
        // Fetched from Yahoo and replaced wholesale; see schema.rs.
        "dividend_events",
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
async fn hindsight_rows(db_path: &Path) -> Vec<serde_json::Value> {
    let app = actix_web::test::init_service(
        App::new().app_data(web::Data::new(db_path.to_path_buf())).service(get_hindsight),
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

/// A US-listed stock's per-share dividend is in US dollars and has tax
/// withheld. The P/L adds this total to an AUD market value, so it must be
/// converted at the ex-date's rate and net of the withholding.
#[test]
fn dividend_totals_are_aud_and_net_of_withholding() {
    let (_file, db_path) = setup_test_db();
    let conn = open_db(&db_path).unwrap();
    conn.execute_batch(
        "INSERT INTO holdings_transactions (symbol, transaction_type, date, quantity, price, currency, original_price, fx_rate, created_at)
         VALUES ('VTI', 'purchase', '2025-01-02', 10, 150, 'USD', 100, 1.5, 'x');
         INSERT INTO symbol_info (symbol, currency, dividend_withholding_pct, updated_at) VALUES ('VTI', 'USD', 15, 'x');
         INSERT INTO prices (symbol, date, close, fetched_at) VALUES ('USDAUD=X', '2025-03-01', 1.6, 'x');",
    )
    .unwrap();
    drop(conn);
    insert_dividend_event_for(&db_path, "VTI", "2025-03-03", 1.0);

    let txs = fetch_holdings(&db_path).unwrap();
    let total = calculate_dividend_totals(&db_path, &txs).unwrap()["VTI"];
    // 10 shares × 1.00 USD × 1.6 = 16.00 AUD gross, less 15% = 13.60
    assert!((total - 13.6).abs() < 1e-9, "got {total}");
}

/// A dividend that has been recorded counts as what was received, once —
/// not again from the fetched event on the same ex-date — and a deleted
/// event the user declined does not count at all.
#[test]
fn dividend_totals_count_each_payment_once() {
    let (_file, db_path) = setup_test_db();
    insert_tx(&db_path, 1, "purchase", "2024-06-01", 100.0, 10.0, 0.0);
    insert_dividend_event(&db_path, "2024-09-01", 0.30);
    insert_dividend_event(&db_path, "2025-03-01", 0.40);
    insert_dividend_event(&db_path, "2025-09-01", 0.50);
    let conn = open_db(&db_path).unwrap();
    conn.execute_batch(
        "INSERT INTO holdings_transactions (symbol, transaction_type, date, quantity, price, amount, created_at)
         VALUES ('TST.AX', 'dividend', '2025-03-01', 100, 0.4, 41.0, 'x');
         INSERT INTO dividend_exclusions (symbol, ex_date, reason, created_at) VALUES ('TST.AX', '2025-09-01', 'deleted by user', 'x');",
    )
    .unwrap();
    drop(conn);

    let txs = fetch_holdings(&db_path).unwrap();
    let total = calculate_dividend_totals(&db_path, &txs).unwrap()["TST.AX"];
    // 30.00 from the unrecorded event + 41.00 as recorded; the excluded one is out
    assert!((total - 71.0).abs() < 1e-9, "got {total}");
}

/// Full dividend history must not flood the cash ledger: recording starts
/// at the earliest dividend already recorded, so decades of old events stay
/// counted in the totals but are never booked to an account.
#[test]
fn old_dividends_are_not_recorded_before_the_floor() {
    let (_file, db_path) = setup_test_db();
    seed_cash_account(&db_path, 1, "Everyday AUD", "AUD");
    upsert_config(&db_path, "dividend_account_AUD", "1").unwrap();
    insert_tx(&db_path, 1, "purchase", "2001-07-01", 100.0, 10.0, 0.0);
    insert_dividend_event(&db_path, "2003-03-01", 0.20);
    insert_dividend_event(&db_path, "2025-09-01", 0.50);
    insert_dividend_event(&db_path, "2026-03-01", 0.60);
    let conn = open_db(&db_path).unwrap();
    conn.execute(
        "INSERT INTO holdings_transactions (symbol, transaction_type, date, amount, cash_account_id, created_at)
         VALUES ('TST.AX', 'dividend', '2025-09-01', 50.0, 1, 'x')",
        [],
    )
    .unwrap();
    drop(conn);

    let result = record_new_dividends(&db_path).unwrap();
    assert_eq!(result.recorded, 1, "only the 2026 event is on or after the floor");

    // An explicit setting moves the floor.
    upsert_config(&db_path, DIVIDEND_RECORD_FROM, "2003-01-01").unwrap();
    assert_eq!(record_new_dividends(&db_path).unwrap().recorded, 1, "now the 2003 event is in range");
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
    let payload: stocks::yahoo::ChartResponse = serde_json::from_str(json).unwrap();
    let result = &payload.chart.result.unwrap()[0];
    let gmtoffset = result.gmtoffset();
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

// The dividends payload's exchange-time dating is tested where it now
// lives: `ex_dates_are_taken_in_exchange_time` in src/dividends.rs.

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
            .service(get_portfolio_risk)
            .service(get_portfolio_lots)
            .service(get_watchlist_enriched),
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
    let conn = open_db(db_path).unwrap();
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
        .set_json(serde_json::json!({ "key": "dead_symbol_JLG.AX", "value": "2025-08-01" }))
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
        .set_json(serde_json::json!({ "key": "dead_symbol_BACK.AX", "value": "" }))
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

async fn put_basis_date(db_path: &Path, date: &str) -> actix_web::http::StatusCode {
    let app = actix_web::test::init_service(
        App::new().app_data(web::Data::new(db_path.to_path_buf())).service(update_holdings_symbol_fields),
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

/// A refused date must leave the existing baseline exactly as it was. The
/// date used to be saved before the price was looked up, so a refusal left
/// the new date stored beside the old date's price.
#[actix_web::test]
async fn a_refused_basis_date_keeps_the_previous_baseline() {
    let (_file, db_path) = seed_legacy_holding();
    assert!(put_basis_date(&db_path, "2025-01-01").await.is_success());
    assert_eq!(put_basis_date(&db_path, "2030-01-01").await, actix_web::http::StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(stored_field(&db_path, PL_BASIS_DATE).as_deref(), Some("2025-01-01"));
    assert_eq!(stored_field(&db_path, PL_BASIS_PRICE).as_deref(), Some("20"));
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
    let today = today_local_str();
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

/// The card reads a foreign holding in its own currency, so the 150-day
/// average has to be available unconverted — the AUD one beside it is what
/// the totals and the comparisons are built from.
#[actix_web::test]
async fn the_card_averages_are_reported_in_both_currencies() {
    let (_file, db_path) = setup_test_db();
    let conn = open_db(&db_path).unwrap();
    conn.execute(
        "INSERT INTO holdings_transactions (id, symbol, transaction_type, date, quantity, price, brokerage, created_at)
         VALUES (1, 'USX', 'purchase', '2026-01-05', 10.0, 150.0, 0.0, '2026-01-05T00:00:00Z')",
        [],
    )
    .unwrap();
    conn.execute("INSERT INTO symbol_info (symbol, currency, updated_at) VALUES ('USX', 'USD', 'x')", [])
        .unwrap();
    // A cached rate, fresh enough to be used: without one the endpoint asks
    // Yahoo, and the test would price the holding at the live rate.
    conn.execute(
        "INSERT INTO cached_current_prices (symbol, price, last_updated, price_date)
         VALUES ('USDAUD=X', 1.5, ?1, ?2)",
        params![Utc::now().to_rfc3339(), today_local_str()],
    )
    .unwrap();
    // Two years of closes at 100 USD: enough daily bars for the 150-day
    // averages, and enough weeks behind them for the 40-week EMA to settle.
    let start = NaiveDate::from_ymd_opt(2024, 6, 3).unwrap();
    for i in 0..730 {
        let date = (start + chrono::Duration::days(i)).format("%Y-%m-%d").to_string();
        conn.execute(
            "INSERT INTO prices (symbol, date, close, fetched_at) VALUES ('USX', ?1, 100.0, 'x')",
            params![date],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO prices (symbol, date, close, fetched_at) VALUES ('USDAUD=X', ?1, 1.5, 'x')",
            params![date],
        )
        .unwrap();
    }
    drop(conn);

    let body = get_json(&db_path, "/api/portfolio/holdings").await;
    let usx = find(&body["holdings"], "USX");
    assert!(close_to(&usx["native_sma150"], 100.0), "native average stays in USD, got {}", usx["native_sma150"]);
    assert!(close_to(&usx["sma150"], 150.0), "the AUD average is converted at 1.5, got {}", usx["sma150"]);
    // The card shows all three averages, so all three come both ways.
    assert!(close_to(&usx["native_sma50"], 100.0));
    assert!(close_to(&usx["sma50"], 150.0));
    assert!(close_to(&usx["native_ema40w"], 100.0), "a flat series settles the EMA on its own level");
    assert!(close_to(&usx["ema40w"], 150.0));
}

/// The watchlist card's 40-week EMA comes from the stored bars, not the
/// 300-day history its other indicators use — too few weeks to settle.
#[actix_web::test]
async fn the_watchlist_reports_the_40_week_ema() {
    let (_file, db_path) = setup_test_db();
    let conn = open_db(&db_path).unwrap();
    conn.execute_batch(
        "INSERT INTO watchlist_symbols (symbol, updated_at) VALUES ('WLX', 'x');
         INSERT INTO watchlist_memberships (symbol, list_name, added_at) VALUES ('WLX', 'Main', 'x');
         INSERT INTO watchlist_symbols (symbol, updated_at) VALUES ('NEW', 'x');
         INSERT INTO watchlist_memberships (symbol, list_name, added_at) VALUES ('NEW', 'Main', 'x');",
    )
    .unwrap();
    let start = NaiveDate::from_ymd_opt(2024, 6, 3).unwrap();
    for i in 0..730 {
        let date = (start + chrono::Duration::days(i)).format("%Y-%m-%d").to_string();
        conn.execute(
            "INSERT INTO prices (symbol, date, close, fetched_at) VALUES ('WLX', ?1, 42.0, 'x')",
            params![date],
        )
        .unwrap();
    }
    drop(conn);

    let body = get_json(&db_path, "/api/watchlist/enriched").await;
    let item = |sym: &str| body["items"].as_array().unwrap().iter().find(|i| i["symbol"] == sym).unwrap().clone();
    assert!(close_to(&item("WLX")["indicators"]["ema40w"], 42.0), "got {}", item("WLX")["indicators"]["ema40w"]);
    assert!(item("NEW")["indicators"]["ema40w"].is_null(), "no bars, no average");
}

/// The day's money, not the day's price move: shares × the quote's change,
/// converted, so a screen can total holdings that trade in different
/// currencies without adding a US dollar to an Australian one.
#[actix_web::test]
async fn the_days_pl_is_shares_times_the_change_in_aud() {
    let (_file, db_path) = setup_test_db();
    let today = today_local_str();
    let conn = open_db(&db_path).unwrap();
    conn.execute_batch(
        "INSERT INTO holdings_transactions (id, symbol, transaction_type, date, quantity, price, brokerage, created_at)
           VALUES (1, 'USX', 'purchase', '2026-01-05', 10.0, 150.0, 0.0, '2026-01-05T00:00:00Z');
         INSERT INTO holdings_transactions (id, symbol, transaction_type, date, quantity, price, brokerage, created_at)
           VALUES (2, 'LOC.AX', 'purchase', '2026-01-05', 100.0, 5.0, 0.0, '2026-01-05T00:00:00Z');
         INSERT INTO symbol_info (symbol, currency, updated_at) VALUES ('USX', 'USD', 'x');
         INSERT INTO symbol_info (symbol, currency, updated_at) VALUES ('LOC.AX', 'AUD', 'x');",
    )
    .unwrap();
    for (sym, price, change) in [("USX", 110.0, 2.0), ("LOC.AX", 6.0, 0.25), ("USDAUD=X", 1.5, 0.0)] {
        conn.execute(
            "INSERT INTO cached_current_prices (symbol, price, change, last_updated, price_date)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![sym, price, change, Utc::now().to_rfc3339(), today],
        )
        .unwrap();
    }
    drop(conn);

    let body = get_json(&db_path, "/api/portfolio/holdings").await;
    // 10 shares × US$2.00 × 1.5
    assert!(close_to(&find(&body["holdings"], "USX")["day_pl"], 30.0));
    // Already AUD: 100 × 0.25, converted by nothing.
    assert!(close_to(&find(&body["holdings"], "LOC.AX")["day_pl"], 25.0));
}

/// A lot that is half sold carries half its fee; the rest left with the
/// shares that were sold.
#[actix_web::test]
async fn a_partly_sold_lot_carries_only_its_share_of_the_fee() {
    let (_file, db_path) = setup_test_db();
    let today = today_local_str();
    insert_tx(&db_path, 1, "purchase", "2026-01-05", 100.0, 10.0, 20.0);
    insert_tx(&db_path, 2, "sale", "2026-02-05", 50.0, 11.0, 0.0);
    let conn = open_db(&db_path).unwrap();
    conn.execute(
        "INSERT INTO cached_current_prices (symbol, price, last_updated, price_date) VALUES ('TST.AX', 12.0, ?1, ?2)",
        params![Utc::now().to_rfc3339(), today],
    )
    .unwrap();
    drop(conn);

    let body = get_json(&db_path, "/api/portfolio/lots").await;
    let lot = &body["lots"][0];
    assert!(close_to(&lot["remaining"], 50.0));
    // 50 × 12 − (50 × 10 + 20 × 50/100)
    assert!(close_to(&lot["unrealised_pl"], 90.0), "got {}", lot["unrealised_pl"]);
}

/// A stock with no symbol_info row — its background fetch failed — still
/// trades in the currency its own transactions were recorded in. It used to
/// fall back to AUD and be valued at its US-dollar price.
#[actix_web::test]
async fn a_holding_without_symbol_info_takes_its_currency_from_its_trades() {
    let (_file, db_path) = setup_test_db();
    let today = today_local_str();
    let conn = open_db(&db_path).unwrap();
    conn.execute(
        "INSERT INTO holdings_transactions (symbol, transaction_type, date, quantity, price, currency, original_price, fx_rate, created_at)
         VALUES ('USX', 'purchase', '2026-01-05', 10.0, 150.0, 'USD', 100.0, 1.5, 'x')",
        [],
    )
    .unwrap();
    for (sym, price) in [("USX", 110.0), ("USDAUD=X", 1.5)] {
        conn.execute(
            "INSERT INTO cached_current_prices (symbol, price, last_updated, price_date) VALUES (?1, ?2, ?3, ?4)",
            params![sym, price, Utc::now().to_rfc3339(), today],
        )
        .unwrap();
    }
    drop(conn);

    let body = get_json(&db_path, "/api/portfolio/holdings").await;
    let usx = find(&body["holdings"], "USX");
    assert_eq!(usx["currency"], "USD");
    assert!(close_to(&usx["current_price"], 165.0), "110 USD × 1.5, got {}", usx["current_price"]);
    assert_eq!(usx["fx_missing"], false);
}

/// A manual stop loss is in the stock's trading currency on every screen.
/// For a US stock bought in AUD the risk screen displays in AUD, and used
/// to read the same USD 100 as AUD 100.
#[actix_web::test]
async fn a_manual_stop_loss_means_the_same_on_every_screen() {
    let (_file, db_path) = setup_test_db();
    let today = today_local_str();
    let conn = open_db(&db_path).unwrap();
    conn.execute_batch(
        "INSERT INTO holdings_transactions (symbol, transaction_type, date, quantity, price, currency, created_at)
           VALUES ('USX', 'purchase', '2026-01-05', 10.0, 180.0, 'AUD', 'x');
         INSERT INTO symbol_info (symbol, currency, updated_at) VALUES ('USX', 'USD', 'x');
         INSERT INTO holdings_symbol_fields (symbol, field_key, value) VALUES ('USX', 'stop_loss', '100');",
    )
    .unwrap();
    for (sym, price) in [("USX", 110.0), ("USDAUD=X", 1.5)] {
        conn.execute(
            "INSERT INTO cached_current_prices (symbol, price, last_updated, price_date) VALUES (?1, ?2, ?3, ?4)",
            params![sym, price, Utc::now().to_rfc3339(), today],
        )
        .unwrap();
    }
    drop(conn);

    let holdings = get_json(&db_path, "/api/portfolio/holdings").await;
    assert!(close_to(&find(&holdings["holdings"], "USX")["stop_loss"], 100.0), "native, beside the native price");

    let risk = get_json(&db_path, "/api/portfolio/risk").await;
    let row = find(&risk["rows"], "USX");
    assert_eq!(row["currency"], "AUD");
    assert!(close_to(&row["current_price"], 165.0));
    assert!(close_to(&row["stop_loss"], 150.0), "USD 100 in AUD, got {}", row["stop_loss"]);
}

/// No rate means no AUD figure — not the native figure passed off as AUD.
#[test]
fn converting_without_a_rate_gives_nothing() {
    let ctx = PortfolioContext {
        groups: Vec::new(),
        prices: HashMap::new(),
        info: HashMap::new(),
        fields: HashMap::new(),
        intl: HashMap::new(),
        etf: HashMap::new(),
        all_aud: HashMap::new(),
        currency: HashMap::from([("USX".to_string(), "USD".to_string()), ("LOC.AX".to_string(), "AUD".to_string())]),
        fx_rates: HashMap::from([("USD".to_string(), None)]),
    };
    assert_eq!(ctx.to_aud("USX", 100.0), None);
    assert_eq!(ctx.to_aud("LOC.AX", 100.0), Some(100.0));
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

async fn put_cash_account(db_path: &std::path::Path, body: serde_json::Value) -> actix_web::http::StatusCode {
    let app = actix_web::test::init_service(
        App::new().app_data(web::Data::new(db_path.to_path_buf())).service(update_cash_account),
    )
    .await;
    let req = actix_web::test::TestRequest::put().uri("/api/cash/accounts/1").set_json(&body).to_request();
    actix_web::test::call_service(&app, req).await.status()
}

/// An update gets the same checks as creating an account.
#[actix_web::test]
async fn a_cash_account_update_is_validated_like_a_new_one() {
    let (_file, db_path) = setup_test_db();
    seed_cash_account(&db_path, 1, "Everyday", "AUD");
    let bad_name = serde_json::json!({ "name": "  ", "currency": "AUD" });
    let bad_ccy = serde_json::json!({ "name": "Everyday", "currency": "Dollars" });
    assert_eq!(put_cash_account(&db_path, bad_name).await, actix_web::http::StatusCode::BAD_REQUEST);
    assert_eq!(put_cash_account(&db_path, bad_ccy).await, actix_web::http::StatusCode::BAD_REQUEST);
}

/// Money in and out that nets to zero is an empty account, even when the
/// floating-point sum is a hair off zero.
#[actix_web::test]
async fn an_account_netted_to_zero_can_change_currency() {
    let (_file, db_path) = setup_test_db();
    seed_cash_account(&db_path, 1, "Everyday", "AUD");
    let conn = open_db(&db_path).unwrap();
    for amount in [0.1, 0.2, -0.3] {
        conn.execute(
            "INSERT INTO cash_transactions (account_id, date, amount, kind, created_at) VALUES (1, '2026-01-01', ?1, 'deposit', 'x')",
            params![amount],
        )
        .unwrap();
    }
    drop(conn);
    let body = serde_json::json!({ "name": "Everyday", "currency": "USD" });
    assert_eq!(put_cash_account(&db_path, body).await, actix_web::http::StatusCode::OK);
}

#[test]
fn a_manual_price_must_be_a_positive_number_or_empty() {
    assert!(validate_config_value("manual_price_JLG.AX", "0.42").is_ok());
    assert!(validate_config_value("manual_price_JLG.AX", "").is_ok());
    assert!(validate_config_value("manual_price_JLG.AX", "0").is_err());
    assert!(validate_config_value("manual_price_JLG.AX", "-1").is_err());
    assert!(validate_config_value("manual_price_JLG.AX", "abc").is_err());
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

/// USDAUD=X 2025-01-20 exactly as Yahoo's daily bars carry it: the close
/// sits above the high. Stored as-is it draws a candle body past its wick,
/// and every history re-fetch would undo `backfill_ohlc --repair`.
#[test]
fn stored_history_bars_contain_their_own_open_and_close() {
    let (_file, db_path) = setup_test_db();
    let conn = open_db(&db_path).unwrap();
    // A bar already filled, which a close-only refresh must not blank.
    conn.execute(
        "INSERT INTO prices (symbol, date, open, high, low, close, fetched_at)
         VALUES ('USDAUD=X', '2025-01-21', 1.60, 1.62, 1.59, 1.61, 'x')",
        [],
    )
    .unwrap();
    let bar = |date: &str, open, high, low, close| PriceHistoryPoint {
        date: date.to_string(), open, high, low, close, volume: None,
    };
    persist_price_history(&conn, "USDAUD=X", &[
        bar("2025-01-20", Some(1.61335003376007), Some(1.61363196372986), Some(1.59096300601959), Some(1.61409997940063)),
        bar("2025-01-21", None, None, None, Some(1.615)),
        bar("2025-01-22", None, None, None, Some(1.62)),
    ]);

    let row = |date: &str| -> (Option<f64>, Option<f64>, Option<f64>, f64) {
        conn.query_row(
            "SELECT open, high, low, close FROM prices WHERE symbol = 'USDAUD=X' AND date = ?1",
            params![date],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap()
    };
    let (open, high, low, close) = row("2025-01-20");
    assert_eq!(high, Some(1.61409997940063), "high widened to the close");
    assert_eq!(low, Some(1.59096300601959), "low already contained the body");
    assert_eq!((open, close), (Some(1.61335003376007), 1.61409997940063), "open and close are never altered");

    assert_eq!(row("2025-01-21"), (Some(1.60), Some(1.62), Some(1.59), 1.615), "a close-only refresh keeps the stored range");
    assert_eq!(row("2025-01-22"), (None, None, None, 1.62), "a close alone is not a range");
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

/// Settings keyed by symbol name follow a rename. A delisted holding used
/// to lose its manual valuation and its dead marker when renamed.
#[test]
fn a_rename_carries_the_symbols_settings() {
    let (_file, db_path) = setup_test_db();
    insert_tx(&db_path, 1, "purchase", "2024-01-01", 10.0, 1.0, 0.0);
    let conn = open_db(&db_path).unwrap();
    conn.execute("UPDATE holdings_transactions SET symbol = 'OLD.AX'", []).unwrap();
    drop(conn);
    upsert_config(&db_path, "manual_price_OLD.AX", "0.42").unwrap();
    upsert_config(&db_path, "dead_symbol_OLD.AX", "2025-06-30").unwrap();
    upsert_config(&db_path, "instrument_type_OLD.AX", "ETF").unwrap();

    rename_holdings_symbol(&db_path, "OLD.AX", "NEW.AX").unwrap();

    let config: HashMap<String, String> = load_config(&db_path).unwrap().into_iter().map(|c| (c.key, c.value)).collect();
    assert_eq!(config.get("manual_price_NEW.AX").map(String::as_str), Some("0.42"));
    assert_eq!(config.get("dead_symbol_NEW.AX").map(String::as_str), Some("2025-06-30"));
    assert_eq!(config.get("instrument_type_NEW.AX").map(String::as_str), Some("ETF"));
    assert!(!config.keys().any(|k| k.ends_with("OLD.AX")), "nothing is left under the old name");
}

/// The same dividend recorded from two overlapping refreshes must be
/// booked once.
#[test]
fn overlapping_dividend_recording_books_each_payment_once() {
    let (_file, db_path) = setup_test_db();
    seed_cash_account(&db_path, 1, "Everyday AUD", "AUD");
    upsert_config(&db_path, "dividend_account_AUD", "1").unwrap();
    insert_tx(&db_path, 1, "purchase", "2025-01-02", 100.0, 10.0, 0.0);
    insert_dividend_event(&db_path, "2026-03-02", 0.50);

    let handles: Vec<_> = (0..4)
        .map(|_| {
            let path = db_path.clone();
            std::thread::spawn(move || record_new_dividends(&path).unwrap().recorded)
        })
        .collect();
    let recorded: usize = handles.into_iter().map(|h| h.join().unwrap()).sum();
    assert_eq!(recorded, 1);
    let conn = open_db(&db_path).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM holdings_transactions WHERE transaction_type = 'dividend'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1);
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
    div.withholding_amount = Some(Some(12.00));
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

/// A payment's withholding can be corrected back to none. It used to be
/// `COALESCE`d on edit, so once set it could never be cleared.
#[test]
fn a_withholding_amount_can_be_cleared_and_is_kept_when_not_sent() {
    let (_file, db_path) = setup_test_db();
    let stored = |id: i64| -> Option<f64> {
        open_db(&db_path)
            .unwrap()
            .query_row("SELECT withholding_amount FROM holdings_transactions WHERE id = ?1", params![id], |r| r.get(0))
            .unwrap()
    };
    let mut div = trade_payload("DMP.AX", "dividend", 50.0, 0.555);
    div.amount = Some(27.75);
    div.withholding_amount = Some(Some(12.0));
    let record = insert_holding_transaction(&db_path, "DMP.AX", div).unwrap();

    let mut note_only = trade_payload("DMP.AX", "dividend", 50.0, 0.555);
    note_only.amount = Some(27.75);
    modify_holding_transaction(&db_path, record.id, "DMP.AX", note_only).unwrap();
    assert_eq!(stored(record.id), Some(12.0), "absent keeps it");

    let mut cleared = trade_payload("DMP.AX", "dividend", 50.0, 0.555);
    cleared.amount = Some(27.75);
    cleared.withholding_amount = Some(None);
    modify_holding_transaction(&db_path, record.id, "DMP.AX", cleared).unwrap();
    assert_eq!(stored(record.id), None, "null clears it");

    let absent: NewHoldingTransaction =
        serde_json::from_str(r#"{"symbol":"X","transaction_type":"dividend","date":"2026-01-01"}"#).unwrap();
    let null: NewHoldingTransaction = serde_json::from_str(
        r#"{"symbol":"X","transaction_type":"dividend","date":"2026-01-01","withholding_amount":null}"#,
    )
    .unwrap();
    assert_eq!(absent.withholding_amount, None);
    assert_eq!(null.withholding_amount, Some(None));
}

/// A US dividend paid into an AUD account is converted at its own rate, and
/// so is the tax withheld from it: US$6 withheld is A$9 at 1.5, not A$6.
#[test]
fn foreign_withholding_is_converted_with_the_payment() {
    let (_file, db_path) = setup_test_db();
    seed_cash_account(&db_path, 1, "CommSec AUD", "AUD");

    let mut div = trade_payload("VTI", "dividend", 40.0, 1.5);
    div.currency = Some("USD".to_string());
    div.original_price = Some(1.0);
    div.fx_rate = Some(1.5);
    div.amount = Some(60.0);
    div.withholding_amount = Some(Some(6.0));
    div.cash_account_id = Some(1);
    let record = insert_holding_transaction(&db_path, "VTI", div).unwrap();

    let mut legs = cash_legs(&db_path, record.id);
    legs.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
    assert_eq!(legs, vec![(60.0, "dividend".to_string(), 1), (-9.0, "fee".to_string(), 1)]);
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

/// A refused settlement must leave nothing behind. The trade used to be
/// stored before the leg was attempted, so the user saw an error, fixed the
/// account, saved again — and held the purchase twice.
#[test]
fn a_refused_settlement_stores_no_trade() {
    let (_file, db_path) = setup_test_db();
    seed_cash_account(&db_path, 1, "Settlement USD", "USD");

    let mut buy = trade_payload("BHP.AX", "purchase", 10.0, 40.0);
    buy.currency = Some("AUD".to_string());
    buy.cash_account_id = Some(1);
    assert!(insert_holding_transaction(&db_path, "BHP.AX", buy).is_err());

    let conn = open_db(&db_path).unwrap();
    let stored: i64 = conn
        .query_row("SELECT COUNT(*) FROM holdings_transactions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(stored, 0, "the failed save must not leave the trade stored");
}

/// Editing a dividend's note sends no share count, price or currency — the
/// form only sends those for trades. They used to be overwritten with
/// nulls and the currency with AUD, so a USD dividend lost its rate and
/// its USD account then refused the "AUD" leg.
#[test]
fn editing_a_dividend_note_keeps_its_figures_and_currency() {
    let (_file, db_path) = setup_test_db();
    seed_cash_account(&db_path, 1, "Broker USD", "USD");

    let mut div = trade_payload("VTS.AX", "dividend", 40.0, 1.5);
    div.currency = Some("USD".to_string());
    div.original_price = Some(1.0);
    div.fx_rate = Some(1.5);
    div.amount = Some(60.0);
    div.cash_account_id = Some(1);
    let record = insert_holding_transaction(&db_path, "VTS.AX", div).unwrap();

    // What the web form sends for a dividend edit
    let mut edit = trade_payload("VTS.AX", "dividend", 0.0, 0.0);
    edit.quantity = None;
    edit.price = None;
    edit.amount = Some(60.0);
    edit.notes = Some("Q3 distribution".to_string());
    edit.cash_account_id = Some(1);
    let updated = modify_holding_transaction(&db_path, record.id, "VTS.AX", edit)
        .expect("a note edit must not trip the currency check");

    assert_eq!(updated.currency, "USD");
    assert_eq!(updated.quantity, Some(40.0));
    assert_eq!(updated.price, Some(1.5));
    assert_eq!(updated.original_price, Some(1.0));
    assert_eq!(updated.fx_rate, Some(1.5));
    assert_eq!(cash_legs(&db_path, record.id), vec![(40.0, "dividend".to_string(), 1)]);
}

/// Correcting a dividend's total keeps the share count and rescales the
/// per-share figures, so the USD leg follows the new amount.
#[test]
fn editing_a_dividend_total_rescales_its_per_share_figures() {
    let (_file, db_path) = setup_test_db();
    seed_cash_account(&db_path, 1, "Broker USD", "USD");

    let mut div = trade_payload("VTS.AX", "dividend", 40.0, 1.5);
    div.currency = Some("USD".to_string());
    div.original_price = Some(1.0);
    div.fx_rate = Some(1.5);
    div.amount = Some(60.0);
    div.cash_account_id = Some(1);
    let record = insert_holding_transaction(&db_path, "VTS.AX", div).unwrap();

    let mut edit = trade_payload("VTS.AX", "dividend", 0.0, 0.0);
    edit.quantity = None;
    edit.price = None;
    edit.amount = Some(90.0);
    edit.cash_account_id = Some(1);
    let updated = modify_holding_transaction(&db_path, record.id, "VTS.AX", edit).unwrap();

    assert_eq!(updated.quantity, Some(40.0));
    assert!((updated.price.unwrap() - 2.25).abs() < 1e-9);
    assert!((updated.original_price.unwrap() - 1.5).abs() < 1e-9);
    assert_eq!(cash_legs(&db_path, record.id), vec![(60.0, "dividend".to_string(), 1)]);
}

/// Moving a trade to AUD drops the foreign price and rate rather than
/// carrying a stale USD figure onto an AUD trade.
#[test]
fn editing_a_trade_into_aud_drops_its_foreign_figures() {
    let (_file, db_path) = setup_test_db();
    let mut buy = trade_payload("AAPL", "purchase", 10.0, 150.0);
    buy.currency = Some("USD".to_string());
    buy.original_price = Some(100.0);
    buy.fx_rate = Some(1.5);
    let record = insert_holding_transaction(&db_path, "AAPL", buy).unwrap();

    let mut edit = trade_payload("AAPL", "purchase", 10.0, 150.0);
    edit.currency = Some("AUD".to_string());
    let updated = modify_holding_transaction(&db_path, record.id, "AAPL", edit).unwrap();
    assert_eq!(updated.currency, "AUD");
    assert_eq!(updated.original_price, None);
    assert_eq!(updated.fx_rate, None);
}

/// The same on edit: a refused leg must leave the trade and its existing
/// leg exactly as they were, not half-rewritten.
#[test]
fn a_refused_edit_keeps_the_trade_and_its_leg() {
    let (_file, db_path) = setup_test_db();
    seed_cash_account(&db_path, 1, "Settlement AUD", "AUD");
    seed_cash_account(&db_path, 2, "Settlement USD", "USD");

    let mut buy = trade_payload("BHP.AX", "purchase", 100.0, 10.0);
    buy.cash_account_id = Some(1);
    let record = insert_holding_transaction(&db_path, "BHP.AX", buy).unwrap();

    let mut edit = trade_payload("BHP.AX", "purchase", 100.0, 11.0);
    edit.currency = Some("AUD".to_string());
    edit.cash_account_id = Some(2);
    assert!(modify_holding_transaction(&db_path, record.id, "BHP.AX", edit).is_err());

    let conn = open_db(&db_path).unwrap();
    let (price, account): (f64, i64) = conn
        .query_row(
            "SELECT price, cash_account_id FROM holdings_transactions WHERE id = ?1",
            params![record.id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((price, account), (10.0, 1), "the edit must not have been applied");
    assert_eq!(cash_legs(&db_path, record.id), vec![(-1000.0, "trade_buy".to_string(), 1)]);
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

/// Raising a sale above the shares held is the same over-sell as recording
/// one, and used to go through on edit without a word. The sale's own old
/// quantity is not counted against it.
#[actix_web::test]
async fn editing_a_sale_into_an_oversell_needs_confirmation() {
    let (_file, db_path) = setup_test_db();
    call_write(&db_path, "POST", "/api/holdings", Some(purchase_json("TST.AX", 10.0, 5.0))).await;
    let sale = |qty: f64, confirm: bool| serde_json::json!({
        "symbol": "TST.AX", "transaction_type": "sale",
        "date": "2026-06-01", "quantity": qty, "price": 6.0, "confirm": confirm
    });
    let (_, created) = call_write(&db_path, "POST", "/api/holdings", Some(sale(4.0, false))).await;
    let uri = format!("/api/holdings/{}", created["id"]);

    let (status, _) = call_write(&db_path, "PUT", &uri, Some(sale(10.0, false))).await;
    assert_eq!(status, 200, "selling all 10 is within the holding once the old 4 is set aside");

    let (status, body) = call_write(&db_path, "PUT", &uri, Some(sale(12.0, false))).await;
    assert_eq!(status, 409);
    assert!(close_to(&body["error"]["held"], 10.0));

    let (status, _) = call_write(&db_path, "PUT", &uri, Some(sale(12.0, true))).await;
    assert_eq!(status, 200);
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
        
        stmt.query_map([], |r| r.get(0)).unwrap().flatten().collect()
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

/// A client that sends only the lists must not erase the notes and price
/// levels it didn't mention.
#[actix_web::test]
async fn changing_only_the_lists_keeps_notes_and_levels() {
    let (_file, db_path) = setup_test_db();
    insert_watchlist_symbol(&db_path, "TST.AX", "Default", Some("keep me"), Some(6.5), Some(4.0), None).unwrap();

    let body = serde_json::json!({ "lists": ["Growth"] });
    let (status, _) = call_watchlist(&db_path, "PUT", "/api/watchlist/symbol/TST.AX", Some(body)).await;
    assert_eq!(status, 200);

    let conn = open_db(&db_path).unwrap();
    let row: (Option<String>, Option<f64>, Option<f64>) = conn
        .query_row(
            "SELECT notes, breakthrough_price, stop_loss_price FROM watchlist_symbols WHERE symbol = 'TST.AX'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(row, (Some("keep me".to_string()), Some(6.5), Some(4.0)));
    assert_eq!(membership_lists(&db_path, "TST.AX"), vec!["Growth"]);
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

    // A refresh abandoned part-way — the client went away, so actix
    // dropped the handler's future — must still release the lock, or every
    // refresh after it is skipped until restart. Dropping the guard is
    // that path. Kept in this test because the flag is process-global.
    let guard = RefreshGuard::acquire().expect("lock starts free");
    assert!(RefreshGuard::acquire().is_none(), "a second refresh is refused while one runs");
    drop(guard);
    assert!(
        !REFRESH_IN_FLIGHT.load(std::sync::atomic::Ordering::SeqCst),
        "dropping the guard frees the lock"
    );
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
async fn a_level_moves_to_a_new_price_but_a_trendline_needs_its_anchors() {
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
    // Moving only the first anchor's price would tilt the line.
    assert_eq!(actix_web::test::call_service(&app, mv(trend_id, 31.0)).await.status(), 400);

    let level_with_anchors = actix_web::test::TestRequest::patch()
        .uri(&format!("/api/chart-drawings/id/{id}"))
        .set_json(serde_json::json!({ "price": 40.0, "end_date": "2026-03-05" }))
        .to_request();
    assert_eq!(actix_web::test::call_service(&app, level_with_anchors).await.status(), 400);
}

/// Dragging either end of a trendline restates both anchors; a move that
/// would leave it with no slope or no end is refused like a new one.
#[actix_web::test]
async fn a_trendline_moves_by_restating_both_anchors() {
    let (_file, db_path) = setup_test_db();
    let app = actix_web::test::init_service(
        App::new()
            .app_data(web::Data::new(db_path.clone()))
            .service(add_chart_drawing)
            .service(move_chart_drawing),
    )
    .await;
    let req = actix_web::test::TestRequest::post()
        .uri("/api/chart-drawings/BHP.AX")
        .set_json(serde_json::json!({
            "kind": "trend", "price": 30.0, "label": "uptrend",
            "start_date": "2026-01-05", "end_date": "2026-03-05", "end_price": 42.5
        }))
        .to_request();
    let body: serde_json::Value = actix_web::test::call_and_read_body_json(&app, req).await;
    let id = body["drawings"][0]["id"].as_i64().unwrap();

    let mv = |body: serde_json::Value| {
        actix_web::test::TestRequest::patch()
            .uri(&format!("/api/chart-drawings/id/{id}"))
            .set_json(body)
            .to_request()
    };
    for (case, body) in [
        ("no end anchor", serde_json::json!({ "price": 31.0, "start_date": "2026-01-05" })),
        ("zero end price", serde_json::json!({ "price": 31.0, "start_date": "2026-01-05", "end_date": "2026-03-05", "end_price": 0.0 })),
        ("same date twice", serde_json::json!({ "price": 31.0, "start_date": "2026-03-05", "end_date": "2026-03-05", "end_price": 40.0 })),
    ] {
        assert_eq!(actix_web::test::call_service(&app, mv(body)).await.status(), 400, "{case} should be refused");
    }

    let body: serde_json::Value = actix_web::test::call_and_read_body_json(&app, mv(serde_json::json!({
        "price": 29.0, "start_date": "2026-01-12", "end_date": "2026-04-01", "end_price": 45.0
    }))).await;
    let rows = body["drawings"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    let d = &rows[0];
    assert_eq!(d["id"], id);
    assert_eq!(d["kind"], "trend");
    assert_eq!(d["label"], "uptrend");
    assert_eq!((d["price"].as_f64(), d["start_date"].as_str()), (Some(29.0), Some("2026-01-12")));
    assert_eq!((d["end_price"].as_f64(), d["end_date"].as_str()), (Some(45.0), Some("2026-04-01")));

    let audited: String = open_db(&db_path).unwrap()
        .query_row(
            "SELECT json_extract(old_values, '$.end_date') || ' -> ' || json_extract(new_values, '$.end_date')
               FROM audit_log WHERE table_name = 'chart_drawings' AND action = 'UPDATE'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(audited, "2026-03-05 -> 2026-04-01");
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

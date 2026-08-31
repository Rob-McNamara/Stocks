//! Database schema: table creation, column migrations and the audit triggers.
//!
//! Split out of `main.rs` because it is the one part of the API a change to the
//! data model always has to touch, and CLAUDE.md sends readers here for the
//! audit-trigger rules. Finding it meant scrolling past eight thousand lines of
//! handlers.
//!
//! `init_db` runs on every start and is idempotent: `CREATE TABLE IF NOT
//! EXISTS`, `add_column_if_missing`, and triggers dropped immediately before
//! being recreated so an edit reaches a database that already has them.

use chrono::Utc;
use rusqlite::{params, Connection};
use std::path::PathBuf;

use crate::{open_db, DEFAULT_SECTORS_JSON};

pub fn init_db(path: &PathBuf) -> Result<(), String> {
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

    // Retention: the operational log grows on every refresh and would
    // otherwise dominate the database over time. Pruned at API startup.
    // (Timestamps compare lexicographically; format differences beyond the
    // date portion don't matter at a 90-day horizon.)
    let pruned_events = conn
        .execute(
            "DELETE FROM event_log WHERE timestamp < strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-90 days')",
            [],
        )
        .map_err(|err| err.to_string())?;

    // User-drawn price levels on a symbol's chart.
    //
    // Anchored by price, never by pixel: the chart re-maps for six timeframes,
    // a Day/Week interval and a native/AUD toggle, so a stored screen position
    // would be wrong the moment any of those changed. The price is in the
    // symbol's own currency for the same reason the purchase markers are —
    // the chart applies the FX rate itself at render.
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS chart_drawings (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            symbol TEXT NOT NULL,
            kind TEXT NOT NULL,
            price REAL NOT NULL,
            label TEXT,
            colour TEXT,
            created_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_chart_drawings_symbol ON chart_drawings(symbol);",
    )
    .map_err(|err| err.to_string())?;

    // Dividend events the user has deliberately declined to record. Without
    // this, automatic recording resurrects them on the next refresh — deleting
    // a dividend has to mean "not this one", not "record it again shortly".
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS dividend_exclusions (
            symbol TEXT NOT NULL,
            ex_date TEXT NOT NULL,
            reason TEXT,
            created_at TEXT NOT NULL,
            PRIMARY KEY (symbol, ex_date)
        );",
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

    // Cash accounts and their ledger.
    //
    // A balance is never stored — it is SUM(amount) up to a date, the same way
    // positions are derived from holdings_transactions rather than cached. That
    // keeps a back-dated entry correct instead of silently stale.
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS cash_accounts (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL UNIQUE,
            currency TEXT NOT NULL,
            -- Nominal annual rate, for display and projection only. Interest is
            -- recorded as it is actually credited, not accrued from this.
            interest_rate REAL,
            -- Whether this account counts toward portfolio value and returns.
            -- An everyday savings account can be tracked without treating its
            -- balance as invested capital.
            include_in_portfolio INTEGER NOT NULL DEFAULT 1,
            notes TEXT,
            created_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS cash_transactions (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            account_id INTEGER NOT NULL REFERENCES cash_accounts(id),
            date TEXT NOT NULL,
            -- Signed, in the account's own currency: positive is money in.
            amount REAL NOT NULL,
            -- One of CASH_TX_KINDS; validated on write rather than by a CHECK
            -- constraint, which could only be changed by rebuilding the table.
            kind TEXT NOT NULL,
            -- The trade this leg settles, when kind is trade_buy/trade_sell.
            holding_tx_id INTEGER REFERENCES holdings_transactions(id),
            -- Pairs the two legs of a currency conversion, which move value
            -- between accounts without any money entering or leaving.
            transfer_group_id TEXT,
            notes TEXT,
            created_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_cash_tx_account_date ON cash_transactions(account_id, date);
        CREATE INDEX IF NOT EXISTS idx_cash_tx_date ON cash_transactions(date);
        CREATE INDEX IF NOT EXISTS idx_cash_tx_holding ON cash_transactions(holding_tx_id);
        CREATE INDEX IF NOT EXISTS idx_cash_tx_transfer ON cash_transactions(transfer_group_id);",
    )
    .map_err(|err| err.to_string())?;

    // Migrate: add columns if they don't exist
    add_column_if_missing(&conn, "holdings_transactions", "brokerage", "REAL")?;
    // Which cash account a trade settles against — the source of funds on a
    // buy, the destination on a sale.
    add_column_if_missing(&conn, "holdings_transactions", "cash_account_id", "INTEGER")?;
    // Tax withheld at source on this specific payment, in the trade's currency.
    // Distinct from `symbol_info.dividend_withholding_pct`: that is a standing
    // rate for a symbol (US funds withhold on every distribution), whereas this
    // is a one-off amount, such as TFN withholding on the unfranked portion of
    // a single distribution, which varies with each payment's franking and
    // stops once a TFN is quoted.
    add_column_if_missing(&conn, "holdings_transactions", "withholding_amount", "REAL")?;
    // A trendline's two anchors. `price` above is the first anchor's price, so
    // an existing horizontal level needs no migration: its extra columns are
    // simply null.
    add_column_if_missing(&conn, "chart_drawings", "start_date", "TEXT")?;
    add_column_if_missing(&conn, "chart_drawings", "end_date", "TEXT")?;
    add_column_if_missing(&conn, "chart_drawings", "end_price", "REAL")?;
    add_column_if_missing(&conn, "holdings_transactions", "currency", "TEXT NOT NULL DEFAULT 'AUD'")?;
    add_column_if_missing(&conn, "holdings_transactions", "original_price", "REAL")?;
    add_column_if_missing(&conn, "holdings_transactions", "fx_rate", "REAL")?;
    add_column_if_missing(&conn, "symbol_info", "currency", "TEXT")?;
    // Non-resident withholding deducted before the cash reaches the account.
    // US-domiciled funds (VEU.AX, VTS.AX) withhold 30% statutory / 15% by treaty,
    // and Yahoo reports the gross distribution, so without this the ledger
    // credits money that never arrived.
    add_column_if_missing(&conn, "symbol_info", "dividend_withholding_pct", "REAL")?;
    // Session OHLC for the cached live price, so today's candle is complete
    // before the daily bar lands in `prices`.
    add_column_if_missing(&conn, "cached_current_prices", "day_open", "REAL")?;
    add_column_if_missing(&conn, "cached_current_prices", "day_high", "REAL")?;
    add_column_if_missing(&conn, "cached_current_prices", "day_low", "REAL")?;

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

    // The presence of watchlist_memberships means the normalised two-table
    // design is already in place. Both legacy rebuilds below drop every column
    // they don't explicitly copy — notes, breakthrough_price, stop_loss_price —
    // so once we are normalised they must never run again. Step 1's guard is
    // "list_name is absent", which is *also* true of the normalised table, so
    // without this check every API restart silently wiped those three columns.
    let has_memberships = conn
        .query_row("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='watchlist_memberships'", [], |row| row.get::<_, i64>(0))
        .unwrap_or(0) > 0;

    // Step 1: old single-column table → multi-list table (legacy migration)
    if !has_memberships && !cols.contains(&"list_name".to_string()) {
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

    // Retention: audit rows carry full old/new JSON per change and every
    // refresh stamps app_config (three trigger rows each). One year of
    // history is plenty for a personal audit trail.
    let pruned_audit = conn
        .execute(
            "DELETE FROM audit_log WHERE timestamp < strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-365 days')",
            [],
        )
        .map_err(|err| err.to_string())?;
    if pruned_events > 0 || pruned_audit > 0 {
        let now = Utc::now().to_rfc3339();
        let _ = conn.execute(
            "INSERT INTO event_log (timestamp, level, source, event_type, symbol, details) VALUES (?1, 'info', 'api', 'log_retention', NULL, ?2)",
            params![now, format!("Pruned {} event_log row(s) older than 90 days and {} audit_log row(s) older than 365 days", pruned_events, pruned_audit)],
        );
    }

    // Drop every audit trigger before recreating it below. The CREATE
    // statements are `IF NOT EXISTS`, so without this an edit to a trigger's
    // column list would silently never reach a database where the trigger
    // already exists — which is how breakthrough_price and stop_loss_price
    // went unrecorded for months. Dropping first makes this code the single
    // source of truth: triggers hold no state, so recreating them is free.
    let drop_trigger_sql = "
        DROP TRIGGER IF EXISTS audit_watchlist_symbols_insert;
        DROP TRIGGER IF EXISTS audit_watchlist_symbols_update;
        DROP TRIGGER IF EXISTS audit_watchlist_symbols_delete;
        DROP TRIGGER IF EXISTS audit_watchlist_memberships_insert;
        DROP TRIGGER IF EXISTS audit_watchlist_memberships_update;
        DROP TRIGGER IF EXISTS audit_watchlist_memberships_delete;
        DROP TRIGGER IF EXISTS audit_watchlist_symbol_fields_insert;
        DROP TRIGGER IF EXISTS audit_watchlist_symbol_fields_update;
        DROP TRIGGER IF EXISTS audit_watchlist_symbol_fields_delete;
        DROP TRIGGER IF EXISTS audit_holdings_transactions_insert;
        DROP TRIGGER IF EXISTS audit_holdings_transactions_update;
        DROP TRIGGER IF EXISTS audit_holdings_transactions_delete;
        DROP TRIGGER IF EXISTS audit_holdings_custom_fields_insert;
        DROP TRIGGER IF EXISTS audit_holdings_custom_fields_update;
        DROP TRIGGER IF EXISTS audit_holdings_custom_fields_delete;
        DROP TRIGGER IF EXISTS audit_holdings_symbol_fields_insert;
        DROP TRIGGER IF EXISTS audit_holdings_symbol_fields_update;
        DROP TRIGGER IF EXISTS audit_holdings_symbol_fields_delete;
        DROP TRIGGER IF EXISTS audit_app_config_insert;
        DROP TRIGGER IF EXISTS audit_app_config_update;
        DROP TRIGGER IF EXISTS audit_app_config_delete;
        DROP TRIGGER IF EXISTS audit_chart_drawings_insert;
        DROP TRIGGER IF EXISTS audit_chart_drawings_update;
        DROP TRIGGER IF EXISTS audit_chart_drawings_delete;
        DROP TRIGGER IF EXISTS audit_dividend_exclusions_insert;
        DROP TRIGGER IF EXISTS audit_dividend_exclusions_update;
        DROP TRIGGER IF EXISTS audit_dividend_exclusions_delete;
        DROP TRIGGER IF EXISTS audit_dividend_events_insert;
        DROP TRIGGER IF EXISTS audit_dividend_events_update;
        DROP TRIGGER IF EXISTS audit_dividend_events_delete;
        DROP TRIGGER IF EXISTS audit_symbol_info_insert;
        DROP TRIGGER IF EXISTS audit_symbol_info_update;
        DROP TRIGGER IF EXISTS audit_symbol_info_delete;
        DROP TRIGGER IF EXISTS audit_stock_analysis_messages_insert;
        DROP TRIGGER IF EXISTS audit_stock_analysis_messages_update;
        DROP TRIGGER IF EXISTS audit_stock_analysis_messages_delete;
        DROP TRIGGER IF EXISTS audit_cash_accounts_insert;
        DROP TRIGGER IF EXISTS audit_cash_accounts_update;
        DROP TRIGGER IF EXISTS audit_cash_accounts_delete;
        DROP TRIGGER IF EXISTS audit_cash_transactions_insert;
        DROP TRIGGER IF EXISTS audit_cash_transactions_update;
        DROP TRIGGER IF EXISTS audit_cash_transactions_delete;
    ";
    conn.execute_batch(drop_trigger_sql).map_err(|err| err.to_string())?;

    // Create triggers for all tracked tables.
    // Each trigger captures old/new values as JSON. Every column of the table
    // must appear here — see the Audit Logging rule in CLAUDE.md. The
    // `audit_triggers_cover_every_column` test enforces this.
    let trigger_sql = "
        -- watchlist_symbols
        CREATE TRIGGER IF NOT EXISTS audit_watchlist_symbols_insert AFTER INSERT ON watchlist_symbols
        BEGIN
            INSERT INTO audit_log (timestamp, table_name, action, row_id, old_values, new_values)
            VALUES (strftime('%Y-%m-%dT%H:%M:%fZ','now'), 'watchlist_symbols', 'INSERT', NEW.symbol, NULL,
                json_object('id', NEW.id, 'symbol', NEW.symbol, 'notes', NEW.notes, 'updated_at', NEW.updated_at,
                    'breakthrough_price', NEW.breakthrough_price, 'stop_loss_price', NEW.stop_loss_price));
        END;
        CREATE TRIGGER IF NOT EXISTS audit_watchlist_symbols_update AFTER UPDATE ON watchlist_symbols
        BEGIN
            INSERT INTO audit_log (timestamp, table_name, action, row_id, old_values, new_values)
            VALUES (strftime('%Y-%m-%dT%H:%M:%fZ','now'), 'watchlist_symbols', 'UPDATE', NEW.symbol,
                json_object('id', OLD.id, 'symbol', OLD.symbol, 'notes', OLD.notes, 'updated_at', OLD.updated_at,
                    'breakthrough_price', OLD.breakthrough_price, 'stop_loss_price', OLD.stop_loss_price),
                json_object('id', NEW.id, 'symbol', NEW.symbol, 'notes', NEW.notes, 'updated_at', NEW.updated_at,
                    'breakthrough_price', NEW.breakthrough_price, 'stop_loss_price', NEW.stop_loss_price));
        END;
        CREATE TRIGGER IF NOT EXISTS audit_watchlist_symbols_delete AFTER DELETE ON watchlist_symbols
        BEGIN
            INSERT INTO audit_log (timestamp, table_name, action, row_id, old_values, new_values)
            VALUES (strftime('%Y-%m-%dT%H:%M:%fZ','now'), 'watchlist_symbols', 'DELETE', OLD.symbol,
                json_object('id', OLD.id, 'symbol', OLD.symbol, 'notes', OLD.notes, 'updated_at', OLD.updated_at,
                    'breakthrough_price', OLD.breakthrough_price, 'stop_loss_price', OLD.stop_loss_price), NULL);
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
                    'notes', NEW.notes, 'currency', NEW.currency, 'original_price', NEW.original_price, 'fx_rate', NEW.fx_rate, 'created_at', NEW.created_at, 'cash_account_id', NEW.cash_account_id, 'withholding_amount', NEW.withholding_amount));
        END;
        CREATE TRIGGER IF NOT EXISTS audit_holdings_transactions_update AFTER UPDATE ON holdings_transactions
        BEGIN
            INSERT INTO audit_log (timestamp, table_name, action, row_id, old_values, new_values)
            VALUES (strftime('%Y-%m-%dT%H:%M:%fZ','now'), 'holdings_transactions', 'UPDATE', CAST(NEW.id AS TEXT),
                json_object('id', OLD.id, 'symbol', OLD.symbol, 'transaction_type', OLD.transaction_type, 'date', OLD.date,
                    'quantity', OLD.quantity, 'price', OLD.price, 'amount', OLD.amount, 'brokerage', OLD.brokerage,
                    'notes', OLD.notes, 'currency', OLD.currency, 'original_price', OLD.original_price, 'fx_rate', OLD.fx_rate, 'created_at', OLD.created_at, 'cash_account_id', OLD.cash_account_id, 'withholding_amount', OLD.withholding_amount),
                json_object('id', NEW.id, 'symbol', NEW.symbol, 'transaction_type', NEW.transaction_type, 'date', NEW.date,
                    'quantity', NEW.quantity, 'price', NEW.price, 'amount', NEW.amount, 'brokerage', NEW.brokerage,
                    'notes', NEW.notes, 'currency', NEW.currency, 'original_price', NEW.original_price, 'fx_rate', NEW.fx_rate, 'created_at', NEW.created_at, 'cash_account_id', NEW.cash_account_id, 'withholding_amount', NEW.withholding_amount));
        END;
        CREATE TRIGGER IF NOT EXISTS audit_holdings_transactions_delete AFTER DELETE ON holdings_transactions
        BEGIN
            INSERT INTO audit_log (timestamp, table_name, action, row_id, old_values, new_values)
            VALUES (strftime('%Y-%m-%dT%H:%M:%fZ','now'), 'holdings_transactions', 'DELETE', CAST(OLD.id AS TEXT),
                json_object('id', OLD.id, 'symbol', OLD.symbol, 'transaction_type', OLD.transaction_type, 'date', OLD.date,
                    'quantity', OLD.quantity, 'price', OLD.price, 'amount', OLD.amount, 'brokerage', OLD.brokerage,
                    'notes', OLD.notes, 'currency', OLD.currency, 'original_price', OLD.original_price, 'fx_rate', OLD.fx_rate, 'created_at', OLD.created_at, 'cash_account_id', OLD.cash_account_id, 'withholding_amount', OLD.withholding_amount), NULL);
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
                json_object('id', NEW.id, 'symbol', NEW.symbol, 'ex_date', NEW.ex_date, 'amount', NEW.amount,
                    'payment_date', NEW.payment_date, 'record_date', NEW.record_date, 'fetched_at', NEW.fetched_at));
        END;
        CREATE TRIGGER IF NOT EXISTS audit_dividend_events_update AFTER UPDATE ON dividend_events
        BEGIN
            INSERT INTO audit_log (timestamp, table_name, action, row_id, old_values, new_values)
            VALUES (strftime('%Y-%m-%dT%H:%M:%fZ','now'), 'dividend_events', 'UPDATE', CAST(NEW.id AS TEXT),
                json_object('id', OLD.id, 'symbol', OLD.symbol, 'ex_date', OLD.ex_date, 'amount', OLD.amount,
                    'payment_date', OLD.payment_date, 'record_date', OLD.record_date, 'fetched_at', OLD.fetched_at),
                json_object('id', NEW.id, 'symbol', NEW.symbol, 'ex_date', NEW.ex_date, 'amount', NEW.amount,
                    'payment_date', NEW.payment_date, 'record_date', NEW.record_date, 'fetched_at', NEW.fetched_at));
        END;
        CREATE TRIGGER IF NOT EXISTS audit_dividend_events_delete AFTER DELETE ON dividend_events
        BEGIN
            INSERT INTO audit_log (timestamp, table_name, action, row_id, old_values, new_values)
            VALUES (strftime('%Y-%m-%dT%H:%M:%fZ','now'), 'dividend_events', 'DELETE', CAST(OLD.id AS TEXT),
                json_object('id', OLD.id, 'symbol', OLD.symbol, 'ex_date', OLD.ex_date, 'amount', OLD.amount,
                    'payment_date', OLD.payment_date, 'record_date', OLD.record_date, 'fetched_at', OLD.fetched_at), NULL);
        END;

        -- cash_accounts
        CREATE TRIGGER IF NOT EXISTS audit_cash_accounts_insert AFTER INSERT ON cash_accounts
        BEGIN
            INSERT INTO audit_log (timestamp, table_name, action, row_id, old_values, new_values)
            VALUES (strftime('%Y-%m-%dT%H:%M:%fZ','now'), 'cash_accounts', 'INSERT', CAST(NEW.id AS TEXT), NULL,
                json_object('id', NEW.id, 'name', NEW.name, 'currency', NEW.currency,
                    'interest_rate', NEW.interest_rate, 'include_in_portfolio', NEW.include_in_portfolio,
                    'notes', NEW.notes, 'created_at', NEW.created_at));
        END;
        CREATE TRIGGER IF NOT EXISTS audit_cash_accounts_update AFTER UPDATE ON cash_accounts
        BEGIN
            INSERT INTO audit_log (timestamp, table_name, action, row_id, old_values, new_values)
            VALUES (strftime('%Y-%m-%dT%H:%M:%fZ','now'), 'cash_accounts', 'UPDATE', CAST(NEW.id AS TEXT),
                json_object('id', OLD.id, 'name', OLD.name, 'currency', OLD.currency,
                    'interest_rate', OLD.interest_rate, 'include_in_portfolio', OLD.include_in_portfolio,
                    'notes', OLD.notes, 'created_at', OLD.created_at),
                json_object('id', NEW.id, 'name', NEW.name, 'currency', NEW.currency,
                    'interest_rate', NEW.interest_rate, 'include_in_portfolio', NEW.include_in_portfolio,
                    'notes', NEW.notes, 'created_at', NEW.created_at));
        END;
        CREATE TRIGGER IF NOT EXISTS audit_cash_accounts_delete AFTER DELETE ON cash_accounts
        BEGIN
            INSERT INTO audit_log (timestamp, table_name, action, row_id, old_values, new_values)
            VALUES (strftime('%Y-%m-%dT%H:%M:%fZ','now'), 'cash_accounts', 'DELETE', CAST(OLD.id AS TEXT),
                json_object('id', OLD.id, 'name', OLD.name, 'currency', OLD.currency,
                    'interest_rate', OLD.interest_rate, 'include_in_portfolio', OLD.include_in_portfolio,
                    'notes', OLD.notes, 'created_at', OLD.created_at), NULL);
        END;

        -- cash_transactions
        CREATE TRIGGER IF NOT EXISTS audit_cash_transactions_insert AFTER INSERT ON cash_transactions
        BEGIN
            INSERT INTO audit_log (timestamp, table_name, action, row_id, old_values, new_values)
            VALUES (strftime('%Y-%m-%dT%H:%M:%fZ','now'), 'cash_transactions', 'INSERT', CAST(NEW.id AS TEXT), NULL,
                json_object('id', NEW.id, 'account_id', NEW.account_id, 'date', NEW.date,
                    'amount', NEW.amount, 'kind', NEW.kind, 'holding_tx_id', NEW.holding_tx_id,
                    'transfer_group_id', NEW.transfer_group_id, 'notes', NEW.notes, 'created_at', NEW.created_at));
        END;
        CREATE TRIGGER IF NOT EXISTS audit_cash_transactions_update AFTER UPDATE ON cash_transactions
        BEGIN
            INSERT INTO audit_log (timestamp, table_name, action, row_id, old_values, new_values)
            VALUES (strftime('%Y-%m-%dT%H:%M:%fZ','now'), 'cash_transactions', 'UPDATE', CAST(NEW.id AS TEXT),
                json_object('id', OLD.id, 'account_id', OLD.account_id, 'date', OLD.date,
                    'amount', OLD.amount, 'kind', OLD.kind, 'holding_tx_id', OLD.holding_tx_id,
                    'transfer_group_id', OLD.transfer_group_id, 'notes', OLD.notes, 'created_at', OLD.created_at),
                json_object('id', NEW.id, 'account_id', NEW.account_id, 'date', NEW.date,
                    'amount', NEW.amount, 'kind', NEW.kind, 'holding_tx_id', NEW.holding_tx_id,
                    'transfer_group_id', NEW.transfer_group_id, 'notes', NEW.notes, 'created_at', NEW.created_at));
        END;
        CREATE TRIGGER IF NOT EXISTS audit_cash_transactions_delete AFTER DELETE ON cash_transactions
        BEGIN
            INSERT INTO audit_log (timestamp, table_name, action, row_id, old_values, new_values)
            VALUES (strftime('%Y-%m-%dT%H:%M:%fZ','now'), 'cash_transactions', 'DELETE', CAST(OLD.id AS TEXT),
                json_object('id', OLD.id, 'account_id', OLD.account_id, 'date', OLD.date,
                    'amount', OLD.amount, 'kind', OLD.kind, 'holding_tx_id', OLD.holding_tx_id,
                    'transfer_group_id', OLD.transfer_group_id, 'notes', OLD.notes, 'created_at', OLD.created_at), NULL);
        END;

        -- chart_drawings
        CREATE TRIGGER IF NOT EXISTS audit_chart_drawings_insert AFTER INSERT ON chart_drawings
        BEGIN
            INSERT INTO audit_log (timestamp, table_name, action, row_id, old_values, new_values)
            VALUES (strftime('%Y-%m-%dT%H:%M:%fZ','now'), 'chart_drawings', 'INSERT', CAST(NEW.id AS TEXT), NULL,
                json_object('id', NEW.id, 'symbol', NEW.symbol, 'kind', NEW.kind, 'price', NEW.price,
                    'label', NEW.label, 'colour', NEW.colour, 'start_date', NEW.start_date, 'end_date', NEW.end_date, 'end_price', NEW.end_price, 'created_at', NEW.created_at));
        END;
        CREATE TRIGGER IF NOT EXISTS audit_chart_drawings_update AFTER UPDATE ON chart_drawings
        BEGIN
            INSERT INTO audit_log (timestamp, table_name, action, row_id, old_values, new_values)
            VALUES (strftime('%Y-%m-%dT%H:%M:%fZ','now'), 'chart_drawings', 'UPDATE', CAST(NEW.id AS TEXT),
                json_object('id', OLD.id, 'symbol', OLD.symbol, 'kind', OLD.kind, 'price', OLD.price,
                    'label', OLD.label, 'colour', OLD.colour, 'start_date', OLD.start_date, 'end_date', OLD.end_date, 'end_price', OLD.end_price, 'created_at', OLD.created_at),
                json_object('id', NEW.id, 'symbol', NEW.symbol, 'kind', NEW.kind, 'price', NEW.price,
                    'label', NEW.label, 'colour', NEW.colour, 'start_date', NEW.start_date, 'end_date', NEW.end_date, 'end_price', NEW.end_price, 'created_at', NEW.created_at));
        END;
        CREATE TRIGGER IF NOT EXISTS audit_chart_drawings_delete AFTER DELETE ON chart_drawings
        BEGIN
            INSERT INTO audit_log (timestamp, table_name, action, row_id, old_values, new_values)
            VALUES (strftime('%Y-%m-%dT%H:%M:%fZ','now'), 'chart_drawings', 'DELETE', CAST(OLD.id AS TEXT),
                json_object('id', OLD.id, 'symbol', OLD.symbol, 'kind', OLD.kind, 'price', OLD.price,
                    'label', OLD.label, 'colour', OLD.colour, 'start_date', OLD.start_date, 'end_date', OLD.end_date, 'end_price', OLD.end_price, 'created_at', OLD.created_at), NULL);
        END;

        -- dividend_exclusions
        CREATE TRIGGER IF NOT EXISTS audit_dividend_exclusions_insert AFTER INSERT ON dividend_exclusions
        BEGIN
            INSERT INTO audit_log (timestamp, table_name, action, row_id, old_values, new_values)
            VALUES (strftime('%Y-%m-%dT%H:%M:%fZ','now'), 'dividend_exclusions', 'INSERT', NEW.symbol || ':' || NEW.ex_date, NULL,
                json_object('symbol', NEW.symbol, 'ex_date', NEW.ex_date, 'reason', NEW.reason, 'created_at', NEW.created_at));
        END;
        CREATE TRIGGER IF NOT EXISTS audit_dividend_exclusions_update AFTER UPDATE ON dividend_exclusions
        BEGIN
            INSERT INTO audit_log (timestamp, table_name, action, row_id, old_values, new_values)
            VALUES (strftime('%Y-%m-%dT%H:%M:%fZ','now'), 'dividend_exclusions', 'UPDATE', NEW.symbol || ':' || NEW.ex_date,
                json_object('symbol', OLD.symbol, 'ex_date', OLD.ex_date, 'reason', OLD.reason, 'created_at', OLD.created_at),
                json_object('symbol', NEW.symbol, 'ex_date', NEW.ex_date, 'reason', NEW.reason, 'created_at', NEW.created_at));
        END;
        CREATE TRIGGER IF NOT EXISTS audit_dividend_exclusions_delete AFTER DELETE ON dividend_exclusions
        BEGIN
            INSERT INTO audit_log (timestamp, table_name, action, row_id, old_values, new_values)
            VALUES (strftime('%Y-%m-%dT%H:%M:%fZ','now'), 'dividend_exclusions', 'DELETE', OLD.symbol || ':' || OLD.ex_date,
                json_object('symbol', OLD.symbol, 'ex_date', OLD.ex_date, 'reason', OLD.reason, 'created_at', OLD.created_at), NULL);
        END;

        -- symbol_info
        CREATE TRIGGER IF NOT EXISTS audit_symbol_info_insert AFTER INSERT ON symbol_info
        BEGIN
            INSERT INTO audit_log (timestamp, table_name, action, row_id, old_values, new_values)
            VALUES (strftime('%Y-%m-%dT%H:%M:%fZ','now'), 'symbol_info', 'INSERT', NEW.symbol, NULL,
                json_object('symbol', NEW.symbol, 'instrument_type', NEW.instrument_type, 'long_name', NEW.long_name,
                    'currency', NEW.currency, 'dividend_withholding_pct', NEW.dividend_withholding_pct, 'updated_at', NEW.updated_at));
        END;
        CREATE TRIGGER IF NOT EXISTS audit_symbol_info_update AFTER UPDATE ON symbol_info
        BEGIN
            INSERT INTO audit_log (timestamp, table_name, action, row_id, old_values, new_values)
            VALUES (strftime('%Y-%m-%dT%H:%M:%fZ','now'), 'symbol_info', 'UPDATE', NEW.symbol,
                json_object('symbol', OLD.symbol, 'instrument_type', OLD.instrument_type, 'long_name', OLD.long_name,
                    'currency', OLD.currency, 'dividend_withholding_pct', OLD.dividend_withholding_pct, 'updated_at', OLD.updated_at),
                json_object('symbol', NEW.symbol, 'instrument_type', NEW.instrument_type, 'long_name', NEW.long_name,
                    'currency', NEW.currency, 'dividend_withholding_pct', NEW.dividend_withholding_pct, 'updated_at', NEW.updated_at));
        END;
        CREATE TRIGGER IF NOT EXISTS audit_symbol_info_delete AFTER DELETE ON symbol_info
        BEGIN
            INSERT INTO audit_log (timestamp, table_name, action, row_id, old_values, new_values)
            VALUES (strftime('%Y-%m-%dT%H:%M:%fZ','now'), 'symbol_info', 'DELETE', OLD.symbol,
                json_object('symbol', OLD.symbol, 'instrument_type', OLD.instrument_type, 'long_name', OLD.long_name,
                    'currency', OLD.currency, 'dividend_withholding_pct', OLD.dividend_withholding_pct, 'updated_at', OLD.updated_at), NULL);
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

    // Audit triggers for stock_analysis_messages. These live here rather than
    // in the main trigger batch because the table is created after it, and a
    // trigger cannot be created before its table exists. The corresponding
    // DROPs are in drop_trigger_sql above.
    conn.execute_batch(
        "CREATE TRIGGER IF NOT EXISTS audit_stock_analysis_messages_insert AFTER INSERT ON stock_analysis_messages
        BEGIN
            INSERT INTO audit_log (timestamp, table_name, action, row_id, old_values, new_values)
            VALUES (strftime('%Y-%m-%dT%H:%M:%fZ','now'), 'stock_analysis_messages', 'INSERT', CAST(NEW.id AS TEXT), NULL,
                json_object('id', NEW.id, 'symbol', NEW.symbol, 'role', NEW.role, 'content', NEW.content,
                    'model_used', NEW.model_used, 'created_at', NEW.created_at));
        END;
        CREATE TRIGGER IF NOT EXISTS audit_stock_analysis_messages_update AFTER UPDATE ON stock_analysis_messages
        BEGIN
            INSERT INTO audit_log (timestamp, table_name, action, row_id, old_values, new_values)
            VALUES (strftime('%Y-%m-%dT%H:%M:%fZ','now'), 'stock_analysis_messages', 'UPDATE', CAST(NEW.id AS TEXT),
                json_object('id', OLD.id, 'symbol', OLD.symbol, 'role', OLD.role, 'content', OLD.content,
                    'model_used', OLD.model_used, 'created_at', OLD.created_at),
                json_object('id', NEW.id, 'symbol', NEW.symbol, 'role', NEW.role, 'content', NEW.content,
                    'model_used', NEW.model_used, 'created_at', NEW.created_at));
        END;
        CREATE TRIGGER IF NOT EXISTS audit_stock_analysis_messages_delete AFTER DELETE ON stock_analysis_messages
        BEGIN
            INSERT INTO audit_log (timestamp, table_name, action, row_id, old_values, new_values)
            VALUES (strftime('%Y-%m-%dT%H:%M:%fZ','now'), 'stock_analysis_messages', 'DELETE', CAST(OLD.id AS TEXT),
                json_object('id', OLD.id, 'symbol', OLD.symbol, 'role', OLD.role, 'content', OLD.content,
                    'model_used', OLD.model_used, 'created_at', OLD.created_at), NULL);
        END;",
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
        .query_map([], |row| row.get::<_, String>(1))
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

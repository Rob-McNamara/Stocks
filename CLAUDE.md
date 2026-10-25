# Stocks App — Coding Rules

## Database Schema

### Watchlist tables
The watchlist uses a **two-table normalised design**. Do not collapse these back into a single table.

- `watchlist_symbols` — one row per symbol. Holds per-symbol data (notes, updated_at).
- `watchlist_memberships` — one row per symbol/list pair. Holds the list name and added date.

```sql
watchlist_symbols     (id, symbol UNIQUE, notes, updated_at)
watchlist_memberships (id, symbol, list_name, added_at)  -- symbol → watchlist_symbols.symbol (logical link, not a declared FK)
```

When querying watchlist data always JOIN the two tables. When a membership is deleted and no memberships remain for that symbol, also delete the `watchlist_symbols` row.

The link is not declared as a foreign key, and SQLite foreign keys are not enabled (`PRAGMA foreign_keys` is never set), so nothing in the database enforces it — the code that adds and removes memberships does. Keep that cleanup in the same transaction as the membership change.

## Audit Logging

Every new table and every new column **must** be added to the audit triggers in the same change that introduces it. The triggers live in `init_db()` in `src/bin/api/schema.rs` (search `CREATE TRIGGER IF NOT EXISTS audit_`).

A column is only audited if it appears explicitly in the trigger's `json_object(...)` payload — for the update trigger, in **both** the `OLD` and `NEW` object. Adding a column with `add_column_if_missing()` does not extend the trigger, and nothing detects the drift: the audit log keeps working and silently omits the new field. This is how `watchlist_symbols.breakthrough_price` and `stop_loss_price` became unrecoverable after a data-loss incident.

For a new table, add all three triggers (`_insert`, `_update`, `_delete`) following the existing pattern.

**The triggers are `CREATE TRIGGER IF NOT EXISTS`, so editing the SQL has no effect on a database where the trigger already exists.** When changing an existing trigger, `DROP TRIGGER IF EXISTS <name>` immediately before recreating it, or live databases keep running the old definition.

Deliberate exclusions — do **not** add triggers to these:

- `prices` — 150k+ machine-fetched rows; auditing it would dwarf the database, and the data is re-fetchable from Yahoo.
- `cached_current_prices` — pure cache, rewritten on every price refresh.
- `dividend_events` — fetched from Yahoo and re-derivable. Its triggers are dropped in `init_db()`; when it was audited, each refresh wrote thousands of rows with no user input in any of them. (What the user decides about a dividend lives in `dividend_exclusions` and `holdings_transactions`, which are audited.)

`symbol_info` stays audited because `dividend_withholding_pct` is user-typed, but its update trigger has a `WHEN` clause so the per-quote upsert that changes nothing is not recorded. A new column there must be added to that `WHEN` list as well as to the payloads.
- `audit_log`, `event_log` — the log tables themselves.

The test for whether something needs auditing: **if a user typed it, it must be audited.** Machine-fetched data that can be re-derived from an external source does not.

### Tables that are not part of the schema

The rule above covers the tables `init_db()` creates. One more may exist in a
database and is **not** one of them:

- `watchlist_prices` — daily OHLC bars, superseded by `prices`. `init_db()` has
  not created it for some time and nothing writes to it, so a database created
  today has 19 tables and no such table; one carried forward from an older
  version still has it, holding whatever it held when writes stopped.

It needs no triggers, and adding them would be wrong: a fresh database would
never run them. It is listed in `SYMBOL_KEYED_TABLES` so a symbol rename still
migrates rows in the databases that have it, and that loop skips any table
`sqlite_master` does not report — which is why its absence is harmless.

Do not read its lack of triggers as the drift this section warns about. If it is
ever dropped for good, remove it from `SYMBOL_KEYED_TABLES` at the same time.

## Code Layout

Shared by every binary, in `src/` (the `stocks` library):

- `db.rs` — `open_db` (WAL + busy timeout) and `insert_event_log`. Use these; don't open SQLite directly.
- `http.rs` — the only way to build an HTTP client (it sets the timeouts).
- `yahoo.rs` — Yahoo chart response types and `local_date` (bars and events are dated in exchange time). Yahoo's daily history can leave the newest session's close null for hours after the close, while the meta's `regularMarketPrice` already has it; the API's reader, `market::chart_history`, completes that bar from the meta. A reader that skips null closes without doing the same loses the newest session.
- `prices.rs` — `upsert_daily_bar`, the one writer for `prices` (refuses zero closes, keeps backfilled OHLC). Stored bars can have holes: the price refresh writes one bar per quote, so a session that closed while the app was not open never gets one, and the history top-up only appends days after the newest stored bar. Code that measures a move from stored bars must not take the previous stored bar for the previous session — `sectors::reread_histories` writes Yahoo's recent history over the stored bars first.
- `dividends.rs` — fetching, de-duplicating and storing dividend events.
- `portfolio.rs`, `indicators.rs`, `hindsight.rs` — the calculation engines. `portfolio::walk_parcels` is the one FIFO walk that matches sales to purchases parcel by parcel; reuse it rather than keeping another lot queue.
- `rotation.rs` — sector rotation: each period's price move (1D, 1W, 1M, 3M) to a given day, and the difference from a benchmark in percentage points. Also `trailing_moves`, the holdings heat map's 1W–2Y moves, which end on the holding's quote rather than a stored close; the startup refresh re-reads the holdings' recent bars (`sectors::reread_histories`) so a period does not start on a missing session.
- `tax.rs` — Australian income-tax rules for the tax report: income years, the 12-month CGT discount, loss ordering, franking gross-up.

The API (`src/bin/api/`) is split by domain: `holdings`, `watchlist`, `cash`, `dividend`, `dividend_import` (CSV of dividend statements), `quotes`, `fx`, `valuation` (portfolio screens), `sectors` (the Dashboard's sector rotation sets, stored in `app_config` as `sector_rotation`, and each fund's top holdings, which come from Yahoo's quoteSummary behind a cookie-and-crumb session in `market`), `charts`, `analysis`, `refresh`, `config`, `tax` (the tax report, symbol tax settings, trusts' annual tax statements), plus `schema` and `market` (Yahoo access). `main.rs` holds startup, routing, auth, the OpenAPI document and the shared helpers (`with_tx`, `collect_rows`, error responses); tests are in `tests.rs`. Each module starts `use super::*;` and its items are `pub(crate)`, so a handler can move between modules without import changes.

A write that touches more than one row goes through `with_tx`, so it commits all-or-nothing.

A recorded dividend is linked to its fetched `dividend_events` row by date — or by `source_ex_date` once its own date has been corrected (the import and the edit form both set it). Any new code that matches dividends to events must check both, or a corrected dividend reads as unrecorded and gets recorded a second time.

## Error and Warning Logging

Any error or warning condition in the backend **must** be recorded in the `event_log` table via `insert_event_log()`. This applies to:

- Failed external fetches (Yahoo Finance, FX rates, etc.)
- Data validation failures
- Any `Err(...)` branch that would otherwise be silently swallowed

Use level `"error"` for failures and `"warn"` for recoverable issues. Do not silently discard errors with bare `let _ = ...` or `if let Ok(...) =` patterns without also logging the failure case.

# Stocks App — Coding Rules

## Database Schema

### Watchlist tables
The watchlist uses a **two-table normalised design**. Do not collapse these back into a single table.

- `watchlist_symbols` — one row per symbol. Holds per-symbol data (notes, updated_at).
- `watchlist_memberships` — one row per symbol/list pair. Holds the list name and added date.

```sql
watchlist_symbols     (id, symbol UNIQUE, notes, updated_at)
watchlist_memberships (id, symbol, list_name, added_at)  -- FK: symbol → watchlist_symbols.symbol
```

When querying watchlist data always JOIN the two tables. When a membership is deleted and no memberships remain for that symbol, also delete the `watchlist_symbols` row.

## Audit Logging

Every new table and every new column **must** be added to the audit triggers in the same change that introduces it. The triggers live in `init_db()` in `src/bin/api.rs` (search `CREATE TRIGGER IF NOT EXISTS audit_`).

A column is only audited if it appears explicitly in the trigger's `json_object(...)` payload — for the update trigger, in **both** the `OLD` and `NEW` object. Adding a column with `add_column_if_missing()` does not extend the trigger, and nothing detects the drift: the audit log keeps working and silently omits the new field. This is how `watchlist_symbols.breakthrough_price` and `stop_loss_price` became unrecoverable after a data-loss incident.

For a new table, add all three triggers (`_insert`, `_update`, `_delete`) following the existing pattern.

**The triggers are `CREATE TRIGGER IF NOT EXISTS`, so editing the SQL has no effect on a database where the trigger already exists.** When changing an existing trigger, `DROP TRIGGER IF EXISTS <name>` immediately before recreating it, or live databases keep running the old definition.

Deliberate exclusions — do **not** add triggers to these:

- `prices` — 150k+ machine-fetched rows; auditing it would dwarf the database, and the data is re-fetchable from Yahoo.
- `cached_current_prices` — pure cache, rewritten on every price refresh.
- `audit_log`, `event_log` — the log tables themselves.

The test for whether something needs auditing: **if a user typed it, it must be audited.** Machine-fetched data that can be re-derived from an external source does not.

## Error and Warning Logging

Any error or warning condition in the backend **must** be recorded in the `event_log` table via `insert_event_log()`. This applies to:

- Failed external fetches (Yahoo Finance, FX rates, etc.)
- Data validation failures
- Any `Err(...)` branch that would otherwise be silently swallowed

Use level `"error"` for failures and `"warn"` for recoverable issues. Do not silently discard errors with bare `let _ = ...` or `if let Ok(...) =` patterns without also logging the failure case.

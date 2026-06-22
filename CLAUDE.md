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

## Error and Warning Logging

Any error or warning condition in the backend **must** be recorded in the `event_log` table via `insert_event_log()`. This applies to:

- Failed external fetches (Yahoo Finance, FX rates, etc.)
- Data validation failures
- Any `Err(...)` branch that would otherwise be silently swallowed

Use level `"error"` for failures and `"warn"` for recoverable issues. Do not silently discard errors with bare `let _ = ...` or `if let Ok(...) =` patterns without also logging the failure case.

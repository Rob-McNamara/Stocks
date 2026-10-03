//! Writing daily bars to `prices` — shared by the API and the price daemon.
//!
//! The API learned two lessons the hard way: a delisted symbol comes back from
//! Yahoo as a close of 0.0, which must never overwrite a real bar, and a
//! close-only refresh must never blank the open/high/low a backfill filled in.
//! The daemon wrote bars with a bare `INSERT OR REPLACE` and knew neither, so
//! every scheduled run could undo both. Every writer now goes through
//! `upsert_daily_bar`.

use rusqlite::{params, Connection};

/// Whether a price is a real one rather than an absence of one.
///
/// Yahoo answers a delisted or unknown symbol with `0.0` rather than null.
/// Zero is never a price for an equity, and treating it as one values the
/// holding at nothing.
pub fn is_usable_price(price: Option<f64>) -> bool {
    matches!(price, Some(p) if p.is_finite() && p > 0.0)
}

/// The session's high/low as the envelope of every figure available for it.
///
/// `highs`/`lows` are the reported extremes (from up to two sources); `trades`
/// are prices that definitely traded in the session (the open and the close or
/// latest price). Every one of them is a real trade, so the true range contains
/// them all. With no reported high or low, the trades alone are not a range,
/// and that side is left empty rather than invented.
pub fn session_range(
    highs: [Option<f64>; 2],
    lows: [Option<f64>; 2],
    trades: [Option<f64>; 2],
) -> (Option<f64>, Option<f64>) {
    let usable = |v: &Option<f64>| v.filter(|p| is_usable_price(Some(*p)));
    let high = highs.iter().chain(trades.iter()).filter_map(usable).reduce(f64::max);
    let low = lows.iter().chain(trades.iter()).filter_map(usable).reduce(f64::min);
    let high = if highs.iter().any(|v| usable(v).is_some()) { high } else { None };
    let low = if lows.iter().any(|v| usable(v).is_some()) { low } else { None };
    (high, low)
}

/// One daily bar as fetched.
#[derive(Debug, Clone)]
pub struct DailyBar<'a> {
    pub date: &'a str,
    pub open: Option<f64>,
    pub high: Option<f64>,
    pub low: Option<f64>,
    pub close: Option<f64>,
    pub volume: Option<i64>,
}

/// What `upsert_daily_bar` did with a bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Upsert {
    Written,
    /// The close was missing, zero or not finite; the stored bar is untouched.
    RefusedUnusableClose,
}

/// Insert or update one daily bar, keeping what is already known.
///
/// - A close that is not a real price is refused, so the last good bar stands.
/// - The high/low are widened to contain the bar's own open and close; Yahoo's
///   bars are not always self-consistent, and one that isn't draws a candle
///   body outside its wick.
/// - Open/high/low are `COALESCE`d, so a refresh that lacks them never blanks
///   values a backfill already filled.
pub fn upsert_daily_bar(conn: &Connection, symbol: &str, bar: &DailyBar, fetched_at: &str) -> rusqlite::Result<Upsert> {
    if !is_usable_price(bar.close) {
        return Ok(Upsert::RefusedUnusableClose);
    }
    let (high, low) = session_range([bar.high, None], [bar.low, None], [bar.open, bar.close]);
    conn.execute(
        "INSERT INTO prices (symbol, date, open, high, low, close, volume, fetched_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(symbol, date) DO UPDATE SET
           open = COALESCE(excluded.open, open),
           high = COALESCE(excluded.high, high),
           low = COALESCE(excluded.low, low),
           close = excluded.close,
           volume = COALESCE(excluded.volume, volume),
           fetched_at = excluded.fetched_at",
        params![symbol, bar.date, bar.open, high, low, bar.close, bar.volume, fetched_at],
    )?;
    Ok(Upsert::Written)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE prices (id INTEGER PRIMARY KEY, symbol TEXT NOT NULL, date TEXT NOT NULL,
                open REAL, high REAL, low REAL, close REAL, volume INTEGER, fetched_at TEXT NOT NULL,
                UNIQUE(symbol, date));",
        )
        .unwrap();
        conn
    }

    fn bar(date: &str, open: Option<f64>, high: Option<f64>, low: Option<f64>, close: Option<f64>) -> DailyBar<'_> {
        DailyBar { date, open, high, low, close, volume: Some(100) }
    }

    fn stored(conn: &Connection) -> (Option<f64>, Option<f64>, Option<f64>, f64) {
        conn.query_row("SELECT open, high, low, close FROM prices", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap()
    }

    #[test]
    fn a_zero_close_never_replaces_a_real_bar() {
        let conn = db();
        upsert_daily_bar(&conn, "JLG.AX", &bar("2026-01-05", Some(1.0), Some(1.2), Some(0.9), Some(1.1)), "x").unwrap();
        let outcome = upsert_daily_bar(&conn, "JLG.AX", &bar("2026-01-05", None, None, None, Some(0.0)), "y").unwrap();
        assert_eq!(outcome, Upsert::RefusedUnusableClose);
        assert_eq!(stored(&conn).3, 1.1);
    }

    #[test]
    fn a_close_only_refresh_keeps_the_backfilled_range() {
        let conn = db();
        upsert_daily_bar(&conn, "BHP.AX", &bar("2026-01-05", Some(40.0), Some(42.0), Some(39.0), Some(41.0)), "x").unwrap();
        upsert_daily_bar(&conn, "BHP.AX", &bar("2026-01-05", None, None, None, Some(41.5)), "y").unwrap();
        assert_eq!(stored(&conn), (Some(40.0), Some(42.0), Some(39.0), 41.5));
    }

    #[test]
    fn a_bar_is_widened_to_contain_its_own_body() {
        let conn = db();
        upsert_daily_bar(&conn, "USDAUD=X", &bar("2025-01-21", Some(1.60), Some(1.62), Some(1.59), Some(1.63)), "x").unwrap();
        assert_eq!(stored(&conn).1, Some(1.63));
    }
}

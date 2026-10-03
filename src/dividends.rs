//! Fetching and storing dividend events — shared by the API and the dividends
//! daemon.
//!
//! These used to be written twice. The API's copy learned to collapse Yahoo's
//! repeated events and to replace a symbol's set rather than grow it; the
//! daemon's copy did neither, so every daemon run put the duplicates back and
//! the API then recorded the second copy as a real payment. One implementation
//! means a fix to either reaches both.

use chrono::{NaiveDate, Utc};
use reqwest::Client;
use rusqlite::{params, Connection};

use crate::yahoo;

#[derive(Debug, Clone)]
pub struct DividendEvent {
    pub symbol: String,
    pub ex_date: NaiveDate,
    pub payment_date: Option<NaiveDate>,
    pub record_date: Option<NaiveDate>,
    /// Per share, in the currency the symbol is listed in.
    pub amount: f64,
    pub fetched_at: String,
}

/// The symbol's whole dividend history.
///
/// `range=max` with monthly bars: the events are the same ex-dates and amounts
/// a daily request reports, but over the symbol's full life rather than five
/// years, and the bar arrays that come along with them stay small. A five-year
/// window made this table's contents slide: each refresh replaced the set, so
/// a dividend dropped out of every total on the day it turned five years old.
pub fn history_url(symbol: &str) -> String {
    format!(
        "https://query1.finance.yahoo.com/v8/finance/chart/{}?interval=1mo&range=max&events=div",
        symbol
    )
}

pub async fn fetch_events(client: &Client, symbol: &str) -> Result<Vec<DividendEvent>, String> {
    let response = client.get(history_url(symbol)).send().await.map_err(|e| e.to_string())?;
    let response = response.error_for_status().map_err(|e| e.to_string())?;
    let payload: yahoo::ChartResponse = response.json().await.map_err(|e| e.to_string())?;
    let result = payload.chart.into_first(symbol)?;

    let now = Utc::now().to_rfc3339();
    let gmtoffset = result.gmtoffset();
    let mut events = Vec::new();
    if let Some(dividends) = result.events.and_then(|e| e.dividends) {
        for entry in dividends.values() {
            let amount = entry.amount.unwrap_or(0.0);
            if amount <= 0.0 {
                continue;
            }
            let ts = entry
                .ex_date
                .or(entry.date)
                .ok_or_else(|| format!("Dividend entry missing date for {}", symbol))?;
            let ex_date = yahoo::local_date(ts, gmtoffset)
                .ok_or_else(|| format!("Invalid timestamp {} for {}", ts, symbol))?;
            events.push(DividendEvent {
                symbol: symbol.to_string(),
                ex_date,
                payment_date: entry.payment_date.and_then(|t| yahoo::local_date(t, gmtoffset)),
                record_date: entry.record_date.and_then(|t| yahoo::local_date(t, gmtoffset)),
                amount,
                fetched_at: now.clone(),
            });
        }
    }
    events.sort_by_key(|e| e.ex_date);
    Ok(events)
}

/// Days within which two identical amounts are taken to be one distribution.
/// Yahoo has been seen listing a Vanguard quarterly three days apart.
pub const DUPLICATE_WINDOW_DAYS: i64 = 4;

/// Collapse the same distribution reported more than once.
///
/// Yahoo's feed genuinely repeats some events: VAE.AX comes back with
/// timestamps exactly 86400 apart carrying an identical 0.677876, which is one
/// distribution described twice, not two payments. Left alone it is paid twice
/// into the cash ledger. Duplicates are matched on an identical amount within a
/// few days, and the earliest is kept — that is the real ex-date, the later
/// copy being the artefact.
pub fn dedupe(events: &[DividendEvent]) -> Vec<DividendEvent> {
    let mut sorted: Vec<DividendEvent> = events.to_vec();
    sorted.sort_by(|a, b| a.ex_date.cmp(&b.ex_date));

    let mut kept: Vec<DividendEvent> = Vec::with_capacity(sorted.len());
    for event in sorted {
        let is_repeat = kept.iter().any(|k| {
            (k.amount - event.amount).abs() < 1e-6
                && (event.ex_date - k.ex_date).num_days() <= DUPLICATE_WINDOW_DAYS
        });
        if !is_repeat {
            kept.push(event);
        }
    }
    kept
}

/// Replace a symbol's stored events with a fresh fetch, returning how many were
/// written — zero when the fetch matched what was already stored.
///
/// The fetch is the symbol's complete history (see `history_url`), so it is the
/// authority: a stored row it no longer reports is stale, not merely
/// unrefreshed. Upserting alone made this table grow-only — when a
/// date-handling change moved ex-dates by a day, every refresh added a second
/// copy of every dividend rather than correcting the first, because the key is
/// (symbol, ex_date). Replacing the whole set is what makes a refresh converge.
///
/// An empty fetch is a failure to learn anything, not news that the symbol
/// never paid a dividend, so it leaves the stored history alone.
pub fn replace_events(conn: &mut Connection, symbol: &str, events: &[DividendEvent]) -> rusqlite::Result<usize> {
    if events.is_empty() {
        return Ok(0);
    }
    let events = dedupe(events);
    let tx = conn.transaction()?;
    // Most refreshes learn nothing new. Rewriting an identical set would still
    // move every `fetched_at`, which is what tells polling clients the
    // dividends changed, so an unchanged set is left exactly as it is.
    let as_row = |e: &DividendEvent| {
        (
            e.ex_date.format("%Y-%m-%d").to_string(),
            e.payment_date.map(|d| d.format("%Y-%m-%d").to_string()),
            e.record_date.map(|d| d.format("%Y-%m-%d").to_string()),
            e.amount,
        )
    };
    let wanted: Vec<_> = events.iter().map(as_row).collect();
    let stored: Vec<(String, Option<String>, Option<String>, f64)> = {
        let mut stmt = tx.prepare(
            "SELECT ex_date, payment_date, record_date, amount FROM dividend_events WHERE symbol = ?1 ORDER BY ex_date",
        )?;
        stmt.query_map(params![symbol], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .collect::<rusqlite::Result<_>>()?
    };
    if stored == wanted {
        return Ok(0);
    }
    {
        tx.execute("DELETE FROM dividend_events WHERE symbol = ?1", params![symbol])?;
        let mut stmt = tx.prepare(
            "INSERT OR REPLACE INTO dividend_events (symbol, ex_date, payment_date, record_date, amount, fetched_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?;
        for event in &events {
            stmt.execute(params![
                symbol,
                event.ex_date.format("%Y-%m-%d").to_string(),
                event.payment_date.map(|d| d.format("%Y-%m-%d").to_string()),
                event.record_date.map(|d| d.format("%Y-%m-%d").to_string()),
                event.amount,
                event.fetched_at,
            ])?;
        }
    }
    tx.commit()?;
    Ok(events.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(date: &str, amount: f64) -> DividendEvent {
        DividendEvent {
            symbol: "VAE.AX".to_string(),
            ex_date: NaiveDate::parse_from_str(date, "%Y-%m-%d").unwrap(),
            payment_date: None,
            record_date: None,
            amount,
            fetched_at: "x".to_string(),
        }
    }

    fn memory_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE dividend_events (
                id INTEGER PRIMARY KEY, symbol TEXT NOT NULL, ex_date TEXT NOT NULL,
                payment_date TEXT, record_date TEXT, amount REAL NOT NULL,
                fetched_at TEXT NOT NULL, UNIQUE(symbol, ex_date));",
        )
        .unwrap();
        conn
    }

    fn stored(conn: &Connection) -> Vec<String> {
        let mut stmt = conn.prepare("SELECT ex_date FROM dividend_events ORDER BY ex_date").unwrap();
        stmt.query_map([], |r| r.get(0)).unwrap().flatten().collect()
    }

    /// The store both binaries use collapses Yahoo's repeated event. The
    /// daemon used to write it raw, re-creating the duplicate after every API
    /// refresh had removed it.
    #[test]
    fn storing_collapses_a_repeated_event() {
        let mut conn = memory_db();
        let written = replace_events(&mut conn, "VAE.AX", &[event("2025-07-01", 0.677876), event("2025-07-02", 0.677876)]).unwrap();
        assert_eq!(written, 1);
        assert_eq!(stored(&conn), vec!["2025-07-01"]);
    }

    /// An unchanged fetch writes nothing, so `fetched_at` — the change stamp
    /// polling clients read — only moves when the dividends actually did.
    #[test]
    fn an_unchanged_fetch_writes_nothing() {
        let mut conn = memory_db();
        replace_events(&mut conn, "VAE.AX", &[event("2025-02-27", 0.1)]).unwrap();
        let mut again = event("2025-02-27", 0.1);
        again.fetched_at = "later".to_string();
        assert_eq!(replace_events(&mut conn, "VAE.AX", &[again]).unwrap(), 0);
        let fetched: String = conn.query_row("SELECT fetched_at FROM dividend_events", [], |r| r.get(0)).unwrap();
        assert_eq!(fetched, "x");
    }

    /// A fetch is the whole history, so a stored row it no longer reports goes.
    #[test]
    fn storing_replaces_rather_than_grows() {
        let mut conn = memory_db();
        replace_events(&mut conn, "VAE.AX", &[event("2025-02-27", 0.1)]).unwrap();
        replace_events(&mut conn, "VAE.AX", &[event("2025-02-28", 0.1)]).unwrap();
        assert_eq!(stored(&conn), vec!["2025-02-28"]);
    }

    /// An empty fetch says nothing about history and must not erase it.
    #[test]
    fn an_empty_fetch_keeps_what_is_stored() {
        let mut conn = memory_db();
        replace_events(&mut conn, "VAE.AX", &[event("2025-02-27", 0.1)]).unwrap();
        replace_events(&mut conn, "VAE.AX", &[]).unwrap();
        assert_eq!(stored(&conn), vec!["2025-02-27"]);
    }

    /// ASX ex-dates are stamped at the market open — 23:00 UTC the previous
    /// day during daylight saving — and land a day early without the offset.
    #[test]
    fn ex_dates_are_taken_in_exchange_time() {
        // 1767564000 = 2026-01-04 22:00 UTC = 2026-01-05 09:00 AEDT
        assert_eq!(yahoo::local_date(1767564000, Some(39600)), NaiveDate::from_ymd_opt(2026, 1, 5));
        assert_eq!(yahoo::local_date(1767564000, None), NaiveDate::from_ymd_opt(2026, 1, 4));
    }

    #[test]
    fn the_history_request_asks_for_the_whole_life_of_the_symbol() {
        assert!(history_url("CBA.AX").contains("range=max"));
    }
}

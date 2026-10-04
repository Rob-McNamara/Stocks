//! Opening the database and writing to the event log — shared by every binary.
//!
//! Each binary used to carry its own copy of both. They were identical until
//! they weren't: the backfill tools logged under a fixed source and swallowed
//! failures differently. One definition keeps the WAL and busy-timeout
//! settings, which let the API and the daemons write concurrently, the same
//! everywhere.

use chrono::Utc;
use rusqlite::{params, Connection};
use std::path::Path;

/// Open the SQLite database with WAL mode and a busy timeout so the API,
/// price daemon and backfill tools can write concurrently without
/// intermittent "database is locked" failures.
pub fn open_db<P: AsRef<Path>>(path: P) -> Result<Connection, rusqlite::Error> {
    let conn = Connection::open(path)?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    let _: String = conn.query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))?;
    Ok(conn)
}

/// Append one row to `event_log` on an open connection.
pub fn log_event(
    conn: &Connection,
    level: &str,
    event_type: &str,
    source: &str,
    symbol: Option<&str>,
    details: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO event_log (timestamp, level, source, event_type, symbol, details) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![Utc::now().to_rfc3339(), level, source, event_type, symbol, details],
    )?;
    Ok(())
}

/// Append one row to `event_log`, opening the database to do it.
///
/// Takes `&Path` rather than a generic so callers holding a `PathBuf` behind a
/// wrapper (actix's `web::Data`) can pass it by reference and let it deref.
pub fn insert_event_log(
    db_path: &Path,
    level: &str,
    event_type: &str,
    source: &str,
    symbol: Option<&str>,
    details: &str,
) -> Result<(), String> {
    let conn = open_db(db_path).map_err(|err| err.to_string())?;
    log_event(&conn, level, event_type, source, symbol, details).map_err(|err| err.to_string())
}

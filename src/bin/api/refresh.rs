//! The one-call startup refresh: prices for the watchlist, holdings and sold
//! positions, then dividends — debounced, and one at a time.

use super::*;

#[derive(Deserialize)]
pub(crate) struct RefreshQuery {
    pub(crate) force: Option<bool>,
}

/// Only one refresh may run at a time — a second caller gets a skip
/// response instead of doubling the Yahoo traffic.
pub(crate) static REFRESH_IN_FLIGHT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Holds `REFRESH_IN_FLIGHT` for as long as it lives.
///
/// Released on drop rather than by a store at the end of the handler, because
/// the end of the handler is not guaranteed to run: actix drops a handler's
/// future when the client disconnects (a page reloaded mid-refresh), and a
/// panic unwinds past it. Either one used to leave the flag set, and every
/// refresh after it answered "in progress" until the API was restarted.
pub(crate) struct RefreshGuard;

impl RefreshGuard {
    pub(crate) fn acquire() -> Option<Self> {
        REFRESH_IN_FLIGHT
            .compare_exchange(false, true, std::sync::atomic::Ordering::SeqCst, std::sync::atomic::Ordering::SeqCst)
            .ok()
            .map(|_| RefreshGuard)
    }
}

impl Drop for RefreshGuard {
    fn drop(&mut self) {
        REFRESH_IN_FLIGHT.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Whether a completed refresh should stamp `last_full_refresh_at`.
/// Stamp when the refresh achieved something, or when there was nothing to
/// do — but a total failure (e.g. Yahoo down) must leave the stamp unset so
/// the next attempt isn't debounced into a stale-data window.
pub(crate) fn refresh_should_stamp(attempted_any: bool, did_any_work: bool) -> bool {
    did_any_work || !attempted_any
}

/// One-call startup refresh: watchlist prices, holdings prices and dividends,
/// debounced server-side so a client opening repeatedly doesn't hammer Yahoo.
#[utoipa::path(post, path = "/api/v1/refresh", tag = "system", responses((status = 200, description = "Refresh all")))]
#[post("/api/refresh")]
pub(crate) async fn refresh_all(db_path: web::Data<PathBuf>, query: web::Query<RefreshQuery>) -> impl Responder {
    const DEBOUNCE_SECS: i64 = 600;

    if query.force != Some(true) {
        let last = load_config(&db_path)
            .ok()
            .and_then(|c| c.into_iter().find(|i| i.key == "last_full_refresh_at").map(|i| i.value));
        if let Some(last) = last
            && let Ok(t) = chrono::DateTime::parse_from_rfc3339(&last)
                && (Utc::now() - t.with_timezone(&Utc)).num_seconds() < DEBOUNCE_SECS {
                    return HttpResponse::Ok().json(serde_json::json!({ "skipped": true, "last_refreshed_at": last }));
                }
    }
    let Some(_guard) = RefreshGuard::acquire() else {
        return HttpResponse::Ok().json(serde_json::json!({ "skipped": true, "reason": "refresh_in_progress" }));
    };

    let mut errors: Vec<String> = Vec::new();
    let mut watchlist_ok = 0;
    let mut holdings_ok = 0;

    let watchlist_count = match fetch_watchlist_current_prices(&db_path, None).await {
        Ok(prices) => {
            watchlist_ok = prices.iter().filter(|p| p.error.is_none()).count();
            errors.extend(prices.iter().filter_map(|p| p.error.clone()));
            prices.len()
        }
        Err(err) => {
            errors.push(err);
            0
        }
    };

    // An unreadable list used to read as "nothing held", so the refresh
    // reported success having refreshed nothing.
    let holding_symbols = load_holding_symbols(&db_path).unwrap_or_else(|err| {
        let _ = insert_event_log(&db_path, "error", "refresh_all", "api", None, &format!("Could not list holdings: {}", err));
        errors.push(err);
        Vec::new()
    });
    let holdings_count = if holding_symbols.is_empty() {
        0
    } else {
        match fetch_and_cache_current_prices(&db_path, &holding_symbols, "holdings_prices_updated_at").await {
            Ok(prices) => {
                holdings_ok = prices.iter().filter(|p| p.error.is_none()).count();
                errors.extend(prices.iter().filter_map(|p| p.error.clone()));
                prices.len()
            }
            Err(err) => {
                errors.push(err);
                0
            }
        }
    };

    // Sold positions, so the Hindsight windows keep filling. Their own stamp:
    // a stale quote on something sold last year is far less urgent than one on
    // a holding, and separating them makes that visible rather than assumed.
    let mut exited_ok = 0;
    let exited_symbols = load_exited_symbols(&db_path).unwrap_or_else(|err| {
        let _ = insert_event_log(&db_path, "error", "refresh_all", "api", None, &format!("Could not list sold positions: {}", err));
        errors.push(err);
        Vec::new()
    });
    let exited_count = if exited_symbols.is_empty() {
        0
    } else {
        match fetch_and_cache_current_prices(&db_path, &exited_symbols, "sold_prices_updated_at").await {
            Ok(prices) => {
                exited_ok = prices.iter().filter(|p| p.error.is_none()).count();
                // A delisted symbol errors on every run for good reason, so its
                // failure does not belong in the banner the user sees.
                prices.len()
            }
            Err(err) => {
                errors.push(err);
                0
            }
        }
    };

    let dividends = refresh_dividends_for_symbols(&db_path, holding_symbols).await;
    errors.extend(dividends.errors.clone());

    // Record what was just fetched, as the dividend refresh endpoints do.
    // Without this an event fetched here sat in the ledger as an unrecorded
    // row, missing from the cash balance, until someone pressed the separate
    // dividend refresh.
    let recorded = match record_new_dividends(&db_path) {
        Ok(r) => r,
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "dividend_record", "api", None, &err);
            errors.push(err.clone());
            DividendRecordResult { errors: vec![err], ..Default::default() }
        }
    };
    errors.extend(recorded.errors.iter().cloned());

    let attempted_any = watchlist_count > 0 || holdings_count > 0;
    let did_any_work = watchlist_ok > 0 || holdings_ok > 0 || dividends.updated > 0;
    if refresh_should_stamp(attempted_any, did_any_work)
        && let Err(err) = upsert_config(&db_path, "last_full_refresh_at", &Utc::now().to_rfc3339()) {
            let _ = insert_event_log(&db_path, "error", "refresh_all", "api", None, &err);
        }

    let _ = insert_event_log(&db_path, "info", "refresh_all", "api", None, &format!("Refreshed {} watchlist prices, {} holdings prices, {} sold prices, dividends for {} symbols ({} error(s))", watchlist_ok, holdings_ok, exited_ok, dividends.updated, errors.len()));

    HttpResponse::Ok().json(serde_json::json!({
        "skipped": false,
        "watchlist_prices": watchlist_count,
        "holdings_prices": holdings_count,
        "sold_prices": exited_count,
        "dividends_updated": dividends.updated,
        "dividends_recorded": recorded.recorded,
        "errors": errors,
    }))
}

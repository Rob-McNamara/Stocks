//! Corporate actions: changes to what is held that are not trades. Today that
//! is a merger or takeover paid in shares — Brickworks holders receiving 0.82
//! Soul Patts shares each — and the table is shaped to take splits, demergers
//! and returns of capital later.
//!
//! An action is stored once, as the user records it. `expand_corporate_actions`
//! turns it into rows every engine already understands when the holdings are
//! loaded: the old company's parcels closing on the action's date, and a parcel
//! in the new company for each one given up. Nothing is copied into
//! holdings_transactions, so deleting the action undoes it completely, and the
//! carried cost always follows the purchases as they are now recorded.

use super::*;
use std::collections::HashSet;

/// What a corporate action may be.
pub(crate) const CORPORATE_ACTION_KINDS: &[&str] = &["merger"];

/// Below this many shares a shortfall is float noise, not a missing purchase.
const SHARE_TOLERANCE: f64 = 1e-6;

/// The action a worked-out row stands for, carried on the row.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub(crate) struct CorporateActionLink {
    pub(crate) id: i64,
    pub(crate) kind: String,
    /// The other side: the symbol merged into, or merged from.
    pub(crate) counterpart: String,
    /// Scrip-for-scrip roll-over chosen: no capital gain, cost and dates carried.
    pub(crate) rollover: bool,
}

#[derive(Serialize, Clone, Debug)]
pub(crate) struct CorporateAction {
    pub(crate) id: i64,
    pub(crate) kind: String,
    /// The implementation date: the old shares are gone and the new ones held.
    pub(crate) date: String,
    pub(crate) from_symbol: String,
    pub(crate) to_symbol: String,
    pub(crate) from_quantity: f64,
    pub(crate) to_quantity: f64,
    pub(crate) rollover: bool,
    /// Without roll-over: the AUD value of the shares received, which is what
    /// the old ones were disposed of for.
    pub(crate) market_value: Option<f64>,
    pub(crate) notes: Option<String>,
    pub(crate) created_at: String,
}

#[derive(Deserialize, Clone, Debug)]
pub(crate) struct CorporateActionPayload {
    pub(crate) kind: Option<String>,
    pub(crate) date: String,
    pub(crate) from_symbol: String,
    pub(crate) to_symbol: String,
    pub(crate) from_quantity: f64,
    pub(crate) to_quantity: f64,
    /// Defaults to true: most holders choose roll-over.
    pub(crate) rollover: Option<bool>,
    pub(crate) market_value: Option<f64>,
    pub(crate) notes: Option<String>,
}

pub(crate) fn load_corporate_actions(conn: &Connection) -> Result<Vec<CorporateAction>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT id, kind, date, from_symbol, to_symbol, from_quantity, to_quantity, rollover, market_value, notes, created_at
               FROM corporate_actions ORDER BY date, id",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| {
            Ok(CorporateAction {
                id: r.get(0)?,
                kind: r.get(1)?,
                date: r.get(2)?,
                from_symbol: r.get(3)?,
                to_symbol: r.get(4)?,
                from_quantity: r.get(5)?,
                to_quantity: r.get(6)?,
                rollover: r.get::<_, i64>(7)? != 0,
                market_value: r.get(8)?,
                notes: r.get(9)?,
                created_at: r.get(10)?,
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
}

/// One parcel a merger hands on.
#[derive(Serialize, Clone, Debug)]
pub(crate) struct CarriedParcel {
    /// The purchase it came from, or a parcel an earlier action handed on.
    pub(crate) purchase_id: i64,
    /// When it was bought: kept for CGT under roll-over.
    pub(crate) acquired: String,
    pub(crate) given_up: f64,
    pub(crate) received: f64,
    /// AUD, brokerage included.
    pub(crate) cost: f64,
    /// In the shares' own currency.
    pub(crate) native_cost: f64,
}

/// What a merger takes from the old holding, worked out from the holding as
/// it stood at the close of the action's date.
#[derive(Serialize, Clone, Debug)]
pub(crate) struct MergerPlan {
    /// Shares of the old company held on the date.
    pub(crate) held: f64,
    pub(crate) parcels: Vec<CarriedParcel>,
    pub(crate) cost: f64,
    pub(crate) currency: String,
}

/// The parcels an action consumes, first in first out, from `transactions`
/// (which may already include parcels an earlier action handed on).
pub(crate) fn plan_merger(transactions: &[HoldingTransaction], action: &CorporateAction) -> MergerPlan {
    let from: Vec<HoldingTransaction> = transactions
        .iter()
        .filter(|t| t.symbol == action.from_symbol && t.date.as_str() <= action.date.as_str())
        .cloned()
        .collect();
    let currency = from
        .iter()
        .map(|t| t.currency.to_uppercase())
        .find(|c| !c.is_empty() && c != "AUD")
        .unwrap_or_else(|| "AUD".to_string());
    let walk = portfolio::walk_parcels(&to_portfolio_txs(&from), &[]);
    let held: f64 = walk.open.iter().map(|p| p.quantity).sum();
    let ratio = action.to_quantity / action.from_quantity;

    let mut remaining = action.from_quantity;
    let mut parcels = Vec::new();
    for parcel in &walk.open {
        if remaining <= SHARE_TOLERANCE {
            break;
        }
        let used = remaining.min(parcel.quantity);
        remaining -= used;
        parcels.push(CarriedParcel {
            purchase_id: parcel.purchase_id,
            acquired: parcel.acquired.clone(),
            given_up: used,
            received: used * ratio,
            cost: used * parcel.cost_per_share,
            native_cost: used * parcel.native_cost_per_share,
        });
    }
    let cost = parcels.iter().map(|p| p.cost).sum();
    MergerPlan { held, parcels, cost, currency }
}

/// A merger's closing row for the old holding, by its worked-out id. The
/// screens about sales (Sold Stocks, Hindsight) leave it out: nothing was sold.
pub(crate) fn is_merger_disposal(tx: &PortfolioTx) -> bool {
    tx.id < 0 && tx.tx_type == TxType::Sale
}

/// Worked-out rows' ids: negative, so they never meet a stored row's, and
/// spaced per action so an action's rows sort together — the closing row
/// first, then the parcels it hands on, in order.
fn synthetic_id(action_id: i64, n: usize) -> i64 {
    -(action_id * 1000) + n as i64
}

#[allow(clippy::too_many_arguments)]
fn worked_out_row(
    id: i64,
    action: &CorporateAction,
    symbol: &str,
    kind: &str,
    quantity: f64,
    price: f64,
    native_price: Option<f64>,
    currency: &str,
    notes: String,
    acquired_date: Option<String>,
) -> HoldingTransaction {
    let foreign = currency != "AUD";
    HoldingTransaction {
        id,
        symbol: symbol.to_string(),
        transaction_type: kind.to_string(),
        date: action.date.clone(),
        quantity: Some(quantity),
        price: Some(price),
        amount: None,
        brokerage: None,
        notes: Some(notes),
        created_at: action.created_at.clone(),
        dividends_total: 0.0,
        currency: currency.to_string(),
        original_price: if foreign { native_price } else { None },
        fx_rate: if foreign { native_price.filter(|n| *n > 0.0).map(|n| price / n) } else { None },
        // A merger moves shares, not cash: no settlement account.
        cash_account_id: None,
        withholding_amount: None,
        franked_amount: None,
        franking_credit: None,
        payment_date: None,
        dividend_type: None,
        source_ex_date: None,
        corporate_action: Some(CorporateActionLink {
            id: action.id,
            kind: action.kind.clone(),
            counterpart: if kind == "sale" { action.to_symbol.clone() } else { action.from_symbol.clone() },
            rollover: action.rollover,
        }),
        acquired_date,
        custom_fields: std::collections::HashMap::new(),
    }
}

/// The rows one merger stands for.
///
/// With roll-over the old parcels close at their own cost, so no gain is
/// made, and each reappears in the new company with the same cost and its
/// original purchase date, its share count scaled to what was received. A
/// holder's 80 Brickworks became 66 Soul Patts, not the 65.6 that 0.82 gives,
/// so the ratio is the shares actually received over those given up.
///
/// Without roll-over the old shares are disposed of for the value of the new
/// ones, and the new holding starts on the day at that value.
pub(crate) fn merger_rows(action: &CorporateAction, plan: &MergerPlan) -> Vec<HoldingTransaction> {
    let currency = plan.currency.as_str();
    let native_total: f64 = plan.parcels.iter().map(|p| p.native_cost).sum();
    // AUD per unit of the shares' own currency, across the parcels carried.
    let rate = if native_total > 0.0 { plan.cost / native_total } else { 1.0 };
    let summary = format!(
        "{} {} → {} {}",
        trim_quantity(action.from_quantity), action.from_symbol, trim_quantity(action.to_quantity), action.to_symbol
    );

    let mut rows = Vec::new();
    if action.rollover {
        let price = plan.cost / action.from_quantity;
        rows.push(worked_out_row(
            synthetic_id(action.id, 0), action, &action.from_symbol, "sale", action.from_quantity, price,
            Some(native_total / action.from_quantity), currency,
            format!("Merged into {}: {} (roll-over)", action.to_symbol, summary), None,
        ));
        for (i, parcel) in plan.parcels.iter().enumerate() {
            rows.push(worked_out_row(
                synthetic_id(action.id, i + 1), action, &action.to_symbol, "purchase", parcel.received,
                parcel.cost / parcel.received, Some(parcel.native_cost / parcel.received), currency,
                format!("From {}: {} (roll-over; acquired {})", action.from_symbol, summary, parcel.acquired),
                Some(parcel.acquired.clone()),
            ));
        }
    } else {
        let value = action.market_value.unwrap_or(0.0);
        rows.push(worked_out_row(
            synthetic_id(action.id, 0), action, &action.from_symbol, "sale", action.from_quantity,
            value / action.from_quantity, Some(value / action.from_quantity / rate), currency,
            format!("Merged into {}: {} (no roll-over)", action.to_symbol, summary), None,
        ));
        rows.push(worked_out_row(
            synthetic_id(action.id, 1), action, &action.to_symbol, "purchase", action.to_quantity,
            value / action.to_quantity, Some(value / action.to_quantity / rate), currency,
            format!("From {}: {} (no roll-over)", action.from_symbol, summary), None,
        ));
    }
    rows
}

fn trim_quantity(value: f64) -> String {
    let s = format!("{:.6}", value);
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// Actions whose shortfall has been logged this run. The holdings are loaded
/// on nearly every request; the warning is worth one line, not thousands.
static WARNED: std::sync::OnceLock<std::sync::Mutex<HashSet<(i64, String)>>> = std::sync::OnceLock::new();

/// Add every corporate action's rows to `transactions`, oldest action first,
/// so a holding merged twice is carried through both.
pub(crate) fn expand_corporate_actions(db_path: &Path, conn: &Connection, transactions: &mut Vec<HoldingTransaction>) -> Result<(), String> {
    let actions = load_corporate_actions(conn)?;
    for action in &actions {
        let plan = plan_merger(transactions, action);
        if plan.held + SHARE_TOLERANCE < action.from_quantity {
            let message = format!(
                "The {} merger on {} gives up {} {} but only {} were held then; the shares received are carried for those held",
                action.from_symbol, action.date, trim_quantity(action.from_quantity), action.from_symbol, trim_quantity(plan.held)
            );
            let warned = WARNED.get_or_init(Default::default);
            let first = warned.lock().map(|mut seen| seen.insert((action.id, message.clone()))).unwrap_or(true);
            if first {
                let _ = insert_event_log(db_path, "warn", "corporate_action", "api", Some(&action.from_symbol), &message);
            }
        }
        transactions.extend(merger_rows(action, &plan));
    }
    Ok(())
}

/// Check an action before it is stored, against the holdings as they are.
/// Returns it ready to store, and the plan it would follow.
fn validate_action(db_path: &Path, payload: &CorporateActionPayload) -> Result<(CorporateAction, MergerPlan), String> {
    let kind = payload.kind.clone().unwrap_or_else(|| "merger".to_string());
    if !CORPORATE_ACTION_KINDS.contains(&kind.as_str()) {
        return Err(format!("Unknown kind '{}'. Valid kinds: {}", kind, CORPORATE_ACTION_KINDS.join(", ")));
    }
    let date = NaiveDate::parse_from_str(payload.date.trim(), "%Y-%m-%d")
        .map_err(|_| "Invalid date format. Use YYYY-MM-DD.".to_string())?
        .to_string();
    let from_symbol = normalize_symbol(&payload.from_symbol);
    let to_symbol = normalize_symbol(&payload.to_symbol);
    if from_symbol.is_empty() || to_symbol.is_empty() {
        return Err("Both symbols are needed".to_string());
    }
    if from_symbol == to_symbol {
        return Err("A merger is from one symbol into another".to_string());
    }
    for (name, value) in [("from_quantity", payload.from_quantity), ("to_quantity", payload.to_quantity)] {
        if !(value.is_finite() && value > 0.0) {
            return Err(format!("{} must be more than zero", name));
        }
    }
    let rollover = payload.rollover.unwrap_or(true);
    if !rollover && !payload.market_value.is_some_and(|v| v.is_finite() && v > 0.0) {
        return Err("Without roll-over, give the market value of the shares received: it is what the old shares were disposed of for".to_string());
    }

    let conn = open_db(db_path).map_err(|e| e.to_string())?;
    let duplicate: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM corporate_actions WHERE from_symbol = ?1 AND date = ?2",
            params![from_symbol, date],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    if duplicate > 0 {
        return Err(format!("{} already has a corporate action on {}", from_symbol, date));
    }

    let action = CorporateAction {
        id: 0,
        kind,
        date,
        from_symbol,
        to_symbol,
        from_quantity: payload.from_quantity,
        to_quantity: payload.to_quantity,
        rollover,
        market_value: if rollover { None } else { payload.market_value },
        notes: payload.notes.as_deref().map(str::trim).filter(|n| !n.is_empty()).map(str::to_string),
        created_at: Utc::now().to_rfc3339(),
    };
    let transactions = fetch_holdings(&db_path.to_path_buf())?;
    let plan = plan_merger(&transactions, &action);
    if plan.held + SHARE_TOLERANCE < action.from_quantity {
        return Err(format!(
            "Only {} {} were held on {}, not {}",
            trim_quantity(plan.held), action.from_symbol, action.date, trim_quantity(action.from_quantity)
        ));
    }
    Ok((action, plan))
}

#[utoipa::path(get, path = "/api/v1/corporate-actions", tag = "transactions", responses((status = 200, description = "Recorded corporate actions")))]
#[get("/api/corporate-actions")]
pub(crate) async fn get_corporate_actions(db_path: web::Data<PathBuf>) -> impl Responder {
    match open_db(db_path.as_ref()).map_err(|e| e.to_string()).and_then(|c| load_corporate_actions(&c)) {
        Ok(actions) => HttpResponse::Ok().json(actions),
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "corporate_action", "api", None, &err);
            err_internal(err)
        }
    }
}

/// What recording an action would do, without recording it: the parcels that
/// would move and what each would carry.
#[utoipa::path(post, path = "/api/v1/corporate-actions/preview", tag = "transactions", responses((status = 200, description = "Preview a corporate action")))]
#[post("/api/corporate-actions/preview")]
pub(crate) async fn preview_corporate_action(db_path: web::Data<PathBuf>, payload: web::Json<CorporateActionPayload>) -> impl Responder {
    match validate_action(&db_path, &payload) {
        Ok((action, plan)) => HttpResponse::Ok().json(serde_json::json!({ "action": action, "plan": plan })),
        Err(err) => err_bad_request(err),
    }
}

#[utoipa::path(post, path = "/api/v1/corporate-actions", tag = "transactions", responses((status = 200, description = "Record a corporate action")))]
#[post("/api/corporate-actions")]
pub(crate) async fn add_corporate_action(db_path: web::Data<PathBuf>, payload: web::Json<CorporateActionPayload>) -> impl Responder {
    let (mut action, plan) = match validate_action(&db_path, &payload) {
        Ok(checked) => checked,
        Err(err) => {
            let _ = insert_event_log(&db_path, "warn", "corporate_action", "api", Some(&normalize_symbol(&payload.from_symbol)), &err);
            return err_bad_request(err);
        }
    };
    let stored = open_db(db_path.as_ref()).map_err(|e| e.to_string()).and_then(|conn| {
        conn.execute(
            "INSERT INTO corporate_actions (kind, date, from_symbol, to_symbol, from_quantity, to_quantity, rollover, market_value, notes, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                action.kind, action.date, action.from_symbol, action.to_symbol, action.from_quantity, action.to_quantity,
                action.rollover as i64, action.market_value, action.notes, action.created_at,
            ],
        )
        .map_err(|e| e.to_string())?;
        Ok(conn.last_insert_rowid())
    });
    match stored {
        Ok(id) => {
            action.id = id;
            let _ = insert_event_log(&db_path, "info", "corporate_action", "api", Some(&action.from_symbol), &format!(
                "Recorded {} {} → {} on {}: {} → {} shares, cost {:.2}{}",
                action.kind, action.from_symbol, action.to_symbol, action.date,
                trim_quantity(action.from_quantity), trim_quantity(action.to_quantity), plan.cost,
                if action.rollover { ", roll-over" } else { ", no roll-over" }
            ));
            HttpResponse::Ok().json(serde_json::json!({ "action": action, "plan": plan }))
        }
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "corporate_action", "api", Some(&action.from_symbol), &err);
            err_internal(err)
        }
    }
}

#[utoipa::path(delete, path = "/api/v1/corporate-actions/{id}", tag = "transactions",
    params(("id" = i64, Path, description = "id")),
    responses((status = 204, description = "Delete a corporate action, undoing it")))]
#[delete("/api/corporate-actions/{id}")]
pub(crate) async fn delete_corporate_action(db_path: web::Data<PathBuf>, path: web::Path<i64>) -> impl Responder {
    let id = path.into_inner();
    let result = open_db(db_path.as_ref())
        .map_err(|e| e.to_string())
        .and_then(|conn| conn.execute("DELETE FROM corporate_actions WHERE id = ?1", params![id]).map_err(|e| e.to_string()));
    match result {
        Ok(0) => err_not_found(format!("Corporate action {} not found", id)),
        Ok(_) => {
            let _ = insert_event_log(&db_path, "info", "corporate_action", "api", None, &format!("Deleted corporate action {}", id));
            HttpResponse::NoContent().finish()
        }
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "corporate_action", "api", None, &err);
            err_internal(err)
        }
    }
}

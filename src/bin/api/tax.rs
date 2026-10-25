//! Tax report: what an Australian return needs from the portfolio for one
//! income year — capital gains parcel by parcel, dividends with their franking,
//! foreign income, interest, trust distributions and possible deductions — and
//! the checks worth making before any of it is lodged.
//!
//! The rules are in `stocks::tax` and the parcel matching is
//! `portfolio::walk_parcels`; this module gathers the rows, applies them, and
//! builds one `TaxReport`. The screen, the clipboard text and the CSV file are
//! all rendered from that one structure, so they cannot disagree.
//!
//! Also here: the per-symbol tax settings (income type, standing franking and
//! withholding rates), trusts' annual tax statements, and the record of when a
//! year's report was last exported — which is what lets a later report say
//! "these transactions changed since you sent it".

use super::*;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fmt::Write as _;
use stocks::portfolio::{CostBaseAdjustment, ParcelWalk, SaleMatch};
use stocks::tax::{self as rules, CapitalGainsInput, CapitalGainsResult, Entity, TaxYear};

/// Who the report is for. One place to change when SMSF or company returns
/// are added; the discount already follows it.
const ENTITY: Entity = Entity::Individual;

/// The sections a report can include, in the order they are shown.
pub(crate) const TAX_SECTIONS: &[&str] = &["checks", "cgt", "dividends", "foreign", "interest", "trusts", "deductions", "planning"];

/// app_config key holding when a year's report was last copied or downloaded.
fn export_key(fy: TaxYear) -> String {
    format!("tax_report_exported_{}", fy.0)
}

// ---------------------------------------------------------------------------
// Per-symbol tax settings
// ---------------------------------------------------------------------------

/// What a symbol's income is for tax purposes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IncomeKind {
    /// Dividends from an Australian company: franked or unfranked, item 11.
    Company,
    /// Distributions from a trust or ETF, reported from its annual tax
    /// statement: item 13, with parts in items 18 and 20.
    Trust,
    /// Dividends from a foreign company or fund: item 20.
    Foreign,
}

impl IncomeKind {
    pub(crate) fn parse(value: &str) -> Option<IncomeKind> {
        match value {
            "company" => Some(IncomeKind::Company),
            "trust" => Some(IncomeKind::Trust),
            "foreign" => Some(IncomeKind::Foreign),
            _ => None,
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            IncomeKind::Company => "company",
            IncomeKind::Trust => "trust",
            IncomeKind::Foreign => "foreign",
        }
    }

    fn label(self) -> &'static str {
        match self {
            IncomeKind::Company => "Australian company",
            IncomeKind::Trust => "Trust / ETF",
            IncomeKind::Foreign => "Foreign",
        }
    }
}

/// One symbol's tax-relevant facts, with its income type resolved.
struct SymbolTax {
    name: Option<String>,
    currency: String,
    withholding_pct: Option<f64>,
    franking_pct: Option<f64>,
    kind: IncomeKind,
    /// The kind was worked out, not chosen — the report says so.
    guessed: bool,
    /// The stored "your share %", null meaning all of it.
    share_pct: Option<f64>,
}

impl SymbolTax {
    /// The user's share as a fraction: 0 leaves the symbol out of the report.
    fn share(&self) -> f64 {
        share_fraction(self.share_pct)
    }
}

/// A stored share percentage as a fraction; null is the whole of it.
fn share_fraction(pct: Option<f64>) -> f64 {
    pct.map_or(1.0, |p| (p / 100.0).clamp(0.0, 1.0))
}

/// A symbol or account name, marked when only part of it is the user's.
fn share_label(name: &str, share: f64) -> String {
    if share < 1.0 { format!("{} ({}% yours)", name, trim_number(share * 100.0)) } else { name.to_string() }
}

/// Work out a symbol's income type when the user hasn't set one.
///
/// A foreign listing pays foreign dividends. An AUD listing with a standing
/// withholding rate is a foreign-domiciled fund quoted on the ASX — VTS and
/// VEU are US funds, and the 30% withheld on them is US tax. ETFs and managed
/// funds are trusts. Everything else is taken to be an Australian company.
/// Stapled securities (GMG, say) are reported as equities by Yahoo but pay
/// trust distributions, which is why the guess is flagged for confirming.
fn guess_income_kind(currency: &str, withholding_pct: Option<f64>, instrument_type: Option<&str>) -> IncomeKind {
    if currency != "AUD" || withholding_pct.is_some_and(|p| p > 0.0) {
        IncomeKind::Foreign
    } else if matches!(instrument_type, Some("ETF") | Some("MUTUALFUND")) {
        IncomeKind::Trust
    } else {
        IncomeKind::Company
    }
}

/// Tax settings for every symbol in `symbols`.
fn load_symbol_tax(conn: &Connection, txs: &[HoldingTransaction], symbols: &BTreeSet<String>) -> Result<BTreeMap<String, SymbolTax>, String> {
    type InfoRow = (Option<String>, Option<String>, Option<String>, Option<f64>, Option<f64>, Option<String>, Option<f64>);
    let mut info: HashMap<String, InfoRow> = HashMap::new();
    {
        let mut stmt = conn
            .prepare("SELECT symbol, long_name, currency, instrument_type, dividend_withholding_pct, franking_pct, income_kind, tax_share_pct FROM symbol_info")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, (r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?))))
            .map_err(|e| e.to_string())?;
        for row in rows {
            let (symbol, values) = row.map_err(|e| e.to_string())?;
            info.insert(symbol, values);
        }
    }
    // The ETF override the Settings screen writes, which the portfolio
    // screens already honour.
    let mut overrides: HashMap<String, String> = HashMap::new();
    {
        let mut stmt = conn
            .prepare("SELECT key, value FROM app_config WHERE key LIKE 'instrument_type_%'")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| e.to_string())?;
        for row in rows {
            let (key, value) = row.map_err(|e| e.to_string())?;
            if !value.trim().is_empty() {
                overrides.insert(key["instrument_type_".len()..].to_string(), value.trim().to_string());
            }
        }
    }

    let mut result = BTreeMap::new();
    for symbol in symbols {
        let (name, info_currency, instrument_type, withholding_pct, franking_pct, kind, share_pct) =
            info.get(symbol).cloned().unwrap_or((None, None, None, None, None, None, None));
        // Same rule as the portfolio screens: symbol_info, else the currency
        // the symbol's own trades were recorded in, else AUD.
        let currency = info_currency
            .map(|c| c.trim().to_uppercase())
            .filter(|c| !c.is_empty())
            .or_else(|| {
                txs.iter()
                    .filter(|t| &t.symbol == symbol)
                    .map(|t| t.currency.trim().to_uppercase())
                    .find(|c| !c.is_empty() && c != "AUD")
            })
            .unwrap_or_else(|| "AUD".to_string());
        let set_kind = kind.as_deref().and_then(IncomeKind::parse);
        let instrument = overrides.get(symbol).cloned().or(instrument_type);
        let (kind, guessed) = match set_kind {
            Some(k) => (k, false),
            None => (guess_income_kind(&currency, withholding_pct, instrument.as_deref()), true),
        };
        result.insert(symbol.clone(), SymbolTax { name, currency, withholding_pct, franking_pct, kind, guessed, share_pct });
    }
    Ok(result)
}

#[derive(Deserialize)]
pub(crate) struct SymbolTaxPayload {
    /// "company", "trust" or "foreign"; null returns it to the guess.
    #[serde(default, deserialize_with = "deserialize_explicit_null")]
    pub(crate) income_kind: Option<Option<String>>,
    /// 0–100; null clears it.
    #[serde(default, deserialize_with = "deserialize_explicit_null")]
    pub(crate) franking_pct: Option<Option<f64>>,
    /// 0–100; null clears it.
    #[serde(default, deserialize_with = "deserialize_explicit_null")]
    pub(crate) dividend_withholding_pct: Option<Option<f64>>,
    /// The user's share of the holding, 0–100: 0 leaves it out of the report,
    /// null makes it all theirs again.
    #[serde(default, deserialize_with = "deserialize_explicit_null")]
    pub(crate) tax_share_pct: Option<Option<f64>>,
}

pub(crate) fn validate_symbol_tax(payload: &SymbolTaxPayload) -> Result<(), String> {
    if let Some(Some(kind)) = &payload.income_kind
        && IncomeKind::parse(kind).is_none()
    {
        return Err(format!("Unknown income type '{}': use company, trust or foreign", kind));
    }
    for (name, value) in [
        ("franking_pct", payload.franking_pct),
        ("dividend_withholding_pct", payload.dividend_withholding_pct),
        ("tax_share_pct", payload.tax_share_pct),
    ] {
        if let Some(Some(pct)) = value
            && !(pct.is_finite() && (0.0..=100.0).contains(&pct))
        {
            return Err(format!("{} must be between 0 and 100", name));
        }
    }
    Ok(())
}

/// Store a symbol's tax settings. Absent fields are left alone; `null`
/// clears one.
pub(crate) fn store_symbol_tax_settings(db_path: &Path, symbol: &str, payload: &SymbolTaxPayload) -> Result<(), String> {
    validate_symbol_tax(payload)?;
    with_tx(db_path, |conn| {
        // A symbol held but never quoted has no row yet.
        conn.execute(
            "INSERT INTO symbol_info (symbol, updated_at) VALUES (?1, ?2) ON CONFLICT(symbol) DO NOTHING",
            params![symbol, Utc::now().to_rfc3339()],
        )
        .map_err(|e| e.to_string())?;
        if let Some(kind) = &payload.income_kind {
            conn.execute("UPDATE symbol_info SET income_kind = ?2 WHERE symbol = ?1", params![symbol, kind])
                .map_err(|e| e.to_string())?;
        }
        if let Some(pct) = payload.franking_pct {
            conn.execute("UPDATE symbol_info SET franking_pct = ?2 WHERE symbol = ?1", params![symbol, pct])
                .map_err(|e| e.to_string())?;
        }
        if let Some(pct) = payload.dividend_withholding_pct {
            conn.execute("UPDATE symbol_info SET dividend_withholding_pct = ?2 WHERE symbol = ?1", params![symbol, pct])
                .map_err(|e| e.to_string())?;
        }
        if let Some(pct) = payload.tax_share_pct {
            // 100% is the default; storing it as null keeps "all mine" one value.
            let pct = pct.filter(|p| *p < 100.0);
            conn.execute("UPDATE symbol_info SET tax_share_pct = ?2 WHERE symbol = ?1", params![symbol, pct])
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    })
}

#[derive(Deserialize)]
pub(crate) struct AccountTaxPayload {
    /// The user's share of the account, 0–100; null makes it all theirs.
    pub(crate) tax_share_pct: Option<f64>,
}

/// Set how much of a cash account's interest and fees are the user's.
pub(crate) fn store_account_tax_share(db_path: &Path, account_id: i64, pct: Option<f64>) -> Result<bool, String> {
    if let Some(p) = pct
        && !(p.is_finite() && (0.0..=100.0).contains(&p))
    {
        return Err("tax_share_pct must be between 0 and 100".to_string());
    }
    let pct = pct.filter(|p| *p < 100.0);
    let conn = open_db(db_path).map_err(|e| e.to_string())?;
    let changed = conn
        .execute("UPDATE cash_accounts SET tax_share_pct = ?2 WHERE id = ?1", params![account_id, pct])
        .map_err(|e| e.to_string())?;
    Ok(changed > 0)
}

#[utoipa::path(put, path = "/api/v1/tax/accounts/{id}", tag = "tax",
    params(("id" = i64, Path, description = "Cash account id")),
    responses((status = 204, description = "Set the user's share of a cash account's interest and fees")))]
#[put("/api/tax/accounts/{id}")]
pub(crate) async fn update_account_tax_share(
    db_path: web::Data<PathBuf>,
    path: web::Path<i64>,
    payload: web::Json<AccountTaxPayload>,
) -> impl Responder {
    let id = path.into_inner();
    if payload.tax_share_pct.is_some_and(|p| !(p.is_finite() && (0.0..=100.0).contains(&p))) {
        return err_bad_request("tax_share_pct must be between 0 and 100");
    }
    match store_account_tax_share(&db_path, id, payload.tax_share_pct) {
        Ok(true) => HttpResponse::NoContent().finish(),
        Ok(false) => err_not_found(format!("Cash account {} not found", id)),
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "tax_settings", "api", None, &err);
            err_internal(err)
        }
    }
}

#[utoipa::path(put, path = "/api/v1/tax/symbols/{symbol}", tag = "tax",
    params(("symbol" = String, Path, description = "symbol")),
    responses((status = 204, description = "Set a symbol's income type and standing franking and withholding rates")))]
#[put("/api/tax/symbols/{symbol}")]
pub(crate) async fn update_symbol_tax_settings(
    db_path: web::Data<PathBuf>,
    path: web::Path<String>,
    payload: web::Json<SymbolTaxPayload>,
) -> impl Responder {
    let symbol = normalize_symbol(&path.into_inner());
    if let Err(err) = validate_symbol_tax(&payload) {
        return err_bad_request(err);
    }
    match store_symbol_tax_settings(&db_path, &symbol, &payload) {
        Ok(()) => HttpResponse::NoContent().finish(),
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "tax_settings", "api", Some(&symbol), &err);
            err_internal(err)
        }
    }
}

// ---------------------------------------------------------------------------
// Annual tax statements
// ---------------------------------------------------------------------------

/// The figures on a trust's annual tax statement, AUD, as typed in.
#[derive(Serialize, Deserialize, Clone, Default, Debug, PartialEq)]
pub(crate) struct TaxStatementFigures {
    pub(crate) non_primary_income: Option<f64>,
    pub(crate) franked_distributions: Option<f64>,
    pub(crate) franking_credits: Option<f64>,
    pub(crate) tfn_withheld: Option<f64>,
    pub(crate) total_capital_gains: Option<f64>,
    pub(crate) net_capital_gain: Option<f64>,
    pub(crate) assessable_foreign_income: Option<f64>,
    pub(crate) other_net_foreign_income: Option<f64>,
    pub(crate) foreign_tax_offset: Option<f64>,
    pub(crate) amit_increase: Option<f64>,
    pub(crate) amit_decrease: Option<f64>,
    pub(crate) notes: Option<String>,
}

#[derive(Serialize, Clone, Debug)]
pub(crate) struct TaxStatement {
    pub(crate) symbol: String,
    pub(crate) fy: i32,
    #[serde(flatten)]
    pub(crate) figures: TaxStatementFigures,
    pub(crate) updated_at: String,
}

impl TaxStatement {
    /// The AMIT cost base net amount, signed: positive raises the cost base.
    fn cost_base_change(&self) -> f64 {
        self.figures.amit_increase.unwrap_or(0.0) - self.figures.amit_decrease.unwrap_or(0.0)
    }
}

/// A statement's figures at the user's share of the holding.
fn scale_figures(figures: &TaxStatementFigures, owned: f64) -> TaxStatementFigures {
    let scale = |v: Option<f64>| v.map(|x| x * owned);
    TaxStatementFigures {
        non_primary_income: scale(figures.non_primary_income),
        franked_distributions: scale(figures.franked_distributions),
        franking_credits: scale(figures.franking_credits),
        tfn_withheld: scale(figures.tfn_withheld),
        total_capital_gains: scale(figures.total_capital_gains),
        net_capital_gain: scale(figures.net_capital_gain),
        assessable_foreign_income: scale(figures.assessable_foreign_income),
        other_net_foreign_income: scale(figures.other_net_foreign_income),
        foreign_tax_offset: scale(figures.foreign_tax_offset),
        amit_increase: scale(figures.amit_increase),
        amit_decrease: scale(figures.amit_decrease),
        notes: figures.notes.clone(),
    }
}

const STATEMENT_COLUMNS: &str = "symbol, fy, non_primary_income, franked_distributions, franking_credits, tfn_withheld,
    total_capital_gains, net_capital_gain, assessable_foreign_income, other_net_foreign_income,
    foreign_tax_offset, amit_increase, amit_decrease, notes, updated_at";

fn statement_from_row(r: &rusqlite::Row) -> rusqlite::Result<TaxStatement> {
    Ok(TaxStatement {
        symbol: r.get(0)?,
        fy: r.get(1)?,
        figures: TaxStatementFigures {
            non_primary_income: r.get(2)?,
            franked_distributions: r.get(3)?,
            franking_credits: r.get(4)?,
            tfn_withheld: r.get(5)?,
            total_capital_gains: r.get(6)?,
            net_capital_gain: r.get(7)?,
            assessable_foreign_income: r.get(8)?,
            other_net_foreign_income: r.get(9)?,
            foreign_tax_offset: r.get(10)?,
            amit_increase: r.get(11)?,
            amit_decrease: r.get(12)?,
            notes: r.get(13)?,
        },
        updated_at: r.get(14)?,
    })
}

fn load_statements(conn: &Connection) -> Result<Vec<TaxStatement>, String> {
    let mut stmt = conn
        .prepare(&format!("SELECT {STATEMENT_COLUMNS} FROM tax_statements ORDER BY symbol, fy"))
        .map_err(|e| e.to_string())?;
    let rows = stmt.query_map([], statement_from_row).map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
}

pub(crate) fn validate_tax_statement(fy: i32, figures: &TaxStatementFigures) -> Result<(), String> {
    if !(1990..=2100).contains(&fy) {
        return Err(format!("Income year {} is out of range", fy));
    }
    let amounts = [
        figures.non_primary_income, figures.franked_distributions, figures.franking_credits, figures.tfn_withheld,
        figures.total_capital_gains, figures.net_capital_gain, figures.assessable_foreign_income,
        figures.other_net_foreign_income, figures.foreign_tax_offset, figures.amit_increase, figures.amit_decrease,
    ];
    if amounts.iter().flatten().any(|v| !v.is_finite()) {
        return Err("Every figure must be a number".to_string());
    }
    if figures.amit_increase.is_some_and(|v| v < 0.0) || figures.amit_decrease.is_some_and(|v| v < 0.0) {
        return Err("Enter AMIT increases and decreases as positive amounts".to_string());
    }
    Ok(())
}

pub(crate) fn store_tax_statement(db_path: &Path, symbol: &str, fy: i32, figures: &TaxStatementFigures) -> Result<TaxStatement, String> {
    validate_tax_statement(fy, figures)?;
    with_tx(db_path, |conn| {
        conn.execute(
            "INSERT INTO tax_statements (symbol, fy, non_primary_income, franked_distributions, franking_credits,
                tfn_withheld, total_capital_gains, net_capital_gain, assessable_foreign_income,
                other_net_foreign_income, foreign_tax_offset, amit_increase, amit_decrease, notes, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
             ON CONFLICT(symbol, fy) DO UPDATE SET
                non_primary_income = excluded.non_primary_income,
                franked_distributions = excluded.franked_distributions,
                franking_credits = excluded.franking_credits,
                tfn_withheld = excluded.tfn_withheld,
                total_capital_gains = excluded.total_capital_gains,
                net_capital_gain = excluded.net_capital_gain,
                assessable_foreign_income = excluded.assessable_foreign_income,
                other_net_foreign_income = excluded.other_net_foreign_income,
                foreign_tax_offset = excluded.foreign_tax_offset,
                amit_increase = excluded.amit_increase,
                amit_decrease = excluded.amit_decrease,
                notes = excluded.notes,
                updated_at = excluded.updated_at",
            params![
                symbol, fy, figures.non_primary_income, figures.franked_distributions, figures.franking_credits,
                figures.tfn_withheld, figures.total_capital_gains, figures.net_capital_gain,
                figures.assessable_foreign_income, figures.other_net_foreign_income, figures.foreign_tax_offset,
                figures.amit_increase, figures.amit_decrease,
                figures.notes.as_deref().map(str::trim).filter(|n| !n.is_empty()),
                Utc::now().to_rfc3339(),
            ],
        )
        .map_err(|e| e.to_string())?;
        conn.query_row(
            &format!("SELECT {STATEMENT_COLUMNS} FROM tax_statements WHERE symbol = ?1 AND fy = ?2"),
            params![symbol, fy],
            statement_from_row,
        )
        .map_err(|e| e.to_string())
    })
}

#[utoipa::path(put, path = "/api/v1/tax/statements/{symbol}/{fy}", tag = "tax",
    params(("symbol" = String, Path, description = "symbol"), ("fy" = i32, Path, description = "Year the income year ends in")),
    responses((status = 200, description = "Save a trust's annual tax statement")))]
#[put("/api/tax/statements/{symbol}/{fy}")]
pub(crate) async fn save_tax_statement(
    db_path: web::Data<PathBuf>,
    path: web::Path<(String, i32)>,
    payload: web::Json<TaxStatementFigures>,
) -> impl Responder {
    let (symbol, fy) = path.into_inner();
    let symbol = normalize_symbol(&symbol);
    if let Err(err) = validate_tax_statement(fy, &payload) {
        return err_bad_request(err);
    }
    match store_tax_statement(&db_path, &symbol, fy, &payload) {
        Ok(statement) => HttpResponse::Ok().json(statement),
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "tax_statement", "api", Some(&symbol), &err);
            err_internal(err)
        }
    }
}

#[utoipa::path(delete, path = "/api/v1/tax/statements/{symbol}/{fy}", tag = "tax",
    params(("symbol" = String, Path, description = "symbol"), ("fy" = i32, Path, description = "Year the income year ends in")),
    responses((status = 204, description = "Delete a trust's annual tax statement")))]
#[delete("/api/tax/statements/{symbol}/{fy}")]
pub(crate) async fn delete_tax_statement(db_path: web::Data<PathBuf>, path: web::Path<(String, i32)>) -> impl Responder {
    let (symbol, fy) = path.into_inner();
    let symbol = normalize_symbol(&symbol);
    let conn = match open_db(db_path.as_ref()) {
        Ok(c) => c,
        Err(err) => return err_internal(err.to_string()),
    };
    match conn.execute("DELETE FROM tax_statements WHERE symbol = ?1 AND fy = ?2", params![symbol, fy]) {
        Ok(0) => err_not_found(format!("No {} statement for {}", TaxYear(fy).label(), symbol)),
        Ok(_) => HttpResponse::NoContent().finish(),
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "tax_statement", "api", Some(&symbol), &err.to_string());
            err_internal(err.to_string())
        }
    }
}

// ---------------------------------------------------------------------------
// Exports and the years on offer
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub(crate) struct TaxExportPayload {
    pub(crate) fy: i32,
}

/// Note that a year's report left the app — copied or downloaded — so a later
/// report can list what changed since.
#[utoipa::path(post, path = "/api/v1/tax/exports", tag = "tax", responses((status = 200, description = "Record that a year's tax report was exported")))]
#[post("/api/tax/exports")]
pub(crate) async fn record_tax_export(db_path: web::Data<PathBuf>, payload: web::Json<TaxExportPayload>) -> impl Responder {
    let fy = TaxYear(payload.fy);
    // Same format as audit_log timestamps, so the two compare as strings.
    let now = Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
    match upsert_config(&db_path, &export_key(fy), &now) {
        Ok(()) => HttpResponse::Ok().json(serde_json::json!({ "fy": fy.0, "exported_at": now })),
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "tax_export", "api", None, &err);
            err_internal(err)
        }
    }
}

#[derive(Serialize)]
pub(crate) struct TaxYearOption {
    pub(crate) fy: i32,
    pub(crate) label: String,
    pub(crate) start: String,
    pub(crate) end: String,
    pub(crate) in_progress: bool,
}

/// The income years with anything to report: from the first sale, dividend,
/// interest or fee to the year in progress. Purchases alone are not a tax
/// event, so a share bought in 2001 doesn't put 2000–01 on the list.
pub(crate) fn tax_years(conn: &Connection, today: NaiveDate) -> Result<(Vec<TaxYearOption>, i32), String> {
    let first: Option<String> = conn
        .query_row(
            "SELECT MIN(d) FROM (
                SELECT MIN(date) AS d FROM holdings_transactions WHERE transaction_type IN ('sale', 'dividend')
                UNION ALL
                SELECT MIN(date) FROM cash_transactions WHERE kind IN ('interest', 'fee', 'dividend')
             )",
            [],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    let first_statement: Option<i32> = conn
        .query_row("SELECT MIN(fy) FROM tax_statements", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;

    let current = TaxYear::containing(today);
    let earliest = first
        .as_deref()
        .and_then(rules::parse_date)
        .map(TaxYear::containing)
        .into_iter()
        .chain(first_statement.map(TaxYear))
        .min()
        .unwrap_or(current)
        .min(current);

    let years = (earliest.0..=current.0)
        .rev()
        .map(|y| {
            let fy = TaxYear(y);
            TaxYearOption {
                fy: y,
                label: fy.label(),
                start: fy.start().to_string(),
                end: fy.end().to_string(),
                in_progress: fy == current,
            }
        })
        .collect();
    // The year just finished is the one being lodged; a brand-new portfolio
    // has only the current one.
    let default = if earliest < current { current.0 - 1 } else { current.0 };
    Ok((years, default))
}

/// Every symbol and account set to less than 100% the user's.
fn partial_ownership(conn: &Connection) -> Result<Vec<serde_json::Value>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT 'symbol', symbol, tax_share_pct FROM symbol_info WHERE tax_share_pct IS NOT NULL
             UNION ALL
             SELECT 'account', name, tax_share_pct FROM cash_accounts WHERE tax_share_pct IS NOT NULL
             ORDER BY 1 DESC, 2",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| {
            Ok(serde_json::json!({ "kind": r.get::<_, String>(0)?, "name": r.get::<_, String>(1)?, "share_pct": r.get::<_, f64>(2)? }))
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
}

#[utoipa::path(get, path = "/api/v1/tax/years", tag = "tax", responses((status = 200, description = "Income years the tax report can cover")))]
#[get("/api/tax/years")]
pub(crate) async fn get_tax_years(db_path: web::Data<PathBuf>) -> impl Responder {
    let result = open_db(db_path.as_ref()).map_err(|e| e.to_string()).and_then(|conn| tax_years(&conn, today_local()));
    let result = result.and_then(|(years, default)| {
        let conn = open_db(db_path.as_ref()).map_err(|e| e.to_string())?;
        Ok((years, default, partial_ownership(&conn)?))
    });
    match result {
        Ok((years, default, ownership)) => HttpResponse::Ok().json(serde_json::json!({ "years": years, "default_fy": default, "ownership": ownership })),
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "tax_report", "api", None, &err);
            err_internal(err)
        }
    }
}

// ---------------------------------------------------------------------------
// The report's shape
// ---------------------------------------------------------------------------

/// How a column's values are formatted.
#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ColumnKind {
    Text,
    Date,
    Quantity,
    /// Two decimals.
    Money,
    /// A unit price: up to four decimals, at least two.
    Price,
    /// An exchange rate: four decimals.
    Rate,
    Integer,
    /// A box left empty for the user to write in. Its cells are always null.
    Entry,
}

#[derive(Serialize, Clone, Debug)]
pub(crate) struct TaxColumn {
    pub(crate) label: String,
    pub(crate) kind: ColumnKind,
}

#[derive(Serialize, Clone, Debug)]
pub(crate) struct TaxTable {
    pub(crate) title: String,
    pub(crate) columns: Vec<TaxColumn>,
    /// Cells are strings, numbers or null, one per column.
    pub(crate) rows: Vec<Vec<serde_json::Value>>,
}

/// One figure in a section's summary: a return label where it maps to one.
#[derive(Serialize, Clone, Debug)]
pub(crate) struct TaxSummaryLine {
    /// The return label, such as "18A". None for a working figure.
    pub(crate) item: Option<String>,
    pub(crate) label: String,
    pub(crate) amount: Option<f64>,
    /// A non-numeric answer, such as "Yes".
    pub(crate) value: Option<String>,
    pub(crate) note: Option<String>,
}

#[derive(Serialize, Clone, Debug)]
pub(crate) struct TaxSection {
    pub(crate) key: String,
    pub(crate) title: String,
    pub(crate) summary: Vec<TaxSummaryLine>,
    pub(crate) tables: Vec<TaxTable>,
    pub(crate) notes: Vec<String>,
    /// The section as plain text, for copying on its own.
    pub(crate) text: String,
}

#[derive(Serialize, Clone, Debug)]
pub(crate) struct TaxCheck {
    /// "warn" needs attention before lodging; "info" is worth knowing.
    pub(crate) severity: &'static str,
    pub(crate) section: &'static str,
    pub(crate) message: String,
}

#[derive(Serialize, Clone, Debug)]
pub(crate) struct SymbolTaxRow {
    pub(crate) symbol: String,
    pub(crate) name: Option<String>,
    pub(crate) currency: String,
    pub(crate) income_kind: &'static str,
    pub(crate) income_kind_set: bool,
    pub(crate) franking_pct: Option<f64>,
    pub(crate) withholding_pct: Option<f64>,
    /// The user's share of the holding; null is all of it.
    pub(crate) share_pct: Option<f64>,
}

#[derive(Serialize, Clone, Debug)]
pub(crate) struct AccountTaxRow {
    pub(crate) id: i64,
    pub(crate) name: String,
    pub(crate) currency: String,
    /// The user's share of the account; null is all of it.
    pub(crate) share_pct: Option<f64>,
    /// Interest credited in the year, AUD, before the share is applied.
    pub(crate) interest_aud: f64,
}

#[derive(Serialize, Clone, Debug)]
pub(crate) struct StatementRow {
    pub(crate) symbol: String,
    pub(crate) name: Option<String>,
    /// Distributions assessed in this year, AUD.
    pub(crate) cash_aud: f64,
    pub(crate) distributions: usize,
    pub(crate) statement: Option<TaxStatement>,
}

#[derive(Serialize, Clone, Debug)]
pub(crate) struct TaxReport {
    pub(crate) fy: i32,
    pub(crate) label: String,
    pub(crate) start: String,
    pub(crate) end: String,
    pub(crate) entity: &'static str,
    pub(crate) generated_at: String,
    pub(crate) in_progress: bool,
    pub(crate) detail: &'static str,
    pub(crate) last_exported_at: Option<String>,
    pub(crate) losses_carried_in: f64,
    /// "computed" from earlier years' sales, or "entered".
    pub(crate) losses_carried_in_source: &'static str,
    pub(crate) included: Vec<String>,
    pub(crate) checks: Vec<TaxCheck>,
    pub(crate) sections: Vec<TaxSection>,
    /// Settings for every symbol with income this year, for editing in place.
    pub(crate) symbols: Vec<SymbolTaxRow>,
    /// Trusts with distributions this year, and their statements.
    pub(crate) statements: Vec<StatementRow>,
    /// Cash accounts with interest or fees this year, for setting the share.
    pub(crate) accounts: Vec<AccountTaxRow>,
    /// What is left out, or counted only in part, because it isn't all the
    /// user's. Printed at the top of every form of the report.
    pub(crate) ownership: Vec<String>,
    /// What the app doesn't know and the user still has to fill in, as tables
    /// with blank `Entry` columns. Printed after the report.
    pub(crate) worksheet: Vec<TaxTable>,
    /// The whole report as plain text.
    pub(crate) text: String,
}

pub(crate) struct ReportOptions {
    pub(crate) fy: TaxYear,
    pub(crate) sections: HashSet<String>,
    pub(crate) full: bool,
    pub(crate) losses_carried_in: Option<f64>,
    pub(crate) today: NaiveDate,
    /// AUD price per symbol, for the planning section. None leaves it out.
    pub(crate) prices_aud: Option<HashMap<String, f64>>,
}

// ---------------------------------------------------------------------------
// Cells
// ---------------------------------------------------------------------------

fn r2(v: f64) -> f64 {
    let rounded = (v * 100.0).round() / 100.0;
    if rounded == 0.0 { 0.0 } else { rounded }
}

fn text(value: impl Into<String>) -> serde_json::Value {
    serde_json::Value::String(value.into())
}

fn num(value: f64, kind: ColumnKind) -> serde_json::Value {
    let rounded = match kind {
        ColumnKind::Money => r2(value),
        ColumnKind::Integer => value.round(),
        _ => (value * 1e6).round() / 1e6,
    };
    serde_json::json!(rounded)
}

fn opt(value: Option<f64>, kind: ColumnKind) -> serde_json::Value {
    value.map_or(serde_json::Value::Null, |v| num(v, kind))
}

fn col(label: &str, kind: ColumnKind) -> TaxColumn {
    TaxColumn { label: label.to_string(), kind }
}

fn line(item: Option<&str>, label: &str, amount: f64) -> TaxSummaryLine {
    TaxSummaryLine { item: item.map(str::to_string), label: label.to_string(), amount: Some(r2(amount)), value: None, note: None }
}

fn noted(mut summary: TaxSummaryLine, note: impl Into<String>) -> TaxSummaryLine {
    summary.note = Some(note.into());
    summary
}

// ---------------------------------------------------------------------------
// Gathering
// ---------------------------------------------------------------------------

/// Everything the report reads, loaded once.
struct Inputs {
    txs: Vec<HoldingTransaction>,
    by_id: HashMap<i64, usize>,
    symbols: BTreeMap<String, SymbolTax>,
    /// (symbol, ex-date) → payment date, from the fetched dividend events.
    payment_dates: HashMap<(String, String), String>,
    statements: Vec<TaxStatement>,
    walks: BTreeMap<String, ParcelWalk>,
}

fn load_inputs(db_path: &Path, conn: &Connection) -> Result<Inputs, String> {
    let mut txs = fetch_holdings(&db_path.to_path_buf())?;
    txs.sort_by(|a, b| a.date.cmp(&b.date).then(a.id.cmp(&b.id)));
    let by_id = txs.iter().enumerate().map(|(i, t)| (t.id, i)).collect();

    let mut payment_dates = HashMap::new();
    {
        let mut stmt = conn
            .prepare("SELECT symbol, ex_date, payment_date FROM dividend_events WHERE payment_date IS NOT NULL")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))
            .map_err(|e| e.to_string())?;
        for row in rows {
            let (symbol, ex_date, paid) = row.map_err(|e| e.to_string())?;
            payment_dates.insert((symbol, ex_date), paid);
        }
    }

    let statements = load_statements(conn)?;
    let mut symbol_set: BTreeSet<String> = txs.iter().map(|t| t.symbol.clone()).collect();
    symbol_set.extend(statements.iter().map(|s| s.symbol.clone()));
    let symbols = load_symbol_tax(conn, &txs, &symbol_set)?;

    // Each statement's AMIT net amount lands on the units held at the end of
    // its income year.
    let mut adjustments: HashMap<String, Vec<CostBaseAdjustment>> = HashMap::new();
    for statement in &statements {
        let amount = statement.cost_base_change();
        if amount.abs() >= 0.005 {
            adjustments.entry(statement.symbol.clone()).or_default().push(CostBaseAdjustment {
                date: TaxYear(statement.fy).end().to_string(),
                amount,
            });
        }
    }
    let mut walks = BTreeMap::new();
    for (symbol, group) in portfolio::group_by_symbol(&to_portfolio_txs(&txs)) {
        let symbol_adjustments = adjustments.get(&symbol).map(Vec::as_slice).unwrap_or(&[]);
        walks.insert(symbol, portfolio::walk_parcels(&group, symbol_adjustments));
    }

    Ok(Inputs { txs, by_id, symbols, payment_dates, statements, walks })
}

impl Inputs {
    fn tx(&self, id: i64) -> Option<&HoldingTransaction> {
        self.by_id.get(&id).map(|i| &self.txs[*i])
    }

    fn kind_of(&self, symbol: &str) -> IncomeKind {
        self.symbols.get(symbol).map_or(IncomeKind::Company, |s| s.kind)
    }

    /// When a dividend was paid: as recorded on it from its statement, else
    /// the fetched event's date for the event it stands for.
    fn paid(&self, tx: &HoldingTransaction) -> Option<String> {
        tx.payment_date.clone().or_else(|| {
            let event_date = tx.source_ex_date.clone().unwrap_or_else(|| tx.date.clone());
            self.payment_dates.get(&(tx.symbol.clone(), event_date)).cloned()
        })
    }

    /// The year a dividend is taxed in: when it was paid, for a company or a
    /// foreign payer; the period it was for, for a trust.
    fn income_year(&self, tx: &HoldingTransaction) -> Option<TaxYear> {
        let ex_date = rules::parse_date(&tx.date)?;
        match self.kind_of(&tx.symbol) {
            IncomeKind::Trust => Some(rules::distribution_tax_year(ex_date)),
            _ => {
                let paid = self.paid(tx).as_deref().and_then(rules::parse_date).unwrap_or(ex_date);
                Some(TaxYear::containing(paid))
            }
        }
    }

    /// The user's share of a symbol, as a fraction.
    fn share_of(&self, symbol: &str) -> f64 {
        self.symbols.get(symbol).map_or(1.0, SymbolTax::share)
    }

    /// The year's dividends of one kind, leaving out symbols that aren't the
    /// user's at all.
    fn dividends_in(&self, fy: TaxYear, kind: IncomeKind) -> Vec<&HoldingTransaction> {
        self.txs
            .iter()
            .filter(|t| t.transaction_type == "dividend" && self.kind_of(&t.symbol) == kind && self.income_year(t) == Some(fy))
            .filter(|t| self.share_of(&t.symbol) > 0.0)
            .collect()
    }
}

/// A dividend's gross in the currency it was paid in.
fn dividend_native_gross(tx: &HoldingTransaction) -> f64 {
    let aud = tx.amount.unwrap_or(0.0);
    if tx.currency == "AUD" {
        return aud;
    }
    match (tx.quantity, tx.original_price, tx.fx_rate) {
        (Some(q), Some(unit), _) => q * unit,
        (_, _, Some(rate)) if rate > 0.0 => aud / rate,
        _ => aud,
    }
}

/// Tax withheld from a dividend in the currency it was paid in, and where the
/// figure came from.
fn dividend_withheld(tx: &HoldingTransaction, info: Option<&SymbolTax>, native_gross: f64) -> (f64, String) {
    if let Some(amount) = tx.withholding_amount.filter(|a| *a > 0.0) {
        return (amount, "recorded".to_string());
    }
    match info.and_then(|i| i.withholding_pct).filter(|p| *p > 0.0) {
        Some(pct) => (native_gross * pct / 100.0, format!("standing {}%", trim_number(pct))),
        None => (0.0, "none".to_string()),
    }
}

fn trim_number(value: f64) -> String {
    let s = format!("{:.4}", value);
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// One parcel's gain, AUD, and whether it qualifies for the discount.
struct ParcelResult {
    proceeds: f64,
    brokerage: f64,
    cost: f64,
    gain: f64,
    eligible: bool,
}

/// `owned` is the user's share of the holding: a joint holding's parcels are
/// half theirs, and so is every figure that follows from them.
fn parcel_result(sale: &SaleMatch, parcel: &stocks::portfolio::ParcelUse, owned: f64) -> ParcelResult {
    let quantity = parcel.quantity * owned;
    let of_sale = if sale.quantity > 0.0 { quantity / sale.quantity } else { 0.0 };
    let proceeds = r2(quantity * sale.price);
    let brokerage = r2(sale.brokerage.unwrap_or(0.0) * of_sale);
    let cost = r2(quantity * parcel.cost_per_share);
    let eligible = match (rules::parse_date(&parcel.acquired), rules::parse_date(&sale.date)) {
        (Some(acquired), Some(sold)) => rules::discount_eligible(acquired, sold),
        _ => false,
    };
    ParcelResult { proceeds, brokerage, cost, gain: r2(proceeds - brokerage - cost), eligible }
}

/// Every year's own capital gains and losses, before carried-forward losses:
/// sales parcel by parcel, plus trusts' statements.
fn capital_by_year(inputs: &Inputs) -> BTreeMap<i32, CapitalGainsInput> {
    let mut years: BTreeMap<i32, CapitalGainsInput> = BTreeMap::new();
    for (symbol, walk) in &inputs.walks {
        let owned = inputs.share_of(symbol);
        if owned <= 0.0 {
            continue;
        }
        for sale in &walk.sales {
            let Some(date) = rules::parse_date(&sale.date) else { continue };
            let entry = years.entry(TaxYear::containing(date).0).or_default();
            for parcel in &sale.parcels {
                let result = parcel_result(sale, parcel, owned);
                if result.gain < 0.0 {
                    entry.losses += -result.gain;
                } else if result.eligible {
                    entry.discount_gains += result.gain;
                } else {
                    entry.other_gains += result.gain;
                }
            }
        }
    }
    for statement in &inputs.statements {
        let owned = inputs.share_of(&statement.symbol);
        if owned <= 0.0 {
            continue;
        }
        let (discount, other) = rules::split_trust_capital_gains(
            statement.figures.total_capital_gains.unwrap_or(0.0) * owned,
            statement.figures.net_capital_gain.unwrap_or(0.0) * owned,
        );
        let entry = years.entry(statement.fy).or_default();
        entry.discount_gains += discount;
        entry.other_gains += other;
    }
    years
}

/// Net capital losses reaching `fy` from the years before it, as far back as
/// the app's records go.
fn losses_carried_into(years: &BTreeMap<i32, CapitalGainsInput>, fy: TaxYear) -> f64 {
    let mut carried = 0.0;
    for (_, input) in years.range(..fy.0) {
        carried = rules::net_capital_gain(CapitalGainsInput { carried_in: carried, ..*input }, ENTITY).carried_out;
    }
    carried
}

// ---------------------------------------------------------------------------
// Sections
// ---------------------------------------------------------------------------

/// What the sections found that the label summary needs.
#[derive(Default)]
struct Totals {
    interest: Option<f64>,
    dividends: Option<(f64, f64, f64, f64)>,
    trusts: Option<TaxStatementFigures>,
    /// Trusts whose franking in `trusts` is estimated from their recorded
    /// distributions, having no annual tax statement yet.
    trusts_estimated: Vec<String>,
    capital: Option<(bool, CapitalGainsResult)>,
    foreign: Option<(f64, f64)>,
    deductions: Option<f64>,
}

struct Builder<'a> {
    inputs: &'a Inputs,
    conn: &'a Connection,
    options: &'a ReportOptions,
    fy: TaxYear,
    checks: Vec<TaxCheck>,
    totals: Totals,
    /// Symbols with a sale this year, whose earlier purchases matter too.
    sold_symbols: BTreeSet<String>,
    /// Tables of what the user still has to fill in, built as the sections
    /// find the gaps.
    worksheet: Vec<TaxTable>,
}

impl Builder<'_> {
    fn warn(&mut self, section: &'static str, message: String) {
        self.checks.push(TaxCheck { severity: "warn", section, message });
    }

    fn info(&mut self, section: &'static str, message: String) {
        self.checks.push(TaxCheck { severity: "info", section, message });
    }

    fn sales_in_year(&self) -> Vec<(&String, &SaleMatch)> {
        let mut sales: Vec<(&String, &SaleMatch)> = self
            .inputs
            .walks
            .iter()
            .flat_map(|(symbol, walk)| walk.sales.iter().map(move |s| (symbol, s)))
            .filter(|(_, sale)| rules::parse_date(&sale.date).is_some_and(|d| self.fy.contains(d)))
            .filter(|(symbol, _)| self.inputs.share_of(symbol) > 0.0)
            .collect();
        sales.sort_by(|a, b| a.1.date.cmp(&b.1.date).then(a.0.cmp(b.0)));
        sales
    }

    fn currency_of_tx(&self, id: i64) -> String {
        self.inputs.tx(id).map_or_else(|| "AUD".to_string(), |t| t.currency.clone())
    }

    /// AUD per unit of a trade's currency: its stored rate, else what its two
    /// prices imply. None for an AUD trade.
    fn trade_rate(&self, id: i64) -> Option<f64> {
        let tx = self.inputs.tx(id)?;
        if tx.currency == "AUD" {
            return None;
        }
        tx.fx_rate.or_else(|| match (tx.price, tx.original_price) {
            (Some(aud), Some(native)) if native > 0.0 => Some(aud / native),
            _ => None,
        })
    }

    fn capital_gains(&mut self, years: &BTreeMap<i32, CapitalGainsInput>, carried_in: f64, carried_source: &str) -> TaxSection {
        let fy = self.fy;
        let mut sale_rows = Vec::new();
        let mut parcel_rows = Vec::new();
        let mut unmatched = Vec::new();
        let mut missing_rates = BTreeSet::new();
        let mut any_foreign = false;

        let sales: Vec<(String, SaleMatch)> = self.sales_in_year().into_iter().map(|(s, m)| (s.clone(), m.clone())).collect();
        for (symbol, sale) in &sales {
            let owned = self.inputs.share_of(symbol);
            let label = share_label(symbol, owned);
            let currency = self.currency_of_tx(sale.sale_id);
            let foreign = currency != "AUD";
            any_foreign |= foreign;
            let sale_rate = self.trade_rate(sale.sale_id);
            if foreign && self.inputs.tx(sale.sale_id).is_some_and(|t| t.fx_rate.is_none()) {
                missing_rates.insert(format!("{} sale {}", symbol, sale.date));
            }

            let mut sale_proceeds = 0.0;
            let mut sale_brokerage = 0.0;
            let mut sale_cost = 0.0;
            let mut sale_gain = 0.0;
            for parcel in &sale.parcels {
                let result = parcel_result(sale, parcel, owned);
                sale_proceeds += result.proceeds;
                sale_brokerage += result.brokerage;
                sale_cost += result.cost;
                sale_gain += result.gain;

                let purchase_rate = self.trade_rate(parcel.purchase_id);
                if foreign && self.inputs.tx(parcel.purchase_id).is_some_and(|t| t.fx_rate.is_none()) {
                    missing_rates.insert(format!("{} purchase {}", symbol, parcel.acquired));
                }
                let quantity = parcel.quantity * owned;
                let proceeds_native = quantity * sale.native_price;
                let cost_native = quantity * parcel.native_cost_per_share;
                let gain_native = match sale_rate {
                    Some(rate) if rate > 0.0 => Some(proceeds_native - result.brokerage / rate - cost_native),
                    _ if !foreign => Some(result.gain),
                    _ => None,
                };
                let days_held = match (rules::parse_date(&parcel.acquired), rules::parse_date(&sale.date)) {
                    (Some(a), Some(s)) => Some((s - a).num_days() as f64),
                    _ => None,
                };
                let category = if result.gain < 0.0 { "Loss" } else if result.eligible { "Discount" } else { "Other" };
                parcel_rows.push(vec![
                    text(label.as_str()),
                    text(sale.date.as_str()),
                    text(parcel.acquired.as_str()),
                    opt(days_held, ColumnKind::Integer),
                    num(quantity, ColumnKind::Quantity),
                    text(currency.as_str()),
                    num(sale.native_price, ColumnKind::Price),
                    opt(sale_rate, ColumnKind::Rate),
                    num(proceeds_native, ColumnKind::Money),
                    num(result.proceeds, ColumnKind::Money),
                    num(result.brokerage, ColumnKind::Money),
                    opt(purchase_rate, ColumnKind::Rate),
                    num(cost_native, ColumnKind::Money),
                    num(result.cost, ColumnKind::Money),
                    opt(gain_native, ColumnKind::Money),
                    num(result.gain, ColumnKind::Money),
                    text(if result.eligible { "Yes" } else { "No" }),
                    text(category),
                ]);
            }
            if sale.unmatched > 0.0 {
                unmatched.push(format!("{} sold {} on {}", symbol, trim_number(sale.unmatched), sale.date));
                parcel_rows.push(vec![
                    text(label.as_str()),
                    text(sale.date.as_str()),
                    serde_json::Value::Null,
                    serde_json::Value::Null,
                    num(sale.unmatched * owned, ColumnKind::Quantity),
                    text(currency.as_str()),
                    num(sale.native_price, ColumnKind::Price),
                    opt(sale_rate, ColumnKind::Rate),
                    num(sale.unmatched * owned * sale.native_price, ColumnKind::Money),
                    num(sale.unmatched * owned * sale.price, ColumnKind::Money),
                    serde_json::Value::Null,
                    serde_json::Value::Null,
                    serde_json::Value::Null,
                    serde_json::Value::Null,
                    serde_json::Value::Null,
                    serde_json::Value::Null,
                    serde_json::Value::Null,
                    text("No purchase recorded"),
                ]);
            }
            sale_rows.push(vec![
                text(label.as_str()),
                text(sale.date.as_str()),
                num(sale.quantity * owned, ColumnKind::Quantity),
                text(currency.as_str()),
                num(sale.native_price, ColumnKind::Price),
                opt(sale_rate, ColumnKind::Rate),
                num(sale.quantity * owned * sale.native_price, ColumnKind::Money),
                num(sale_proceeds, ColumnKind::Money),
                num(sale_brokerage, ColumnKind::Money),
                num(sale_cost, ColumnKind::Money),
                num(sale_gain, ColumnKind::Money),
                num(sale.parcels.len() as f64, ColumnKind::Integer),
            ]);
        }

        // Trusts' capital gains are part of the same netting.
        let mut trust_rows = Vec::new();
        let mut trust_total = 0.0;
        for statement in self.inputs.statements.iter().filter(|s| s.fy == fy.0) {
            let owned = self.inputs.share_of(&statement.symbol);
            let total = statement.figures.total_capital_gains.unwrap_or(0.0) * owned;
            let net = statement.figures.net_capital_gain.unwrap_or(0.0) * owned;
            if total == 0.0 && net == 0.0 {
                continue;
            }
            let (discount, other) = rules::split_trust_capital_gains(total, net);
            trust_total += total;
            trust_rows.push(vec![
                text(share_label(&statement.symbol, owned)),
                num(total, ColumnKind::Money),
                num(net, ColumnKind::Money),
                num(discount, ColumnKind::Money),
                num(other, ColumnKind::Money),
            ]);
        }

        let input = years.get(&fy.0).copied().unwrap_or_default();
        let result = rules::net_capital_gain(CapitalGainsInput { carried_in, ..input }, ENTITY);
        let event = !sales.is_empty() || trust_total > 0.0;
        self.totals.capital = Some((event, result));

        if !unmatched.is_empty() {
            self.warn("cgt", format!(
                "No purchase is recorded for some shares sold, so they have no cost base and are left out of the totals: {}.",
                unmatched.join("; ")
            ));
        }
        if !missing_rates.is_empty() {
            self.warn("cgt", format!(
                "Foreign trades with no stored exchange rate (the rate is implied from their AUD price): {}.",
                missing_rates.into_iter().collect::<Vec<_>>().join("; ")
            ));
        }

        let mut summary = vec![
            TaxSummaryLine { item: Some("18G".into()), label: "Did you have a CGT event during the year?".into(), amount: None, value: Some(if event { "Yes" } else { "No" }.into()), note: None },
            line(None, "Gains on parcels held 12 months or more (before discount)", input.discount_gains),
            line(None, "Gains on parcels held under 12 months", input.other_gains),
        ];
        if trust_total > 0.0 {
            summary.push(noted(line(None, "of which from trusts' annual tax statements", trust_total), "included in the two lines above"));
        }
        summary.push(line(None, "Capital losses this year", input.losses));
        summary.push(noted(
            line(None, "Net capital losses carried forward from earlier years", carried_in),
            if carried_source == "entered" { "as entered" } else { "worked out from the sales recorded in the app" },
        ));
        summary.push(line(None, "Losses applied this year", result.current_losses_applied + result.carried_losses_applied));
        summary.push(line(Some("18H"), "Total current year capital gains", result.total_gains));
        summary.push(line(None, &format!("CGT discount ({}%)", trim_number(ENTITY.discount_fraction() * 100.0)), result.discount));
        summary.push(line(Some("18A"), "Net capital gain", result.net_capital_gain));
        summary.push(line(Some("18V"), "Net capital losses carried forward to later income years", result.carried_out));

        let mut tables = Vec::new();
        if self.options.full {
            tables.push(TaxTable {
                title: "Sales".into(),
                columns: vec![
                    col("Symbol", ColumnKind::Text), col("Sale date", ColumnKind::Date), col("Quantity", ColumnKind::Quantity),
                    col("Currency", ColumnKind::Text), col("Sale price", ColumnKind::Price), col("FX rate", ColumnKind::Rate),
                    col("Proceeds (trade ccy)", ColumnKind::Money), col("Proceeds (AUD)", ColumnKind::Money),
                    col("Brokerage (AUD)", ColumnKind::Money), col("Cost base (AUD)", ColumnKind::Money),
                    col("Gain (AUD)", ColumnKind::Money), col("Parcels", ColumnKind::Integer),
                ],
                rows: sale_rows,
            });
            tables.push(TaxTable {
                title: "Parcels sold".into(),
                columns: vec![
                    col("Symbol", ColumnKind::Text), col("Sale date", ColumnKind::Date), col("Acquired", ColumnKind::Date),
                    col("Days held", ColumnKind::Integer), col("Quantity", ColumnKind::Quantity), col("Currency", ColumnKind::Text),
                    col("Sale price", ColumnKind::Price), col("Sale FX rate", ColumnKind::Rate),
                    col("Proceeds (trade ccy)", ColumnKind::Money), col("Proceeds (AUD)", ColumnKind::Money),
                    col("Brokerage (AUD)", ColumnKind::Money), col("Purchase FX rate", ColumnKind::Rate),
                    col("Cost base (trade ccy)", ColumnKind::Money), col("Cost base (AUD)", ColumnKind::Money),
                    col("Gain (trade ccy)", ColumnKind::Money), col("Gain (AUD)", ColumnKind::Money),
                    col("12 months+", ColumnKind::Text), col("Category", ColumnKind::Text),
                ],
                rows: parcel_rows,
            });
            if !trust_rows.is_empty() {
                tables.push(TaxTable {
                    title: "Capital gains from trusts' annual tax statements".into(),
                    columns: vec![
                        col("Symbol", ColumnKind::Text), col("Total gains (18H)", ColumnKind::Money),
                        col("Net capital gain (18A)", ColumnKind::Money), col("Discount gains, grossed up", ColumnKind::Money),
                        col("Other gains", ColumnKind::Money),
                    ],
                    rows: trust_rows,
                });
            }
        }

        let mut notes = vec![
            "Parcels are matched first in, first out — the same matching as the holdings screens.".to_string(),
            "The cost base includes the purchase brokerage; the sale's brokerage comes off its proceeds.".to_string(),
        ];
        if any_foreign {
            notes.push("Foreign trades: the cost base is converted at the purchase's exchange rate and the proceeds at the sale's, as the ATO requires. Trade-currency columns are for reference.".to_string());
        }
        let adjusted: Vec<String> = self
            .inputs
            .statements
            .iter()
            .filter(|s| s.cost_base_change().abs() >= 0.005 && self.sold_symbols.contains(&s.symbol))
            .map(|s| format!("{} {}", s.symbol, TaxYear(s.fy).label()))
            .collect();
        if !adjusted.is_empty() {
            notes.push(format!("Cost bases include the AMIT adjustments from these annual tax statements: {}.", adjusted.join(", ")));
        }
        notes.push("Not modelled: share splits and consolidations, returns of capital, demergers and other corporate actions. Adjust those parcels' cost bases by hand.".to_string());
        if carried_source == "computed" {
            notes.push("Losses carried forward are worked out from sales recorded in the app. If last year's return carried a different figure, enter it and generate again.".to_string());
        }

        TaxSection { key: "cgt".into(), title: "Capital gains".into(), summary, tables, notes, text: String::new() }
    }

    fn franking(tx: &HoldingTransaction, info: Option<&SymbolTax>, gross: f64) -> Option<(f64, f64, String)> {
        match (tx.franked_amount, tx.franking_credit) {
            (Some(franked), credit) => {
                let franked = franked.clamp(0.0, gross.max(0.0));
                Some((franked, credit.unwrap_or_else(|| rules::franking_credit(franked, rules::COMPANY_TAX_RATE)), "statement".into()))
            }
            (None, Some(credit)) => {
                // Only the credit was entered: the franked amount it implies.
                let franked = (credit * (1.0 - rules::COMPANY_TAX_RATE) / rules::COMPANY_TAX_RATE).clamp(0.0, gross.max(0.0));
                Some((franked, credit, "statement".into()))
            }
            (None, None) => info.and_then(|i| i.franking_pct).map(|pct| {
                let franked = gross * pct.clamp(0.0, 100.0) / 100.0;
                (franked, rules::franking_credit(franked, rules::COMPANY_TAX_RATE), format!("standing {}%", trim_number(pct)))
            }),
        }
    }

    fn dividends(&mut self) -> TaxSection {
        let fy = self.fy;
        let mut rows = Vec::new();
        let (mut unfranked_total, mut franked_total, mut credit_total, mut tfn_total, mut unknown_total) = (0.0, 0.0, 0.0, 0.0, 0.0);
        let mut unknown_symbols = BTreeSet::new();
        let mut late_unknown = Vec::new();
        let mut to_enter = Vec::new();

        let dividends: Vec<HoldingTransaction> = self.inputs.dividends_in(fy, IncomeKind::Company).into_iter().cloned().collect();
        for tx in &dividends {
            let info = self.inputs.symbols.get(&tx.symbol);
            // Worked out on the whole payment, then the user's share taken:
            // a statement's franking is for the whole holding too.
            let owned = self.inputs.share_of(&tx.symbol);
            let label = share_label(&tx.symbol, owned);
            let gross_full = tx.amount.unwrap_or(0.0);
            let gross = r2(gross_full * owned);
            let native = dividend_native_gross(tx);
            let (withheld_native, _) = dividend_withheld(tx, info, native);
            let rate = if tx.currency == "AUD" { 1.0 } else if native > 0.0 { gross_full / native } else { 1.0 };
            let withheld = r2(withheld_native * rate * owned);
            let paid = self.inputs.paid(tx);
            if paid.is_none() && rules::parse_date(&tx.date).is_some_and(|d| (fy.end() - d).num_days() < 60) {
                late_unknown.push(format!("{} ex {}", tx.symbol, tx.date));
            }

            let franking = Self::franking(tx, info, gross_full);
            let (franked, unfranked, credit, source) = match &franking {
                Some((franked, credit, source)) => {
                    let franked = r2(franked * owned);
                    (Some(franked), Some(r2(gross - franked)), Some(r2(credit * owned)), source.clone())
                }
                None => {
                    unknown_symbols.insert(tx.symbol.clone());
                    unknown_total += gross;
                    to_enter.push(vec![
                        text(label.as_str()),
                        text(tx.date.as_str()),
                        paid.clone().map_or(serde_json::Value::Null, text),
                        opt(tx.quantity, ColumnKind::Quantity),
                        num(gross_full, ColumnKind::Money),
                        serde_json::Value::Null,
                        serde_json::Value::Null,
                        serde_json::Value::Null,
                    ]);
                    (None, None, None, "not recorded".to_string())
                }
            };
            franked_total += franked.unwrap_or(0.0);
            unfranked_total += unfranked.unwrap_or(0.0);
            credit_total += credit.unwrap_or(0.0);
            tfn_total += withheld;

            rows.push(vec![
                text(label.as_str()),
                text(tx.date.as_str()),
                paid.map_or(serde_json::Value::Null, text),
                tx.dividend_type.clone().map_or(serde_json::Value::Null, text),
                opt(tx.quantity.map(|q| q * owned), ColumnKind::Quantity),
                num(gross, ColumnKind::Money),
                opt(franked, ColumnKind::Money),
                opt(unfranked, ColumnKind::Money),
                opt(credit, ColumnKind::Money),
                num(withheld, ColumnKind::Money),
                text(source),
            ]);
        }

        if !to_enter.is_empty() {
            self.worksheet.push(TaxTable {
                title: "Dividends: franking to copy from each statement".into(),
                columns: vec![
                    col("Symbol", ColumnKind::Text), col("Ex-date", ColumnKind::Date), col("Paid", ColumnKind::Date),
                    col("Shares", ColumnKind::Quantity), col("Gross paid (AUD)", ColumnKind::Money),
                    col("Franked amount", ColumnKind::Entry), col("Franking credit", ColumnKind::Entry),
                    col("TFN withheld", ColumnKind::Entry),
                ],
                rows: to_enter,
            });
        }
        if !unknown_symbols.is_empty() {
            self.warn("dividends", format!(
                "Franking isn't recorded for ${} of dividends from {}. Enter it on each dividend from its statement, or set a standing franking % for the symbol.",
                money_text(unknown_total),
                unknown_symbols.into_iter().collect::<Vec<_>>().join(", ")
            ));
        }
        if !late_unknown.is_empty() {
            self.info("dividends", format!(
                "No payment date is known for these, so they are counted as paid by 30 June. Any paid in July belongs to the next year: {}.",
                late_unknown.join(", ")
            ));
        }
        // The other end: last year's late ex-dates may have been paid in this one.
        let early_unknown: Vec<String> = self
            .inputs
            .txs
            .iter()
            .filter(|t| t.transaction_type == "dividend" && self.inputs.kind_of(&t.symbol) == IncomeKind::Company)
            .filter(|t| self.inputs.share_of(&t.symbol) > 0.0 && self.inputs.paid(t).is_none())
            .filter(|t| rules::parse_date(&t.date).is_some_and(|d| d < fy.start() && (fy.start() - d).num_days() <= 60))
            .map(|t| format!("{} ex {}", t.symbol, t.date))
            .collect();
        if !early_unknown.is_empty() {
            self.info("dividends", format!(
                "These went ex in the last weeks of {} with no known payment date, so they are counted in that year. Any paid on or after 1 July belongs in this one: {}.",
                TaxYear(fy.0 - 1).label(),
                early_unknown.join(", ")
            ));
        }
        self.totals.dividends = Some((r2(unfranked_total), r2(franked_total), r2(credit_total), r2(tfn_total)));

        let mut summary = vec![
            line(Some("11S"), "Unfranked amount", unfranked_total),
            line(Some("11T"), "Franked amount", franked_total),
            line(Some("11U"), "Franking credit", credit_total),
            line(Some("11V"), "TFN amounts withheld from dividends", tfn_total),
        ];
        if unknown_total > 0.0 {
            summary.push(noted(line(None, "Dividends with franking not recorded", unknown_total), "not in the four lines above"));
        }
        let tables = if self.options.full {
            vec![TaxTable {
                title: "Dividends".into(),
                columns: vec![
                    col("Symbol", ColumnKind::Text), col("Ex-date", ColumnKind::Date), col("Paid", ColumnKind::Date),
                    col("Type", ColumnKind::Text), col("Shares", ColumnKind::Quantity), col("Gross (AUD)", ColumnKind::Money),
                    col("Franked", ColumnKind::Money),
                    col("Unfranked", ColumnKind::Money), col("Franking credit", ColumnKind::Money),
                    col("TFN withheld", ColumnKind::Money), col("Franking from", ColumnKind::Text),
                ],
                rows,
            }]
        } else {
            Vec::new()
        };
        TaxSection {
            key: "dividends".into(),
            title: "Australian dividends".into(),
            summary,
            tables,
            notes: vec![
                "A dividend is income in the year it was paid, so the payment date decides the year, not the ex-date.".into(),
                "Franking credits worked out from a standing % assume the 30% company tax rate; a statement's own figure is always used when entered.".into(),
            ],
            text: String::new(),
        }
    }

    fn foreign(&mut self) -> TaxSection {
        let fy = self.fy;
        let mut rows = Vec::new();
        let (mut gross_total, mut withheld_total) = (0.0, 0.0);
        let mut standing: BTreeMap<String, (f64, usize)> = BTreeMap::new();
        let mut to_enter = Vec::new();

        let dividends: Vec<HoldingTransaction> = self.inputs.dividends_in(fy, IncomeKind::Foreign).into_iter().cloned().collect();
        for tx in &dividends {
            let info = self.inputs.symbols.get(&tx.symbol);
            let owned = self.inputs.share_of(&tx.symbol);
            let label = share_label(&tx.symbol, owned);
            let native_full = dividend_native_gross(tx);
            let native = native_full * owned;
            let paid = self.inputs.paid(tx);
            let tax_date = paid.clone().unwrap_or_else(|| tx.date.clone());
            // Foreign income is converted at the rate on the day it was paid.
            let (rate, rate_date) = if tx.currency == "AUD" {
                (None, None)
            } else if let Some(rate) = fx_rate_on(self.conn, &tx.currency, &tax_date) {
                (Some(rate), Some(tax_date.clone()))
            } else {
                (tx.fx_rate, Some(tx.date.clone()))
            };
            let gross_aud = r2(match rate {
                Some(r) => native * r,
                None if tx.currency == "AUD" => native,
                None => tx.amount.unwrap_or(native),
            });
            let (withheld_full, source) = dividend_withheld(tx, info, native_full);
            let withheld_native = withheld_full * owned;
            if source.starts_with("standing") {
                let entry = standing.entry(tx.symbol.clone()).or_insert((info.and_then(|i| i.withholding_pct).unwrap_or(0.0), 0));
                entry.1 += 1;
                to_enter.push(vec![
                    text(label.as_str()),
                    text(tx.date.as_str()),
                    paid.clone().map_or(serde_json::Value::Null, text),
                    text(tx.currency.as_str()),
                    num(native_full, ColumnKind::Money),
                    num(withheld_full, ColumnKind::Money),
                    serde_json::Value::Null,
                ]);
            }
            let withheld_aud = r2(match rate { Some(r) => withheld_native * r, None => withheld_native });
            gross_total += gross_aud;
            withheld_total += withheld_aud;

            rows.push(vec![
                text(label.as_str()),
                text(tx.date.as_str()),
                paid.map_or(serde_json::Value::Null, text),
                opt(tx.quantity.map(|q| q * owned), ColumnKind::Quantity),
                text(tx.currency.as_str()),
                num(native, ColumnKind::Money),
                opt(rate, ColumnKind::Rate),
                rate_date.map_or(serde_json::Value::Null, text),
                num(gross_aud, ColumnKind::Money),
                num(withheld_native, ColumnKind::Money),
                num(withheld_aud, ColumnKind::Money),
                text(source),
                num(gross_aud - withheld_aud, ColumnKind::Money),
            ]);
        }

        if !to_enter.is_empty() {
            self.worksheet.push(TaxTable {
                title: "Foreign dividends: tax actually withheld, from each payment statement".into(),
                columns: vec![
                    col("Symbol", ColumnKind::Text), col("Ex-date", ColumnKind::Date), col("Paid", ColumnKind::Date),
                    col("Currency", ColumnKind::Text), col("Gross paid", ColumnKind::Money),
                    col("Estimated withheld", ColumnKind::Money), col("Actual withheld", ColumnKind::Entry),
                ],
                rows: to_enter,
            });
        }
        for (symbol, (pct, count)) in &standing {
            let treaty = if *pct >= 30.0 { " A US payer withholds 15% when a W-8BEN form is on file; other countries' rates differ." } else { "" };
            self.warn("foreign", format!(
                "{}: tax withheld on {} payment{} is estimated at the standing {}% rate. Check it against the payment statements.{}",
                symbol, count, if *count == 1 { "" } else { "s" }, trim_number(*pct), treaty
            ));
        }
        self.totals.foreign = Some((r2(gross_total), r2(withheld_total)));

        let mut offset = line(Some("20O"), "Foreign income tax offset", withheld_total);
        if withheld_total > 0.0 && withheld_total <= 1000.0 {
            offset = noted(offset, "$1,000 or less, so it can be claimed without working out the offset limit");
        }
        let summary = vec![
            line(Some("20E"), "Assessable foreign source income", gross_total),
            line(Some("20M"), "Other net foreign source income", gross_total),
            offset,
        ];
        let tables = if self.options.full {
            vec![TaxTable {
                title: "Foreign dividends".into(),
                columns: vec![
                    col("Symbol", ColumnKind::Text), col("Ex-date", ColumnKind::Date), col("Paid", ColumnKind::Date),
                    col("Shares", ColumnKind::Quantity), col("Currency", ColumnKind::Text), col("Gross (trade ccy)", ColumnKind::Money),
                    col("FX rate", ColumnKind::Rate), col("Rate date", ColumnKind::Date), col("Gross (AUD)", ColumnKind::Money),
                    col("Tax withheld (trade ccy)", ColumnKind::Money), col("Tax withheld (AUD)", ColumnKind::Money),
                    col("Withholding from", ColumnKind::Text), col("Net (AUD)", ColumnKind::Money),
                ],
                rows,
            }]
        } else {
            Vec::new()
        };
        TaxSection {
            key: "foreign".into(),
            title: "Foreign income".into(),
            summary,
            tables,
            notes: vec![
                "Converted at the exchange rate on the day each payment was made (the rate stored with the dividend when none is held for that day).".into(),
                "Trusts' foreign income is in the Trusts section; add it to these labels.".into(),
            ],
            text: String::new(),
        }
    }

    fn interest(&mut self) -> Result<TaxSection, String> {
        let fy = self.fy;
        let mut stmt = self
            .conn
            .prepare(
                "SELECT c.date, a.name, a.currency, c.amount, c.notes, a.tax_share_pct
                   FROM cash_transactions c JOIN cash_accounts a ON a.id = c.account_id
                  WHERE c.kind = 'interest' AND c.amount > 0 AND c.date BETWEEN ?1 AND ?2
                  ORDER BY c.date, c.id",
            )
            .map_err(|e| e.to_string())?;
        let found = stmt
            .query_map(params![fy.start().to_string(), fy.end().to_string()], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, f64>(3)?,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, Option<f64>>(5)?,
                ))
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;

        let mut rows = Vec::new();
        let (mut aud_total, mut foreign_total) = (0.0, 0.0);
        let mut foreign_accounts = BTreeSet::new();
        for (date, account, currency, amount, notes, share_pct) in found {
            let owned = share_fraction(share_pct);
            if owned <= 0.0 {
                continue;
            }
            let rate = if currency == "AUD" { None } else { fx_rate_on(self.conn, &currency, &date) };
            if currency != "AUD" && rate.is_none() {
                self.warn("interest", format!("No {}/AUD rate for {}; interest of {} {:.2} on {} is shown unconverted.", currency, date, currency, amount, account));
            }
            // The full credit, as on the bank statement, and the user's share
            // of it, which is what is taxed.
            let aud = r2(amount * rate.unwrap_or(1.0));
            let yours = r2(aud * owned);
            if currency == "AUD" {
                aud_total += yours;
            } else {
                foreign_total += yours;
                foreign_accounts.insert(account.clone());
            }
            rows.push(vec![
                text(date),
                text(account),
                text(currency),
                num(amount, ColumnKind::Money),
                opt(rate, ColumnKind::Rate),
                num(aud, ColumnKind::Money),
                num(owned * 100.0, ColumnKind::Quantity),
                num(yours, ColumnKind::Money),
                notes.map_or(serde_json::Value::Null, text),
            ]);
        }
        if foreign_total > 0.0 {
            self.info("interest", format!(
                "Interest on foreign-currency accounts ({}) is listed separately: if it is foreign-sourced it belongs in item 20, not 10L.",
                foreign_accounts.into_iter().collect::<Vec<_>>().join(", ")
            ));
        }
        self.totals.interest = Some(r2(aud_total));

        let mut summary = vec![line(Some("10L"), "Gross interest (AUD accounts)", aud_total)];
        if foreign_total > 0.0 {
            summary.push(noted(line(None, "Interest on foreign-currency accounts (AUD)", foreign_total), "item 20 if foreign-sourced"));
        }
        let tables = if self.options.full {
            vec![TaxTable {
                title: "Interest received".into(),
                columns: vec![
                    col("Date", ColumnKind::Date), col("Account", ColumnKind::Text), col("Currency", ColumnKind::Text),
                    col("Amount (account ccy)", ColumnKind::Money), col("FX rate", ColumnKind::Rate),
                    col("Amount (AUD)", ColumnKind::Money), col("Your share %", ColumnKind::Quantity),
                    col("Yours (AUD)", ColumnKind::Money), col("Notes", ColumnKind::Text),
                ],
                rows,
            }]
        } else {
            Vec::new()
        };
        Ok(TaxSection {
            key: "interest".into(),
            title: "Interest".into(),
            summary,
            tables,
            notes: vec![
                "Interest as credited to each account, and your share of it: a joint account's interest is taxed to each holder in proportion.".into(),
                "TFN withholding on interest isn't recorded in the app; check the bank's annual summary.".into(),
            ],
            text: String::new(),
        })
    }

    fn trusts(&mut self) -> (TaxSection, Vec<StatementRow>) {
        let fy = self.fy;
        let distributions: Vec<HoldingTransaction> = self.inputs.dividends_in(fy, IncomeKind::Trust).into_iter().cloned().collect();
        // (the user's share, the whole payment, how many) per symbol.
        let mut by_symbol: BTreeMap<String, (f64, f64, usize)> = BTreeMap::new();
        // (franked, franking credit) per symbol, from distributions that
        // carry franking — a broker's dividend summary, imported. Stands in
        // for a missing annual statement's 13C and 13Q.
        let mut franking_estimates: BTreeMap<String, (f64, f64)> = BTreeMap::new();
        let mut distribution_rows = Vec::new();
        for tx in &distributions {
            let info = self.inputs.symbols.get(&tx.symbol);
            let owned = self.inputs.share_of(&tx.symbol);
            let gross_full = r2(tx.amount.unwrap_or(0.0));
            let gross = r2(gross_full * owned);
            let native = dividend_native_gross(tx);
            let (withheld_native, _) = dividend_withheld(tx, info, native);
            let rate = if tx.currency == "AUD" { None } else if native > 0.0 { Some(gross_full / native) } else { tx.fx_rate };
            let entry = by_symbol.entry(tx.symbol.clone()).or_insert((0.0, 0.0, 0));
            entry.0 += gross;
            entry.1 += gross_full;
            entry.2 += 1;
            if let Some((franked, credit, _)) = tx.franked_amount.and(Self::franking(tx, info, gross_full)) {
                let estimate = franking_estimates.entry(tx.symbol.clone()).or_insert((0.0, 0.0));
                estimate.0 += r2(franked * owned);
                estimate.1 += r2(credit * owned);
            }
            distribution_rows.push(vec![
                text(share_label(&tx.symbol, owned)),
                text(tx.date.as_str()),
                self.inputs.paid(tx).map_or(serde_json::Value::Null, text),
                opt(tx.quantity.map(|q| q * owned), ColumnKind::Quantity),
                text(tx.currency.as_str()),
                num(native * owned, ColumnKind::Money),
                opt(rate, ColumnKind::Rate),
                num(gross, ColumnKind::Money),
                num(r2(withheld_native * rate.unwrap_or(1.0) * owned), ColumnKind::Money),
            ]);
        }
        let statements: BTreeMap<String, TaxStatement> = self
            .inputs
            .statements
            .iter()
            .filter(|s| s.fy == fy.0 && self.inputs.share_of(&s.symbol) > 0.0)
            .map(|s| (s.symbol.clone(), s.clone()))
            .collect();
        let symbols: BTreeSet<String> = by_symbol.keys().chain(statements.keys()).cloned().collect();

        let mut totals = TaxStatementFigures::default();
        let add = |total: &mut Option<f64>, value: Option<f64>| {
            if let Some(v) = value {
                *total = Some(total.unwrap_or(0.0) + v);
            }
        };
        let mut statement_rows = Vec::new();
        let mut panel = Vec::new();
        let mut missing = Vec::new();
        let mut estimated = Vec::new();
        let mut to_enter = Vec::new();
        let mut cash_without = 0.0;
        for symbol in &symbols {
            let (cash, cash_full, count) = by_symbol.get(symbol).copied().unwrap_or((0.0, 0.0, 0));
            let owned = self.inputs.share_of(symbol);
            // A joint holding's statement is for the whole holding; the
            // return takes the user's share of every figure on it.
            let statement = statements.get(symbol);
            let mut yours = statement.map(|s| scale_figures(&s.figures, owned));
            // No statement yet: the franking recorded on the distributions is
            // the best figure there is, and is shown as an estimate. The
            // statement replaces it the moment it is entered.
            let estimate = if statement.is_none() { franking_estimates.get(symbol).copied() } else { None };
            if let Some((franked, credit)) = estimate {
                add(&mut totals.franked_distributions, Some(franked));
                add(&mut totals.franking_credits, Some(credit));
                estimated.push(symbol.clone());
            }
            match &yours {
                Some(f) => {
                    add(&mut totals.non_primary_income, f.non_primary_income);
                    add(&mut totals.franked_distributions, f.franked_distributions);
                    add(&mut totals.franking_credits, f.franking_credits);
                    add(&mut totals.tfn_withheld, f.tfn_withheld);
                    add(&mut totals.total_capital_gains, f.total_capital_gains);
                    add(&mut totals.net_capital_gain, f.net_capital_gain);
                    add(&mut totals.assessable_foreign_income, f.assessable_foreign_income);
                    add(&mut totals.other_net_foreign_income, f.other_net_foreign_income);
                    add(&mut totals.foreign_tax_offset, f.foreign_tax_offset);
                }
                None => {
                    missing.push(symbol.clone());
                    cash_without += cash;
                    let mut row = vec![text(share_label(symbol, owned)), num(cash_full, ColumnKind::Money)];
                    row.extend(std::iter::repeat_n(serde_json::Value::Null, 11));
                    to_enter.push(row);
                }
            }
            if let Some((franked, credit)) = estimate {
                yours = Some(TaxStatementFigures { franked_distributions: Some(franked), franking_credits: Some(credit), ..Default::default() });
            }
            let f = yours.unwrap_or_default();
            statement_rows.push(vec![
                text(share_label(symbol, owned)),
                text(if statement.is_some() { "Entered" } else if estimate.is_some() { "Missing — franking estimated" } else { "Missing" }),
                num(cash, ColumnKind::Money),
                opt(f.non_primary_income, ColumnKind::Money),
                opt(f.franked_distributions, ColumnKind::Money),
                opt(f.franking_credits, ColumnKind::Money),
                opt(f.tfn_withheld, ColumnKind::Money),
                opt(f.total_capital_gains, ColumnKind::Money),
                opt(f.net_capital_gain, ColumnKind::Money),
                opt(f.assessable_foreign_income, ColumnKind::Money),
                opt(f.other_net_foreign_income, ColumnKind::Money),
                opt(f.foreign_tax_offset, ColumnKind::Money),
                opt(f.amit_increase, ColumnKind::Money),
                opt(f.amit_decrease, ColumnKind::Money),
            ]);
            panel.push(StatementRow {
                symbol: symbol.clone(),
                name: self.inputs.symbols.get(symbol).and_then(|s| s.name.clone()),
                cash_aud: r2(cash_full),
                distributions: count,
                statement: statement.cloned(),
            });
        }
        if !to_enter.is_empty() {
            let mut columns = vec![col("Symbol", ColumnKind::Text), col("Cash paid (AUD)", ColumnKind::Money)];
            for label in ["13U", "13C", "13Q", "13R", "18H", "18A", "20E", "20M", "20O", "AMIT increase", "AMIT decrease"] {
                columns.push(col(label, ColumnKind::Entry));
            }
            self.worksheet.push(TaxTable {
                title: "Annual tax statements: Part A figures, as printed on each statement".into(),
                columns,
                rows: to_enter,
            });
        }
        if !missing.is_empty() {
            let estimate_note = if estimated.is_empty() {
                String::new()
            } else {
                format!(" Until then 13C and 13Q use the franking recorded on the distributions of {}, as an estimate.", estimated.join(", "))
            };
            self.warn("trusts", format!(
                "No {} annual tax statement entered for {}. Their distributions are listed as cash received, which is not what goes on the return.{}",
                fy.label(),
                missing.join(", "),
                estimate_note
            ));
        }
        self.totals.trusts = Some(totals.clone());
        self.totals.trusts_estimated = estimated.clone();

        let estimated_note = (!estimated.is_empty()).then(|| format!("includes estimates for {}", estimated.join(", ")));
        let maybe_noted = |l: TaxSummaryLine| match &estimated_note {
            Some(note) => noted(l, note.clone()),
            None => l,
        };
        let mut summary = vec![
            line(Some("13U"), "Non-primary production income", totals.non_primary_income.unwrap_or(0.0)),
            maybe_noted(line(Some("13C"), "Franked distributions from trusts", totals.franked_distributions.unwrap_or(0.0))),
            maybe_noted(line(Some("13Q"), "Share of franking credits", totals.franking_credits.unwrap_or(0.0))),
            line(Some("13R"), "Share of credit for TFN amounts withheld", totals.tfn_withheld.unwrap_or(0.0)),
            noted(line(Some("18H"), "Capital gains from trusts (in the Capital gains section)", totals.total_capital_gains.unwrap_or(0.0)), "already netted there"),
            noted(line(Some("20E"), "Foreign source income from trusts", totals.assessable_foreign_income.unwrap_or(0.0)), "add to item 20"),
            noted(line(Some("20O"), "Foreign income tax offset from trusts", totals.foreign_tax_offset.unwrap_or(0.0)), "add to item 20"),
        ];
        if cash_without > 0.0 {
            summary.push(noted(line(None, "Distributions with no statement entered (cash)", cash_without), "not in the lines above"));
        }
        let tables = if self.options.full {
            vec![
                TaxTable {
                    title: "Annual tax statements".into(),
                    columns: vec![
                        col("Symbol", ColumnKind::Text), col("Statement", ColumnKind::Text), col("Cash received (AUD)", ColumnKind::Money),
                        col("13U", ColumnKind::Money), col("13C", ColumnKind::Money), col("13Q", ColumnKind::Money),
                        col("13R", ColumnKind::Money), col("18H", ColumnKind::Money), col("18A", ColumnKind::Money),
                        col("20E", ColumnKind::Money), col("20M", ColumnKind::Money), col("20O", ColumnKind::Money),
                        col("AMIT increase", ColumnKind::Money), col("AMIT decrease", ColumnKind::Money),
                    ],
                    rows: statement_rows,
                },
                TaxTable {
                    title: "Distributions".into(),
                    columns: vec![
                        col("Symbol", ColumnKind::Text), col("Ex-date", ColumnKind::Date), col("Paid", ColumnKind::Date),
                        col("Units", ColumnKind::Quantity), col("Currency", ColumnKind::Text), col("Gross (trade ccy)", ColumnKind::Money),
                        col("FX rate", ColumnKind::Rate), col("Gross (AUD)", ColumnKind::Money), col("Tax withheld (AUD)", ColumnKind::Money),
                    ],
                    rows: distribution_rows,
                },
            ]
        } else {
            Vec::new()
        };
        let section = TaxSection {
            key: "trusts".into(),
            title: "Trust and ETF distributions".into(),
            summary,
            tables,
            notes: vec![
                "A trust's income is taxed in the year it was earned: a June-quarter distribution paid in July belongs to the year that ended on 30 June.".into(),
                "Enter each fund's annual tax statement (its Part A summary). Its AMIT cost base adjustment then flows into the capital gains on later sales.".into(),
            ],
            text: String::new(),
        };
        (section, panel)
    }

    fn deductions(&mut self) -> Result<TaxSection, String> {
        let fy = self.fy;
        let mut stmt = self
            .conn
            .prepare(
                "SELECT c.date, a.name, a.currency, c.kind, c.amount, c.notes, a.tax_share_pct
                   FROM cash_transactions c JOIN cash_accounts a ON a.id = c.account_id
                  WHERE c.date BETWEEN ?1 AND ?2
                    AND ((c.kind = 'interest' AND c.amount < 0) OR (c.kind = 'fee' AND c.holding_tx_id IS NULL))
                  ORDER BY c.date, c.id",
            )
            .map_err(|e| e.to_string())?;
        let found = stmt
            .query_map(params![fy.start().to_string(), fy.end().to_string()], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, f64>(4)?,
                    r.get::<_, Option<String>>(5)?,
                    r.get::<_, Option<f64>>(6)?,
                ))
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;

        let mut rows = Vec::new();
        let (mut interest_total, mut fee_total) = (0.0, 0.0);
        for (date, account, currency, kind, amount, notes, share_pct) in found {
            // A joint loan or account: only the user's share of the cost is theirs to claim.
            let owned = share_fraction(share_pct);
            if owned <= 0.0 {
                continue;
            }
            let rate = if currency == "AUD" { None } else { fx_rate_on(self.conn, &currency, &date) };
            let aud = r2(-amount * rate.unwrap_or(1.0));
            let yours = r2(aud * owned);
            let label = if kind == "interest" {
                interest_total += yours;
                "Interest charged"
            } else {
                fee_total += yours;
                "Fee"
            };
            rows.push(vec![
                text(date),
                text(account),
                text(label),
                text(currency),
                num(-amount, ColumnKind::Money),
                opt(rate, ColumnKind::Rate),
                num(aud, ColumnKind::Money),
                num(owned * 100.0, ColumnKind::Quantity),
                num(yours, ColumnKind::Money),
                notes.map_or(serde_json::Value::Null, text),
            ]);
        }
        self.totals.deductions = Some(r2(interest_total + fee_total));

        let summary = vec![
            line(None, "Interest charged on investment borrowings", interest_total),
            line(None, "Account and other fees", fee_total),
            noted(line(Some("D8"), "Possible dividend deductions", interest_total + fee_total), "check each one"),
        ];
        let tables = if self.options.full {
            vec![TaxTable {
                title: "Interest charged and fees".into(),
                columns: vec![
                    col("Date", ColumnKind::Date), col("Account", ColumnKind::Text), col("Type", ColumnKind::Text),
                    col("Currency", ColumnKind::Text), col("Amount (account ccy)", ColumnKind::Money), col("FX rate", ColumnKind::Rate),
                    col("Amount (AUD)", ColumnKind::Money), col("Your share %", ColumnKind::Quantity),
                    col("Yours (AUD)", ColumnKind::Money), col("Notes", ColumnKind::Text),
                ],
                rows,
            }]
        } else {
            Vec::new()
        };
        Ok(TaxSection {
            key: "deductions".into(),
            title: "Possible deductions".into(),
            summary,
            tables,
            notes: vec![
                "Interest on money borrowed to buy income-producing shares is usually deductible. Fees may or may not be — check each one.".into(),
                "Brokerage is not listed here: it is already in the cost base or off the sale proceeds.".into(),
            ],
            text: String::new(),
        })
    }

    fn planning(&mut self, capital: Option<CapitalGainsResult>) -> Option<TaxSection> {
        let prices = self.options.prices_aud.as_ref()?;
        let today = self.options.today;
        let mut parcels: Vec<(NaiveDate, Vec<serde_json::Value>)> = Vec::new();
        let (mut eligible_gain, mut pending_gain, mut soon_gain, mut losses) = (0.0, 0.0, 0.0, 0.0);
        let mut unpriced = BTreeSet::new();

        for (symbol, walk) in &self.inputs.walks {
            let owned = self.inputs.share_of(symbol);
            if owned <= 0.0 {
                continue;
            }
            for parcel in &walk.open {
                let Some(acquired) = rules::parse_date(&parcel.acquired) else { continue };
                let from = rules::discount_eligible_from(acquired);
                let quantity = parcel.quantity * owned;
                let cost = r2(quantity * parcel.cost_per_share);
                let price = prices.get(symbol).copied();
                if price.is_none() {
                    unpriced.insert(symbol.clone());
                }
                let value = price.map(|p| r2(quantity * p));
                let gain = value.map(|v| r2(v - cost));
                let days_to_go = (from - today).num_days().max(0);
                if let Some(g) = gain {
                    if g < 0.0 {
                        losses += -g;
                    } else if days_to_go == 0 {
                        eligible_gain += g;
                    } else {
                        pending_gain += g;
                        if days_to_go <= 90 {
                            soon_gain += g;
                        }
                    }
                }
                parcels.push((from, vec![
                    text(share_label(symbol, owned)),
                    text(parcel.acquired.as_str()),
                    num(quantity, ColumnKind::Quantity),
                    text(self.inputs.symbols.get(symbol).map_or("AUD", |s| s.currency.as_str())),
                    num(cost, ColumnKind::Money),
                    opt(price, ColumnKind::Price),
                    opt(value, ColumnKind::Money),
                    opt(gain, ColumnKind::Money),
                    text(from.to_string()),
                    num(days_to_go as f64, ColumnKind::Integer),
                ]));
            }
        }
        // The parcels still waiting for the discount first, soonest first:
        // those are the ones where the timing of a sale matters.
        parcels.sort_by(|a, b| (a.0 <= today).cmp(&(b.0 <= today)).then(a.0.cmp(&b.0)));
        if !unpriced.is_empty() {
            self.info("planning", format!("No current price for {}, so their unrealised gains are left out.", unpriced.into_iter().collect::<Vec<_>>().join(", ")));
        }

        let mut summary = Vec::new();
        if let Some(result) = capital {
            summary.push(line(None, "Net capital gain realised so far this year", result.net_capital_gain));
        }
        summary.push(line(None, "Unrealised gains — discount already applies", eligible_gain));
        summary.push(line(None, "Unrealised gains — held under 12 months", pending_gain));
        summary.push(line(None, "  of which qualify for the discount within 90 days", soon_gain));
        summary.push(line(None, "Unrealised losses", losses));

        let tables = if self.options.full {
            vec![TaxTable {
                title: "Shares held, by parcel".into(),
                columns: vec![
                    col("Symbol", ColumnKind::Text), col("Acquired", ColumnKind::Date), col("Quantity", ColumnKind::Quantity),
                    col("Currency", ColumnKind::Text), col("Cost base (AUD)", ColumnKind::Money), col("Price (AUD)", ColumnKind::Price),
                    col("Value (AUD)", ColumnKind::Money), col("Unrealised gain (AUD)", ColumnKind::Money),
                    col("Discount from", ColumnKind::Date), col("Days to go", ColumnKind::Integer),
                ],
                rows: parcels.into_iter().map(|(_, row)| row).collect(),
            }]
        } else {
            Vec::new()
        };
        Some(TaxSection {
            key: "planning".into(),
            title: "This year so far".into(),
            summary,
            tables,
            notes: vec!["Valued at the latest prices, the same as the Holdings screen. A parcel qualifies for the discount from the day after its first anniversary.".into()],
            text: String::new(),
        })
    }

    /// Fetched dividend events this year with no transaction recorded — most
    /// likely income missing from the report.
    fn unrecorded_dividends(&mut self) -> Result<(), String> {
        let floor = dividend_record_floor(self.conn);
        let mut stmt = self
            .conn
            .prepare(
                "SELECT e.symbol, e.ex_date, e.payment_date, e.amount FROM dividend_events e
                  WHERE e.ex_date >= ?1
                    AND NOT EXISTS (SELECT 1 FROM holdings_transactions h
                                     WHERE h.symbol = e.symbol AND (h.date = e.ex_date OR h.source_ex_date = e.ex_date)
                                       AND h.transaction_type = 'dividend')
                    AND NOT EXISTS (SELECT 1 FROM dividend_exclusions x WHERE x.symbol = e.symbol AND x.ex_date = e.ex_date)
                  ORDER BY e.ex_date",
            )
            .map_err(|e| e.to_string())?;
        let events = stmt
            .query_map(params![floor], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<String>>(2)?, r.get::<_, f64>(3)?)))
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;

        let groups: HashMap<String, Vec<PortfolioTx>> = portfolio::group_by_symbol(&to_portfolio_txs(&self.inputs.txs)).into_iter().collect();
        let mut missing = Vec::new();
        let mut to_enter = Vec::new();
        for (symbol, ex_date, paid, per_share) in events {
            let Some(group) = groups.get(&symbol) else { continue };
            let owned = self.inputs.share_of(&symbol);
            let shares = portfolio::shares_on_date(group, &ex_date);
            if shares <= 0.0 || owned <= 0.0 {
                continue;
            }
            let Some(ex) = rules::parse_date(&ex_date) else { continue };
            let year = match self.inputs.kind_of(&symbol) {
                IncomeKind::Trust => rules::distribution_tax_year(ex),
                _ => TaxYear::containing(paid.as_deref().and_then(rules::parse_date).unwrap_or(ex)),
            };
            if year == self.fy {
                missing.push(format!("{} ex {} ({} × {})", symbol, ex_date, trim_number(shares), trim_number(per_share)));
                to_enter.push(vec![
                    text(share_label(&symbol, owned)),
                    text(ex_date.as_str()),
                    num(shares, ColumnKind::Quantity),
                    num(per_share, ColumnKind::Price),
                    text(self.inputs.symbols.get(&symbol).map_or("AUD", |s| s.currency.as_str())),
                    serde_json::Value::Null,
                    serde_json::Value::Null,
                ]);
            }
        }
        if !to_enter.is_empty() {
            self.worksheet.push(TaxTable {
                title: "Dividends found in the fetched data but not recorded".into(),
                columns: vec![
                    col("Symbol", ColumnKind::Text), col("Ex-date", ColumnKind::Date), col("Shares held", ColumnKind::Quantity),
                    col("Per share", ColumnKind::Price), col("Currency", ColumnKind::Text),
                    col("Received? (Y/N)", ColumnKind::Entry), col("Amount received", ColumnKind::Entry),
                ],
                rows: to_enter,
            });
        }
        if !missing.is_empty() {
            self.warn("dividends", format!(
                "Dividends in the fetched data with no transaction recorded, so not in this report: {}. Record them on the Transactions screen, or delete them there if they weren't received.",
                missing.join("; ")
            ));
        }
        Ok(())
    }

    fn adjustments(&mut self) -> Result<(), String> {
        let fy = self.fy;
        let mut stmt = self
            .conn
            .prepare(
                "SELECT c.date, a.name, a.currency, c.amount, c.notes FROM cash_transactions c
                   JOIN cash_accounts a ON a.id = c.account_id
                  WHERE c.kind = 'adjustment' AND c.date BETWEEN ?1 AND ?2 ORDER BY c.date",
            )
            .map_err(|e| e.to_string())?;
        let found = stmt
            .query_map(params![fy.start().to_string(), fy.end().to_string()], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, f64>(3)?, r.get::<_, Option<String>>(4)?))
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        if !found.is_empty() {
            let list: Vec<String> = found
                .iter()
                .map(|(date, account, currency, amount, notes)| {
                    format!("{} {} {} {:.2}{}", date, account, currency, amount, notes.as_deref().map(|n| format!(" \"{}\"", n)).unwrap_or_default())
                })
                .collect();
            self.warn("checks", format!(
                "Manual balance adjustments this year — make sure none of them stands in for income or a trade that should be recorded: {}.",
                list.join("; ")
            ));
        }
        Ok(())
    }

    fn guessed_kinds(&mut self, income_symbols: &BTreeSet<String>) {
        let guessed_symbols: Vec<(&String, &SymbolTax)> = income_symbols
            .iter()
            .filter_map(|s| self.inputs.symbols.get(s).filter(|i| i.guessed && i.share() > 0.0).map(|i| (s, i)))
            .collect();
        let guessed: Vec<String> = guessed_symbols.iter().map(|(s, i)| format!("{} ({})", s, i.kind.label())).collect();
        if !guessed_symbols.is_empty() {
            self.worksheet.push(TaxTable {
                title: "Income types to confirm: company, trust or foreign".into(),
                columns: vec![
                    col("Symbol", ColumnKind::Text), col("Name", ColumnKind::Text),
                    col("Worked out as", ColumnKind::Text), col("Correct type", ColumnKind::Entry),
                ],
                rows: guessed_symbols
                    .iter()
                    .map(|(s, i)| vec![
                        text(s.as_str()),
                        i.name.clone().map_or(serde_json::Value::Null, text),
                        text(i.kind.label()),
                        serde_json::Value::Null,
                    ])
                    .collect(),
            });
        }
        if !guessed.is_empty() {
            self.info("checks", format!(
                "Income type worked out rather than set for: {}. Confirm them under Symbol tax settings — a stapled security or a US-domiciled fund is easily misread.",
                guessed.join(", ")
            ));
        }
    }

    fn unapplied_adjustments(&mut self) {
        let mut notes = Vec::new();
        for (symbol, walk) in &self.inputs.walks {
            if self.inputs.share_of(symbol) <= 0.0 {
                continue;
            }
            for (date, amount) in &walk.unapplied {
                notes.push(format!("{} {} ({:.2})", symbol, date, amount));
            }
        }
        if !notes.is_empty() {
            self.warn("trusts", format!(
                "AMIT adjustments that couldn't be applied in full — no units held at 30 June, or a decrease larger than the cost base (that excess is itself a capital gain): {}.",
                notes.join("; ")
            ));
        }
    }

    /// What changed since this year's report was last exported.
    fn changes_since(&mut self, exported_at: &str, income_symbols: &BTreeSet<String>) -> Result<(), String> {
        let fy = self.fy;
        let in_year = |date: Option<&str>| date.and_then(rules::parse_date).is_some_and(|d| fy.contains(d));
        let mut stmt = self
            .conn
            .prepare(
                "SELECT timestamp, table_name, action, old_values, new_values FROM audit_log
                  WHERE timestamp > ?1
                    AND table_name IN ('holdings_transactions', 'cash_transactions', 'cash_accounts', 'symbol_info', 'tax_statements', 'dividend_exclusions')
                  ORDER BY timestamp",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![exported_at], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, Option<String>>(3)?, r.get::<_, Option<String>>(4)?))
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;

        let mut changes = Vec::new();
        for (timestamp, table, action, old, new) in rows {
            let old: serde_json::Value = old.as_deref().and_then(|v| serde_json::from_str(v).ok()).unwrap_or_default();
            let new: serde_json::Value = new.as_deref().and_then(|v| serde_json::from_str(v).ok()).unwrap_or_default();
            let row = if new.is_null() { &old } else { &new };
            let field = |name: &str| row.get(name).and_then(|v| v.as_str()).map(str::to_string);
            let verb = match action.as_str() {
                "INSERT" => "added",
                "DELETE" => "deleted",
                _ => "edited",
            };
            let symbol = field("symbol").unwrap_or_default();
            let description = match table.as_str() {
                "holdings_transactions" => {
                    let kind = field("transaction_type").unwrap_or_default();
                    let dates = [old.get("date").and_then(|v| v.as_str()), new.get("date").and_then(|v| v.as_str())];
                    let relevant = self.inputs.share_of(&symbol) > 0.0
                        && (dates.iter().any(|d| in_year(*d))
                            || (kind == "purchase" && self.sold_symbols.contains(&symbol) && dates.iter().flatten().any(|d| *d <= fy.end().to_string().as_str())));
                    relevant.then(|| format!("{} {} {} dated {}", symbol, kind, verb, field("date").unwrap_or_default()))
                }
                "cash_transactions" => {
                    let kind = field("kind").unwrap_or_default();
                    let own = row.get("holding_tx_id").is_none_or(|v| v.is_null());
                    (own && matches!(kind.as_str(), "interest" | "fee" | "adjustment") && in_year(field("date").as_deref()))
                        .then(|| format!("cash {} {} dated {}", kind, verb, field("date").unwrap_or_default()))
                }
                "symbol_info" => {
                    let tax_fields = ["dividend_withholding_pct", "franking_pct", "income_kind", "tax_share_pct"];
                    let changed = action != "UPDATE" || tax_fields.iter().any(|f| old.get(f) != new.get(f));
                    (changed && action == "UPDATE" && (income_symbols.contains(&symbol) || self.sold_symbols.contains(&symbol)))
                        .then(|| format!("{} tax settings changed", symbol))
                }
                "tax_statements" => {
                    let year = row.get("fy").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
                    (year == fy.0 || (self.sold_symbols.contains(&symbol) && year < fy.0))
                        .then(|| format!("{} {} annual tax statement {}", symbol, TaxYear(year).label(), verb))
                }
                "cash_accounts" => {
                    (action == "UPDATE" && old.get("tax_share_pct") != new.get("tax_share_pct"))
                        .then(|| format!("{} account: your share changed", field("name").unwrap_or_default()))
                }
                "dividend_exclusions" => {
                    (income_symbols.contains(&symbol) && in_year(field("ex_date").as_deref()))
                        .then(|| format!("{} dividend ex {} declined", symbol, field("ex_date").unwrap_or_default()))
                }
                _ => None,
            };
            if let Some(description) = description {
                changes.push(format!("{} ({})", description, timestamp.get(..10).unwrap_or(&timestamp)));
            }
        }
        if !changes.is_empty() {
            let total = changes.len();
            let mut shown: Vec<String> = changes.into_iter().take(15).collect();
            if total > shown.len() {
                shown.push(format!("and {} more", total - 15));
            }
            self.warn("checks", format!(
                "{} change{} affecting this year since its report was last exported on {}: {}.",
                total,
                if total == 1 { "" } else { "s" },
                exported_at.get(..10).unwrap_or(exported_at),
                shown.join("; ")
            ));
        }
        Ok(())
    }
}

/// Return labels the app has no record of, for writing in by hand.
fn not_in_app(options: &ReportOptions, carried_in: f64, carried_source: &str) -> Option<TaxTable> {
    let include = |key: &str| options.sections.contains(key);
    let mut rows = Vec::new();
    if include("interest") {
        rows.push(vec![
            text("10M"),
            text("TFN amounts withheld from interest (from each bank's annual summary)"),
            serde_json::Value::Null,
            serde_json::Value::Null,
        ]);
    }
    if include("cgt") && carried_source == "computed" {
        rows.push(vec![
            text("18V"),
            text("Net capital losses carried forward, from last year's return"),
            num(carried_in, ColumnKind::Money),
            serde_json::Value::Null,
        ]);
    }
    if include("deductions") {
        rows.push(vec![
            text("D8"),
            text("Other investment costs paid outside the app (advice fees, subscriptions)"),
            serde_json::Value::Null,
            serde_json::Value::Null,
        ]);
    }
    (!rows.is_empty()).then(|| TaxTable {
        title: "Figures the app doesn't hold".into(),
        columns: vec![
            col("Label", ColumnKind::Text), col("What", ColumnKind::Text),
            col("App's figure", ColumnKind::Money), col("Your figure", ColumnKind::Entry),
        ],
        rows,
    })
}

/// The figures by return label, from whichever sections were included.
fn label_summary(totals: &Totals) -> TaxSection {
    let mut summary = Vec::new();
    if let Some(interest) = totals.interest {
        summary.push(line(Some("10L"), "Gross interest", interest));
    }
    if let Some((unfranked, franked, credit, tfn)) = totals.dividends {
        summary.push(line(Some("11S"), "Unfranked amount", unfranked));
        summary.push(line(Some("11T"), "Franked amount", franked));
        summary.push(line(Some("11U"), "Franking credit", credit));
        summary.push(line(Some("11V"), "TFN amounts withheld from dividends", tfn));
    }
    if let Some(t) = &totals.trusts {
        summary.push(line(Some("13U"), "Share of net income from trusts (non-primary production)", t.non_primary_income.unwrap_or(0.0)));
        let estimated = |l: TaxSummaryLine| {
            if totals.trusts_estimated.is_empty() {
                l
            } else {
                noted(l, format!("includes estimates for {}", totals.trusts_estimated.join(", ")))
            }
        };
        summary.push(estimated(line(Some("13C"), "Franked distributions from trusts", t.franked_distributions.unwrap_or(0.0))));
        summary.push(estimated(line(Some("13Q"), "Share of franking credits from franked dividends", t.franking_credits.unwrap_or(0.0))));
        summary.push(line(Some("13R"), "Share of credit for TFN amounts withheld", t.tfn_withheld.unwrap_or(0.0)));
    }
    if let Some((event, result)) = &totals.capital {
        summary.push(TaxSummaryLine { item: Some("18G".into()), label: "CGT event during the year".into(), amount: None, value: Some(if *event { "Yes" } else { "No" }.into()), note: None });
        summary.push(line(Some("18H"), "Total current year capital gains", result.total_gains));
        summary.push(line(Some("18A"), "Net capital gain", result.net_capital_gain));
        summary.push(line(Some("18V"), "Net capital losses carried forward to later income years", result.carried_out));
    }
    let trust_foreign = totals.trusts.as_ref().map(|t| {
        (
            t.assessable_foreign_income.unwrap_or(0.0),
            t.other_net_foreign_income.unwrap_or(0.0),
            t.foreign_tax_offset.unwrap_or(0.0),
        )
    });
    if totals.foreign.is_some() || trust_foreign.is_some() {
        let (gross, withheld) = totals.foreign.unwrap_or((0.0, 0.0));
        let (trust_e, trust_m, trust_o) = trust_foreign.unwrap_or((0.0, 0.0, 0.0));
        summary.push(line(Some("20E"), "Assessable foreign source income", gross + trust_e));
        summary.push(line(Some("20M"), "Other net foreign source income", gross + trust_m));
        summary.push(line(Some("20O"), "Foreign income tax offset", withheld + trust_o));
    }
    if let Some(deductions) = totals.deductions {
        summary.push(noted(line(Some("D8"), "Dividend deductions (possible)", deductions), "check each one"));
    }
    TaxSection {
        key: "summary".into(),
        title: "Summary by return label".into(),
        summary,
        tables: Vec::new(),
        notes: vec!["Labels are the individual return's (myTax); check them against this year's form.".into()],
        text: String::new(),
    }
}

/// Build the whole report.
pub(crate) fn build_tax_report(db_path: &Path, options: &ReportOptions) -> Result<TaxReport, String> {
    let conn = open_db(db_path).map_err(|e| e.to_string())?;
    let inputs = load_inputs(db_path, &conn)?;
    let fy = options.fy;
    let include = |key: &str| options.sections.contains(key);
    let in_progress = fy.contains(options.today);

    let years = capital_by_year(&inputs);
    let (carried_in, carried_source) = match options.losses_carried_in {
        Some(v) => (v.max(0.0), "entered"),
        None => (losses_carried_into(&years, fy), "computed"),
    };

    // Every symbol with something in this year, whoever it belongs to: the
    // settings panel lists them all, so a holding left out can be put back.
    let all_sold: BTreeSet<String> = inputs
        .walks
        .iter()
        .filter(|(_, w)| w.sales.iter().any(|s| rules::parse_date(&s.date).is_some_and(|d| fy.contains(d))))
        .map(|(s, _)| s.clone())
        .collect();
    let income_symbols: BTreeSet<String> = inputs
        .txs
        .iter()
        .filter(|t| t.transaction_type == "dividend" && inputs.income_year(t) == Some(fy))
        .map(|t| t.symbol.clone())
        .chain(inputs.statements.iter().filter(|s| s.fy == fy.0).map(|s| s.symbol.clone()))
        .collect();
    let held: BTreeSet<String> = if in_progress {
        inputs.walks.iter().filter(|(_, w)| !w.open.is_empty()).map(|(s, _)| s.clone()).collect()
    } else {
        BTreeSet::new()
    };
    let activity: BTreeSet<String> = all_sold.iter().chain(&income_symbols).chain(&held).cloned().collect();
    let sold_symbols: BTreeSet<String> = all_sold.iter().filter(|s| inputs.share_of(s) > 0.0).cloned().collect();

    let mut builder = Builder {
        inputs: &inputs,
        conn: &conn,
        options,
        fy,
        checks: Vec::new(),
        totals: Totals::default(),
        sold_symbols,
        worksheet: Vec::new(),
    };

    if in_progress {
        builder.info("checks", format!("The {} income year is still in progress: figures are to {}.", fy.label(), options.today));
    }

    let mut sections = Vec::new();
    // Capital gains are always worked out — the planning section and the
    // label summary need them — but only shown when asked for.
    let capital = builder.capital_gains(&years, carried_in, carried_source);
    let capital_result = builder.totals.capital.map(|(_, r)| r);
    if include("cgt") {
        sections.push(capital);
    } else {
        builder.totals.capital = None;
        builder.checks.retain(|c| c.section != "cgt");
    }
    if include("dividends") {
        sections.push(builder.dividends());
    }
    if include("foreign") {
        sections.push(builder.foreign());
    }
    if include("interest") {
        sections.push(builder.interest()?);
    }
    let mut statements = Vec::new();
    if include("trusts") {
        let (section, panel) = builder.trusts();
        sections.push(section);
        statements = panel;
        builder.unapplied_adjustments();
    }
    if include("deductions") {
        sections.push(builder.deductions()?);
    }
    if include("planning") && in_progress
        && let Some(section) = builder.planning(capital_result)
    {
        sections.push(section);
    }

    let last_exported_at: Option<String> = conn
        .query_row("SELECT value FROM app_config WHERE key = ?1", params![export_key(fy)], |r| r.get(0))
        .optional()
        .map_err(|e| e.to_string())?;
    if include("checks") {
        if include("dividends") || include("foreign") || include("trusts") {
            builder.unrecorded_dividends()?;
        }
        builder.adjustments()?;
        builder.guessed_kinds(&income_symbols);
        if let Some(exported_at) = &last_exported_at {
            builder.changes_since(exported_at, &activity)?;
        }
    }
    let mut worksheet = std::mem::take(&mut builder.worksheet);
    worksheet.extend(not_in_app(options, carried_in, carried_source));
    let accounts = account_rows(&conn, fy)?;
    let ownership = ownership_lines(&inputs, &activity, &accounts);

    let summary = label_summary(&builder.totals);
    sections.insert(0, summary);
    for section in &mut sections {
        section.text = section_text(section, options.full);
    }

    let mut checks = if include("checks") { builder.checks } else { Vec::new() };
    // Warnings first, each group in section order.
    checks.sort_by_key(|c| (c.severity != "warn", TAX_SECTIONS.iter().position(|s| *s == c.section).unwrap_or(0)));

    let symbols: Vec<SymbolTaxRow> = activity
        .iter()
        .filter_map(|s| {
            inputs.symbols.get(s).map(|info| SymbolTaxRow {
                symbol: s.clone(),
                name: info.name.clone(),
                currency: info.currency.clone(),
                income_kind: info.kind.as_str(),
                income_kind_set: !info.guessed,
                franking_pct: info.franking_pct,
                withholding_pct: info.withholding_pct,
                share_pct: info.share_pct,
            })
        })
        .collect();

    let mut included: Vec<String> = TAX_SECTIONS.iter().filter(|s| include(s)).map(|s| s.to_string()).collect();
    if !in_progress {
        included.retain(|s| s != "planning");
    }

    let mut report = TaxReport {
        fy: fy.0,
        label: fy.label(),
        start: fy.start().to_string(),
        end: fy.end().to_string(),
        entity: ENTITY.as_str(),
        generated_at: chrono::Local::now().format("%Y-%m-%d %H:%M").to_string(),
        in_progress,
        detail: if options.full { "full" } else { "summary" },
        last_exported_at,
        losses_carried_in: r2(carried_in),
        losses_carried_in_source: carried_source,
        included,
        checks,
        sections,
        symbols,
        statements,
        accounts,
        ownership,
        worksheet,
        text: String::new(),
    };
    report.text = report_text(&report);
    Ok(report)
}

/// Cash accounts that matter this year: any with interest or fees in it,
/// and any already set to less than all the user's.
fn account_rows(conn: &Connection, fy: TaxYear) -> Result<Vec<AccountTaxRow>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT a.id, a.name, a.currency, a.tax_share_pct,
                    COALESCE(SUM(CASE WHEN c.kind = 'interest' AND c.amount > 0 THEN c.amount END), 0),
                    COUNT(c.id)
               FROM cash_accounts a
               LEFT JOIN cash_transactions c ON c.account_id = a.id AND c.date BETWEEN ?1 AND ?2
                    AND (c.kind = 'interest' OR (c.kind = 'fee' AND c.holding_tx_id IS NULL))
              GROUP BY a.id
              ORDER BY a.name",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![fy.start().to_string(), fy.end().to_string()], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, Option<f64>>(3)?, r.get::<_, f64>(4)?, r.get::<_, i64>(5)?))
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    Ok(rows
        .into_iter()
        .filter(|(_, _, _, share, _, count)| *count > 0 || share.is_some())
        .map(|(id, name, currency, share_pct, interest, _)| {
            // Native interest in AUD at the year-end rate: a guide for the
            // panel, not a reported figure.
            let rate = if currency == "AUD" { 1.0 } else { fx_rate_on(conn, &currency, &fy.end().to_string()).unwrap_or(1.0) };
            AccountTaxRow { id, name, currency, share_pct, interest_aud: r2(interest * rate) }
        })
        .collect())
}

/// One line per thing left out or counted only in part, for the top of the
/// report — an accountant reading it needs to know what isn't there.
fn ownership_lines(inputs: &Inputs, activity: &BTreeSet<String>, accounts: &[AccountTaxRow]) -> Vec<String> {
    let mut lines = Vec::new();
    let excluded: Vec<&str> = activity.iter().filter(|s| inputs.share_of(s) <= 0.0).map(String::as_str).collect();
    if !excluded.is_empty() {
        lines.push(format!("Left out, as not yours (share 0%): {}.", excluded.join(", ")));
    }
    let partial: Vec<String> = activity
        .iter()
        .filter(|s| (0.0..1.0).contains(&inputs.share_of(s)) && inputs.share_of(s) > 0.0)
        .map(|s| format!("{} {}%", s, trim_number(inputs.share_of(s) * 100.0)))
        .collect();
    if !partial.is_empty() {
        lines.push(format!("Holdings reported at your share: {}.", partial.join(", ")));
    }
    let accounts: Vec<String> = accounts
        .iter()
        .filter_map(|a| a.share_pct.map(|p| format!("{} {}%", a.name, trim_number(p))))
        .collect();
    if !accounts.is_empty() {
        lines.push(format!("Cash accounts reported at your share: {}.", accounts.join(", ")));
    }
    lines
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

fn group_thousands(integer: &str) -> String {
    let mut out = String::new();
    for (i, ch) in integer.chars().enumerate() {
        if i > 0 && (integer.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// A money figure for reading: thousands separated, two decimals.
fn money_text(value: f64) -> String {
    let formatted = format!("{:.2}", r2(value).abs());
    let (integer, fraction) = formatted.split_once('.').unwrap_or((&formatted, "00"));
    let sign = if r2(value) < 0.0 { "-" } else { "" };
    format!("{}{}.{}", sign, group_thousands(integer), fraction)
}

/// A cell as text. `grouped` adds thousands separators to money, for reading;
/// the CSV leaves them out so a spreadsheet reads the number.
fn cell_text(value: &serde_json::Value, kind: ColumnKind, grouped: bool) -> String {
    if kind == ColumnKind::Entry {
        // A line to write on when read or printed; an empty cell to type in
        // when opened in a spreadsheet.
        return if grouped { "__________".to_string() } else { String::new() };
    }
    let Some(n) = value.as_f64() else {
        return value.as_str().unwrap_or("").to_string();
    };
    match kind {
        ColumnKind::Money if grouped => money_text(n),
        ColumnKind::Money => format!("{:.2}", r2(n)),
        ColumnKind::Rate => format!("{:.4}", n),
        ColumnKind::Integer => format!("{:.0}", n),
        ColumnKind::Price => {
            let s = format!("{:.4}", n);
            let trimmed = s.trim_end_matches('0');
            let decimals = trimmed.split_once('.').map_or(0, |(_, f)| f.len());
            if decimals < 2 { format!("{:.2}", n) } else { trimmed.to_string() }
        }
        ColumnKind::Quantity => trim_number(n),
        ColumnKind::Text | ColumnKind::Date | ColumnKind::Entry => trim_number(n),
    }
}

fn summary_value(line: &TaxSummaryLine) -> String {
    line.value.clone().or_else(|| line.amount.map(money_text)).unwrap_or_default()
}

fn table_text(table: &TaxTable) -> String {
    let cells: Vec<Vec<String>> = table
        .rows
        .iter()
        .map(|row| row.iter().zip(&table.columns).map(|(v, c)| cell_text(v, c.kind, true)).collect())
        .collect();
    let widths: Vec<usize> = table
        .columns
        .iter()
        .enumerate()
        .map(|(i, c)| cells.iter().map(|r| r[i].chars().count()).chain([c.label.chars().count()]).max().unwrap_or(0))
        .collect();
    let numeric = |kind: ColumnKind| !matches!(kind, ColumnKind::Text | ColumnKind::Date);
    let render = |values: Vec<&str>| -> String {
        let parts: Vec<String> = values
            .iter()
            .zip(&table.columns)
            .zip(&widths)
            .map(|((v, c), w)| if numeric(c.kind) { format!("{:>w$}", v, w = *w) } else { format!("{:<w$}", v, w = *w) })
            .collect();
        format!("  {}", parts.join("  ").trim_end())
    };
    let mut out = format!("  {}\n", table.title);
    out.push_str(&render(table.columns.iter().map(|c| c.label.as_str()).collect()));
    out.push('\n');
    if cells.is_empty() {
        out.push_str("  (none)\n");
    }
    for row in &cells {
        out.push_str(&render(row.iter().map(String::as_str).collect()));
        out.push('\n');
    }
    out
}

fn section_text(section: &TaxSection, full: bool) -> String {
    let mut out = format!("{}\n", section.title.to_uppercase());
    for line in &section.summary {
        let item = line.item.as_deref().unwrap_or("");
        let label = &line.label;
        let dots = 62usize.saturating_sub(label.chars().count() + 1);
        let _ = write!(out, "  {:<4} {} {} {:>14}", item, label, ".".repeat(dots.max(2)), summary_value(line));
        if let Some(note) = &line.note {
            let _ = write!(out, "  ({})", note);
        }
        out.push('\n');
    }
    if full {
        for table in &section.tables {
            out.push('\n');
            out.push_str(&table_text(table));
        }
    }
    if !section.notes.is_empty() {
        out.push('\n');
        for note in &section.notes {
            let _ = writeln!(out, "  Note: {}", note);
        }
    }
    out
}

fn report_text(report: &TaxReport) -> String {
    let fy = TaxYear(report.fy);
    let mut out = format!(
        "TAX REPORT — {} income year ({} to {})\n{} · amounts in AUD unless a column says otherwise · generated {}\n",
        report.label,
        fy.start().format("%-d %b %Y"),
        fy.end().format("%-d %b %Y"),
        ENTITY.label(),
        report.generated_at,
    );
    out.push_str("Worked out from the records in the Stocks app. Check every figure before lodging.\n");
    for line in &report.ownership {
        let _ = writeln!(out, "{}", line);
    }
    if !report.checks.is_empty() {
        out.push_str("\nNEEDS ATTENTION\n");
        for check in &report.checks {
            let _ = writeln!(out, "  {} {}", if check.severity == "warn" { "!" } else { "·" }, check.message);
        }
    }
    for section in &report.sections {
        out.push('\n');
        out.push_str(&section.text);
    }
    if !report.worksheet.is_empty() {
        out.push_str("\nTO COMPLETE BY HAND\n");
        out.push_str("  What the app doesn't know yet. Fill it in here, or enter it in the app and generate again.\n");
        for table in &report.worksheet {
            out.push('\n');
            out.push_str(&table_text(table));
        }
    }
    out
}

/// The report as CSV: one block per section, a blank line between them. A
/// spreadsheet opens it with every table in columns.
pub(crate) fn report_csv(report: &TaxReport) -> String {
    let fy = TaxYear(report.fy);
    let row = |cells: Vec<String>| -> String { cells.iter().map(|c| csv_field(c)).collect::<Vec<_>>().join(",") + "\n" };
    // Excel only reads a CSV as UTF-8 when it starts with a byte-order mark;
    // without one the dashes and dots in the labels come out garbled.
    let mut out = String::from("\u{feff}");
    out.push_str(&row(vec!["Tax report".into(), format!("{} income year", fy.slug().trim_start_matches("FY")), fy.start().to_string(), fy.end().to_string()]));
    out.push_str(&row(vec!["Return".into(), ENTITY.label().into()]));
    out.push_str(&row(vec!["Generated".into(), report.generated_at.clone()]));
    out.push_str(&row(vec!["Amounts".into(), "AUD unless a column says otherwise".into()]));
    for line in &report.ownership {
        out.push_str(&row(vec!["Ownership".into(), line.clone()]));
    }

    if !report.checks.is_empty() {
        out.push('\n');
        out.push_str(&row(vec!["Needs attention".into()]));
        out.push_str(&row(vec!["Severity".into(), "Section".into(), "Message".into()]));
        for check in &report.checks {
            out.push_str(&row(vec![check.severity.into(), check.section.into(), check.message.clone()]));
        }
    }
    for section in &report.sections {
        out.push('\n');
        out.push_str(&row(vec![section.title.clone()]));
        out.push_str(&row(vec!["Label".into(), "Description".into(), "Amount (AUD)".into(), "Note".into()]));
        for line in &section.summary {
            let value = line.value.clone().or_else(|| line.amount.map(|a| format!("{:.2}", a))).unwrap_or_default();
            out.push_str(&row(vec![line.item.clone().unwrap_or_default(), line.label.trim().to_string(), value, line.note.clone().unwrap_or_default()]));
        }
        for table in &section.tables {
            out.push('\n');
            out.push_str(&row(vec![format!("{} — {}", section.title, table.title)]));
            out.push_str(&row(table.columns.iter().map(|c| c.label.clone()).collect()));
            for cells in &table.rows {
                out.push_str(&row(cells.iter().zip(&table.columns).map(|(v, c)| cell_text(v, c.kind, false)).collect()));
            }
        }
        for note in &section.notes {
            out.push_str(&row(vec!["Note".into(), note.clone()]));
        }
    }
    if !report.worksheet.is_empty() {
        out.push('\n');
        out.push_str(&row(vec!["To complete by hand".into()]));
        for table in &report.worksheet {
            out.push('\n');
            out.push_str(&row(vec![format!("To complete — {}", table.title)]));
            out.push_str(&row(table.columns.iter().map(|c| c.label.clone()).collect()));
            for cells in &table.rows {
                out.push_str(&row(cells.iter().zip(&table.columns).map(|(v, c)| cell_text(v, c.kind, false)).collect()));
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// The report endpoint
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub(crate) struct TaxReportQuery {
    /// The year the income year ends in; defaults to the last one finished.
    pub(crate) fy: Option<i32>,
    /// Comma-separated section keys; every section when absent.
    pub(crate) sections: Option<String>,
    /// "full" (the default) or "summary".
    pub(crate) detail: Option<String>,
    /// Net capital losses carried in, overriding the worked-out figure.
    pub(crate) losses_carried_in: Option<f64>,
    /// "json" (the default), "csv" or "txt".
    pub(crate) format: Option<String>,
}

#[utoipa::path(get, path = "/api/v1/tax/report", tag = "tax", responses((status = 200, description = "The tax report for one income year, as JSON, CSV or text")))]
#[get("/api/tax/report")]
pub(crate) async fn get_tax_report(db_path: web::Data<PathBuf>, query: web::Query<TaxReportQuery>) -> impl Responder {
    let today = today_local();
    let fy = match query.fy {
        Some(y) if (1990..=2100).contains(&y) => TaxYear(y),
        Some(y) => return err_bad_request(format!("Income year {} is out of range", y)),
        None => {
            let default = open_db(db_path.as_ref()).map_err(|e| e.to_string()).and_then(|c| tax_years(&c, today));
            match default {
                Ok((_, y)) => TaxYear(y),
                Err(err) => return err_internal(err),
            }
        }
    };
    let sections: HashSet<String> = match &query.sections {
        None => TAX_SECTIONS.iter().map(|s| s.to_string()).collect(),
        Some(list) => {
            let keys: HashSet<String> = list.split(',').map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty()).collect();
            if let Some(unknown) = keys.iter().find(|k| !TAX_SECTIONS.contains(&k.as_str())) {
                return err_bad_request(format!("Unknown section '{}'. Valid sections: {}", unknown, TAX_SECTIONS.join(", ")));
            }
            keys
        }
    };
    let full = match query.detail.as_deref() {
        None | Some("full") => true,
        Some("summary") => false,
        Some(other) => return err_bad_request(format!("Unknown detail '{}': use full or summary", other)),
    };
    let format = query.format.as_deref().unwrap_or("json");
    if !matches!(format, "json" | "csv" | "txt") {
        return err_bad_request(format!("Unknown format '{}': use json, csv or txt", format));
    }
    if query.losses_carried_in.is_some_and(|v| !v.is_finite() || v < 0.0) {
        return err_bad_request("losses_carried_in must be zero or more");
    }

    // Current prices only for the year in progress, and from the same source
    // as the Holdings screen so the two agree.
    let prices_aud = if sections.contains("planning") && fy.contains(today) {
        match build_portfolio_context(&db_path).await {
            Ok(ctx) => Some(ctx.prices.iter().filter_map(|(s, p)| p.aud.map(|a| (s.clone(), a))).collect()),
            Err(err) => {
                let _ = insert_event_log(&db_path, "warn", "tax_report", "api", None, &format!("Planning section left out — prices unavailable: {}", err));
                None
            }
        }
    } else {
        None
    };

    let options = ReportOptions { fy, sections, full, losses_carried_in: query.losses_carried_in, today, prices_aud };
    let report = match build_tax_report(&db_path, &options) {
        Ok(r) => r,
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "tax_report", "api", None, &err);
            return err_internal(err);
        }
    };
    let warnings = report.checks.iter().filter(|c| c.severity == "warn").count();
    if warnings > 0 {
        let _ = insert_event_log(&db_path, "warn", "tax_report", "api", None, &format!(
            "{} tax report: {} check{} need attention",
            fy.label(), warnings, if warnings == 1 { "" } else { "s" }
        ));
    }

    match format {
        "csv" => HttpResponse::Ok()
            .content_type("text/csv; charset=utf-8")
            .insert_header(("Content-Disposition", format!("attachment; filename=\"tax-report-{}.csv\"", fy.slug())))
            .body(report_csv(&report)),
        "txt" => HttpResponse::Ok().content_type("text/plain; charset=utf-8").body(report.text),
        _ => HttpResponse::Ok().json(report),
    }
}

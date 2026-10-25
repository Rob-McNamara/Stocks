//! Dividend import: correct recorded dividends — and record missing ones —
//! from a CSV of what was actually paid, such as a broker's dividend summary.
//!
//! One endpoint, two steps. Without `apply`, every row is matched to a
//! recorded dividend and the changes it would make come back for review;
//! nothing is written. With `apply` and the lines the user ticked, the same
//! plan is worked out again from the same file and those lines are written
//! in one transaction, each dividend's cash leg rewritten to match.
//!
//! Matching is by symbol and ex-date, allowing a few days' difference:
//! Yahoo dates US-domiciled ASX funds by their US ex-date, a day or three
//! from the ASX one the broker uses. A corrected date keeps its link to the
//! fetched event (`source_ex_date`), so the event is not recorded again.

use super::*;
use std::collections::HashSet;

/// How far a statement's ex-date may sit from a recorded one and still be the
/// same dividend. Distributions from one fund are at least a month apart.
const MATCH_WINDOW_DAYS: i64 = 7;

/// The columns, and the header names each may go by. Headers are compared
/// lower-cased with punctuation as underscores, and a leading "est_" (the
/// broker's "Est. Franked Amount") is ignored.
const COLUMNS: &[(&str, &[&str])] = &[
    ("symbol", &["symbol", "code", "ticker", "asx_code", "security"]),
    ("ex_date", &["ex_date", "ex_dividend_date", "ex_div_date", "exdate"]),
    ("payment_date", &["payment_date", "pay_date", "paid", "date_paid"]),
    ("dividend_type", &["dividend_type", "type"]),
    ("dividend_per_share", &["dividend_per_share", "per_share", "dps", "amount_per_share"]),
    ("units", &["units", "shares", "quantity", "holding"]),
    ("unfranked_amount", &["unfranked_amount", "unfranked"]),
    ("franked_amount", &["franked_amount", "franked"]),
    ("total_amount", &["total_amount", "total", "total_dividend", "gross_amount", "amount"]),
    ("franking_credit", &["franking_credit", "franking_credits", "imputation_credit"]),
    ("withholding_tax", &["withholding_tax", "withholding", "tax_withheld", "withholding_amount"]),
    ("currency", &["currency"]),
];

/// Split CSV text into rows of fields: quoted fields, doubled quotes, commas
/// and line breaks inside quotes, CRLF, and a byte-order mark are handled.
pub(crate) fn parse_csv(text: &str) -> Result<Vec<Vec<String>>, String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if quoted {
            match c {
                '"' if chars.peek() == Some(&'"') => {
                    field.push('"');
                    chars.next();
                }
                '"' => quoted = false,
                _ => field.push(c),
            }
            continue;
        }
        match c {
            '"' if field.is_empty() => quoted = true,
            ',' => row.push(std::mem::take(&mut field)),
            '\r' => {}
            '\n' => {
                row.push(std::mem::take(&mut field));
                rows.push(std::mem::take(&mut row));
            }
            _ => field.push(c),
        }
    }
    if quoted {
        return Err("The file ends inside a quoted field".to_string());
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    // Blank lines carry no row.
    rows.retain(|r| r.iter().any(|f| !f.trim().is_empty()));
    Ok(rows)
}

fn header_key(name: &str) -> String {
    let mut key = String::new();
    for c in name.trim().to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            key.push(c);
        } else if !key.ends_with('_') {
            key.push('_');
        }
    }
    let key = key.trim_matches('_');
    key.strip_prefix("est_").unwrap_or(key).to_string()
}

fn parse_import_date(value: &str) -> Option<NaiveDate> {
    let value = value.trim();
    ["%Y-%m-%d", "%d/%m/%Y", "%d-%b-%Y", "%d %b %Y"]
        .iter()
        .find_map(|format| NaiveDate::parse_from_str(value, format).ok())
}

fn parse_amount(value: &str) -> Result<Option<f64>, String> {
    let cleaned: String = value.trim().chars().filter(|c| !matches!(c, '$' | ',' | ' ')).collect();
    if cleaned.is_empty() {
        return Ok(None);
    }
    cleaned
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite())
        .map(Some)
        .ok_or_else(|| format!("'{}' is not a number", value.trim()))
}

/// One row of the file, checked and with what can be derived filled in.
#[derive(Debug, Clone)]
pub(crate) struct ImportRow {
    pub(crate) line: usize,
    pub(crate) symbol: String,
    pub(crate) ex_date: NaiveDate,
    pub(crate) payment_date: Option<NaiveDate>,
    pub(crate) dividend_type: Option<String>,
    pub(crate) per_share: Option<f64>,
    pub(crate) units: Option<f64>,
    pub(crate) franked: Option<f64>,
    pub(crate) total: Option<f64>,
    pub(crate) franking_credit: Option<f64>,
    pub(crate) withholding: Option<f64>,
    pub(crate) currency: String,
}

/// One line of the file: its number, and the row or why it couldn't be read.
pub(crate) type ImportLine = (usize, Result<ImportRow, String>);

/// The file's rows, each parsed or with the reason it couldn't be. Errors
/// that concern the whole file (no header, a required column missing) fail it.
pub(crate) fn read_import_rows(text: &str) -> Result<Vec<ImportLine>, String> {
    let rows = parse_csv(text)?;
    let Some((header, body)) = rows.split_first() else {
        return Err("The file is empty".to_string());
    };
    let keys: Vec<String> = header.iter().map(|h| header_key(h)).collect();
    let index: HashMap<&str, usize> = COLUMNS
        .iter()
        .filter_map(|(name, aliases)| keys.iter().position(|k| aliases.contains(&k.as_str())).map(|i| (*name, i)))
        .collect();
    for required in ["symbol", "ex_date"] {
        if !index.contains_key(required) {
            return Err(format!("The header has no {} column. Expected columns: {}", required, COLUMNS.iter().map(|(n, _)| *n).collect::<Vec<_>>().join(", ")));
        }
    }
    if !index.contains_key("total_amount") && !index.contains_key("dividend_per_share") && !index.contains_key("franked_amount") {
        return Err("The file needs a total_amount or dividend_per_share column (or franking figures) — there is nothing to import".to_string());
    }

    Ok(body
        .iter()
        .enumerate()
        .map(|(i, fields)| {
            // Line numbers as a spreadsheet shows them: the header is line 1.
            let line = i + 2;
            let get = |name: &str| index.get(name).and_then(|&i| fields.get(i)).map(|v| v.trim()).unwrap_or("");
            let parsed = (|| -> Result<ImportRow, String> {
                let symbol = normalize_symbol(get("symbol"));
                if symbol.is_empty() {
                    return Err("No symbol".to_string());
                }
                let ex_date = parse_import_date(get("ex_date")).ok_or_else(|| format!("Ex-date '{}' is not a date", get("ex_date")))?;
                let payment_date = match get("payment_date") {
                    "" => None,
                    value => Some(parse_import_date(value).ok_or_else(|| format!("Payment date '{}' is not a date", value))?),
                };
                let amount = |name: &str| parse_amount(get(name)).map_err(|e| format!("{}: {}", name, e));
                let per_share = amount("dividend_per_share")?;
                let units = amount("units")?;
                let unfranked = amount("unfranked_amount")?;
                let franked = amount("franked_amount")?;
                let total = amount("total_amount")?
                    .or_else(|| per_share.zip(units).map(|(p, u)| p * u))
                    .or_else(|| unfranked.zip(franked).map(|(u, f)| u + f));
                if let (Some(u), Some(f), Some(t)) = (unfranked, franked, total)
                    && (u + f - t).abs() > 0.011
                {
                    return Err(format!("Unfranked {:.2} plus franked {:.2} doesn't make the total {:.2}", u, f, t));
                }
                // A summary rounds the per-share figure to four places, so the
                // product may miss the total by that rounding over the units.
                if let (Some(p), Some(u), Some(t)) = (per_share, units, total)
                    && (p * u - t).abs() > u * 0.00005 + 0.011
                {
                    return Err(format!("Dividend per share × units ({:.2}) doesn't make the total {:.2}", p * u, t));
                }
                if [per_share, units, franked, total].iter().flatten().any(|v| *v < 0.0) {
                    return Err("Amounts can't be negative".to_string());
                }
                let currency = match get("currency").to_uppercase() {
                    c if c.is_empty() => "AUD".to_string(),
                    c => c,
                };
                Ok(ImportRow {
                    line,
                    symbol,
                    ex_date,
                    payment_date,
                    dividend_type: Some(get("dividend_type").to_string()).filter(|t| !t.is_empty()),
                    per_share,
                    units,
                    franked: franked.or_else(|| unfranked.zip(total).map(|(u, t)| t - u)),
                    total,
                    franking_credit: amount("franking_credit")?,
                    withholding: amount("withholding_tax")?,
                    currency,
                })
            })();
            (line, parsed)
        })
        .collect())
}

/// One field a row would change, for the preview.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub(crate) struct ImportChange {
    pub(crate) field: &'static str,
    pub(crate) from: Option<String>,
    pub(crate) to: Option<String>,
}

/// What importing one row would do (or did).
#[derive(Serialize, Clone, Debug)]
pub(crate) struct ImportPlanRow {
    pub(crate) line: usize,
    pub(crate) symbol: String,
    pub(crate) ex_date: String,
    /// "update", "create", "unchanged" or "error".
    pub(crate) status: &'static str,
    pub(crate) transaction_id: Option<i64>,
    /// The recorded dividend's ex-date, when it differs from the file's.
    pub(crate) matched_date: Option<String>,
    pub(crate) changes: Vec<ImportChange>,
    pub(crate) warnings: Vec<String>,
    pub(crate) error: Option<String>,
    /// Ticked in the preview: a clean update or a new dividend. A row with a
    /// warning waits for the user to look at it.
    pub(crate) selected: bool,
    pub(crate) applied: bool,
    #[serde(skip)]
    write: Option<DividendWrite>,
}

/// The values a dividend row ends up with.
#[derive(Clone, Debug, PartialEq)]
struct DividendFields {
    date: String,
    payment_date: Option<String>,
    dividend_type: Option<String>,
    quantity: Option<f64>,
    price: Option<f64>,
    original_price: Option<f64>,
    amount: Option<f64>,
    franked_amount: Option<f64>,
    franking_credit: Option<f64>,
    withholding_amount: Option<f64>,
}

#[derive(Clone, Debug)]
enum DividendWrite {
    Update { id: i64, old_date: String, fields: DividendFields },
    Create {
        symbol: String,
        currency: String,
        fx_rate: Option<f64>,
        cash_account_id: Option<i64>,
        source_ex_date: Option<String>,
        fields: DividendFields,
    },
}

fn money_str(v: Option<f64>) -> Option<String> {
    v.map(|v| format!("{:.2}", v))
}

fn number_str(v: Option<f64>) -> Option<String> {
    v.map(|v| {
        let s = format!("{:.6}", v);
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    })
}

fn differs(a: Option<f64>, b: Option<f64>, tolerance: f64) -> bool {
    match (a, b) {
        (Some(x), Some(y)) => (x - y).abs() > tolerance,
        (None, None) => false,
        _ => true,
    }
}

/// The field-by-field difference between a dividend as stored and as it
/// would be.
fn field_changes(before: &DividendFields, after: &DividendFields, foreign: bool) -> Vec<ImportChange> {
    let mut changes = Vec::new();
    let mut push = |field: &'static str, from: Option<String>, to: Option<String>| {
        if from != to {
            changes.push(ImportChange { field, from, to });
        }
    };
    push("Ex-date", Some(before.date.clone()), Some(after.date.clone()));
    push("Payment date", before.payment_date.clone(), after.payment_date.clone());
    push("Type", before.dividend_type.clone(), after.dividend_type.clone());
    if differs(before.quantity, after.quantity, 1e-6) {
        push("Units", number_str(before.quantity), number_str(after.quantity));
    }
    if foreign && differs(before.original_price, after.original_price, 1e-6) {
        push("Per share (trade ccy)", number_str(before.original_price), number_str(after.original_price));
    } else if !foreign && differs(before.price, after.price, 1e-6) {
        push("Per share", number_str(before.price), number_str(after.price));
    }
    if differs(before.amount, after.amount, 0.005) {
        push("Total (AUD)", money_str(before.amount), money_str(after.amount));
    }
    if differs(before.franked_amount, after.franked_amount, 0.005) {
        push("Franked", money_str(before.franked_amount), money_str(after.franked_amount));
    }
    if differs(before.franking_credit, after.franking_credit, 0.005) {
        push("Franking credit", money_str(before.franking_credit), money_str(after.franking_credit));
    }
    if differs(before.withholding_amount, after.withholding_amount, 0.005) {
        push("Tax withheld", money_str(before.withholding_amount), money_str(after.withholding_amount));
    }
    changes
}

/// Choices that change how rows are read.
#[derive(Deserialize, Clone, Copy, Default, Debug)]
pub(crate) struct ImportOptions {
    /// Where a statement's total is lower than the recorded one, the gap is
    /// tax withheld: the recorded gross stays and the gap becomes the
    /// dividend's withholding. A broker's summary shows a fund that pays
    /// after foreign tax (VEU and VTS, 15% US tax) at the net amount.
    #[serde(default)]
    pub(crate) shortfall_is_withholding: bool,
}

/// Match every row to a recorded dividend and work out what it would change.
/// Reads only.
pub(crate) fn plan_dividend_import(db_path: &Path, text: &str, options: ImportOptions) -> Result<Vec<ImportPlanRow>, String> {
    let parsed = read_import_rows(text)?;
    let txs = fetch_holdings(&db_path.to_path_buf())?;
    let conn = open_db(db_path).map_err(|e| e.to_string())?;

    let symbols: HashSet<&str> = txs.iter().map(|t| t.symbol.as_str()).collect();
    // Symbols known to have tax taken at source, at their standing rate.
    let mut standing: HashMap<String, f64> = HashMap::new();
    {
        let mut stmt = conn
            .prepare("SELECT symbol, dividend_withholding_pct FROM symbol_info WHERE dividend_withholding_pct > 0")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?)))
            .map_err(|e| e.to_string())?;
        for row in rows {
            let (symbol, pct) = row.map_err(|e| e.to_string())?;
            standing.insert(symbol, pct);
        }
    }
    let mut by_symbol: HashMap<&str, Vec<&HoldingTransaction>> = HashMap::new();
    for tx in &txs {
        by_symbol.entry(tx.symbol.as_str()).or_default().push(tx);
    }
    let mut claimed: HashSet<i64> = HashSet::new();
    let mut seen: HashMap<(String, NaiveDate), usize> = HashMap::new();
    let mut plan = Vec::new();

    for (line, row) in parsed {
        let row = match row {
            Ok(r) => r,
            Err(error) => {
                plan.push(error_row(line, "", "", error));
                continue;
            }
        };
        // A bare ASX code finds its ".AX" holding.
        let symbol = if symbols.contains(row.symbol.as_str()) {
            row.symbol.clone()
        } else if !row.symbol.contains('.') && symbols.contains(format!("{}.AX", row.symbol).as_str()) {
            format!("{}.AX", row.symbol)
        } else {
            plan.push(error_row(line, &row.symbol, &row.ex_date.to_string(), format!("No {} holding in the app", row.symbol)));
            continue;
        };
        if let Some(first) = seen.insert((symbol.clone(), row.ex_date), line) {
            plan.push(error_row(line, &symbol, &row.ex_date.to_string(), format!("Same dividend as line {}", first)));
            continue;
        }

        let symbol_txs = by_symbol.get(symbol.as_str()).cloned().unwrap_or_default();
        // The recorded dividend this row is about: the same ex-date, or the
        // nearest one within the window that no other row has taken.
        let candidate = symbol_txs
            .iter()
            .filter(|t| t.transaction_type == "dividend" && !claimed.contains(&t.id))
            .filter_map(|t| rules_date(&t.date).map(|d| (t, (d - row.ex_date).num_days().abs())))
            .filter(|(_, gap)| *gap <= MATCH_WINDOW_DAYS)
            .min_by_key(|(t, gap)| (*gap, t.date.clone()))
            .map(|(t, _)| *t);

        let planned = match candidate {
            Some(tx) => {
                claimed.insert(tx.id);
                plan_update(&row, &symbol, tx, options, standing.get(&symbol).copied())
            }
            None => plan_create(&conn, &row, &symbol, &symbol_txs)?,
        };
        plan.push(planned);
    }
    Ok(plan)
}

fn rules_date(value: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(value.get(..10).unwrap_or(value), "%Y-%m-%d").ok()
}

fn error_row(line: usize, symbol: &str, ex_date: &str, error: String) -> ImportPlanRow {
    ImportPlanRow {
        line,
        symbol: symbol.to_string(),
        ex_date: ex_date.to_string(),
        status: "error",
        transaction_id: None,
        matched_date: None,
        changes: Vec::new(),
        warnings: Vec::new(),
        error: Some(error),
        selected: false,
        applied: false,
        write: None,
    }
}

/// `standing_pct` is the symbol's standing withholding rate, when it has one.
fn plan_update(row: &ImportRow, symbol: &str, tx: &HoldingTransaction, options: ImportOptions, standing_pct: Option<f64>) -> ImportPlanRow {
    let before = DividendFields {
        date: tx.date.clone(),
        payment_date: tx.payment_date.clone(),
        dividend_type: tx.dividend_type.clone(),
        quantity: tx.quantity,
        price: tx.price,
        original_price: tx.original_price,
        amount: tx.amount,
        franked_amount: tx.franked_amount,
        franking_credit: tx.franking_credit,
        withholding_amount: tx.withholding_amount,
    };
    let mut after = before.clone();
    let mut warnings = Vec::new();
    // (the share of the gross taken as tax, the gross) when a shortfall is
    // read as tax withheld, for saying so in the preview.
    let mut withheld_reading: Option<(f64, f64)> = None;
    after.date = row.ex_date.to_string();
    if let Some(paid) = row.payment_date {
        after.payment_date = Some(paid.to_string());
    }
    if row.dividend_type.is_some() {
        after.dividend_type = row.dividend_type.clone();
    }

    let foreign = tx.currency != "AUD";
    if foreign && row.currency != tx.currency {
        // The amounts are in a different currency from the dividend's, with
        // no rate in the file to bridge them: change only what isn't money.
        if row.total.is_some() || row.per_share.is_some() {
            warnings.push(format!(
                "Recorded in {} but the file is in {}: the amounts are left as they are",
                tx.currency, row.currency
            ));
        }
    } else {
        if let Some(units) = row.units {
            if tx.quantity.is_some_and(|q| (q - units).abs() > 1e-6) {
                warnings.push(format!(
                    "Units differ: {} recorded, {} on the statement",
                    number_str(tx.quantity).unwrap_or_default(),
                    number_str(Some(units)).unwrap_or_default()
                ));
            }
            after.quantity = Some(units);
        }
        // For a foreign dividend in its own currency the file's figures are
        // native; the AUD ones follow at the dividend's stored rate.
        let rate = if foreign { tx.fx_rate.filter(|r| *r > 0.0) } else { Some(1.0) };
        if let Some(total) = row.total {
            match rate {
                Some(rate) => {
                    let aud = total * rate;
                    let recorded = tx.amount.filter(|a| *a > 0.0);
                    let shortfall = recorded.map(|r| r - aud).filter(|gap| *gap > 0.0);
                    let material = recorded.is_some_and(|r| ((aud - r) / r).abs() > 0.01);
                    // A symbol with tax taken at source, short by no more than
                    // that tax, is a statement showing the net — read so
                    // without being asked. Replacing the gross with the net
                    // loses the income and the tax both, and the standing rate
                    // is then taken off the net a second time.
                    let looks_withheld = standing_pct
                        .zip(shortfall.zip(recorded))
                        .is_some_and(|(pct, (gap, gross))| gap / gross <= pct / 100.0 + 0.005);
                    if material && row.withholding.is_none() && (options.shortfall_is_withholding || looks_withheld)
                        && let (Some(gap), Some(gross)) = (shortfall, recorded)
                    {
                        // The statement's figure is the cash after tax: keep
                        // the gross and record what was taken, in the
                        // dividend's own currency.
                        after.withholding_amount = Some(gap / rate);
                        withheld_reading = Some((gap / gross * 100.0, gross));
                    } else if material {
                        let recorded = recorded.unwrap_or_default();
                        warnings.push(format!(
                            "Total differs from what is recorded: {:.2} → {:.2} ({:+.1}%). If the statement shows the amount after tax withheld, tick \"lower totals are tax withheld\", or put the gross in total_amount and the tax in withholding_tax.",
                            recorded,
                            aud,
                            (aud - recorded) / recorded * 100.0
                        ));
                        after.amount = Some(aud);
                        set_per_share(&mut after, row.per_share, total, rate, foreign);
                    } else if recorded.is_none() || (aud - recorded.unwrap_or_default()).abs() > 0.005 {
                        after.amount = Some(aud);
                        set_per_share(&mut after, row.per_share, total, rate, foreign);
                    }
                    // Otherwise the totals agree to the cent: the stored
                    // per-share figure is the more precise one (a summary
                    // rounds it to four places), so it stays.
                }
                None => warnings.push(format!("No stored {}/AUD rate on this dividend: the amounts are left as they are", tx.currency)),
            }
        }
        if let Some(withheld) = row.withholding {
            after.withholding_amount = Some(withheld);
        }
    }
    if !foreign || row.currency == "AUD" {
        if row.franked.is_some() {
            after.franked_amount = row.franked;
        }
        if row.franking_credit.is_some() {
            after.franking_credit = row.franking_credit;
        }
    }

    let mut changes = field_changes(&before, &after, foreign);
    if let Some((pct, gross)) = withheld_reading
        && let Some(change) = changes.iter_mut().find(|c| c.field == "Tax withheld")
    {
        change.to = change.to.take().map(|to| format!("{} ({:.1}% of the {:.2} recorded)", to, pct, gross));
    }
    let matched_date = (tx.date != after.date).then(|| tx.date.clone());
    let status = if changes.is_empty() { "unchanged" } else { "update" };
    ImportPlanRow {
        line: row.line,
        symbol: symbol.to_string(),
        ex_date: row.ex_date.to_string(),
        status,
        transaction_id: Some(tx.id),
        matched_date,
        selected: status == "update" && warnings.is_empty(),
        changes,
        warnings,
        error: None,
        applied: false,
        write: (status == "update").then(|| DividendWrite::Update { id: tx.id, old_date: tx.date.clone(), fields: after }),
    }
}

/// A changed total brings its per-share figure with it: the statement's own,
/// or the total over the units.
fn set_per_share(after: &mut DividendFields, per_share: Option<f64>, total: f64, rate: f64, foreign: bool) {
    let native = per_share.or_else(|| after.quantity.filter(|q| *q > 0.0).map(|q| total / q));
    if foreign {
        after.original_price = native;
        after.price = native.map(|p| p * rate);
    } else {
        after.price = native;
    }
}

fn plan_create(conn: &Connection, row: &ImportRow, symbol: &str, symbol_txs: &[&HoldingTransaction]) -> Result<ImportPlanRow, String> {
    let ex_date = row.ex_date.to_string();
    let mut warnings = Vec::new();
    let owned: Vec<HoldingTransaction> = symbol_txs.iter().map(|t| (*t).clone()).collect();
    let held = portfolio::shares_on_date(&to_portfolio_txs(&owned), &ex_date);
    let quantity = row.units.or((held > 0.0).then_some(held));
    if held <= 0.0 {
        warnings.push(format!("The app shows no {} held on {}", symbol, ex_date));
    } else if let Some(units) = row.units.filter(|u| (u - held).abs() > 1e-6) {
        warnings.push(format!("The statement says {} units; the app shows {} held on the ex-date", number_str(Some(units)).unwrap_or_default(), number_str(Some(held)).unwrap_or_default()));
    }

    let currency = owned
        .iter()
        .map(|t| t.currency.to_uppercase())
        .find(|c| !c.is_empty())
        .unwrap_or_else(|| "AUD".to_string());
    let (fx_rate, rate) = if currency == "AUD" {
        (None, Some(1.0))
    } else if row.currency == currency {
        let rate = fx_rate_on(conn, &currency, &ex_date);
        (rate, rate)
    } else {
        (None, None)
    };
    let Some(rate) = rate else {
        let reason = if row.currency != currency {
            format!("{} dividends are recorded in {}, but this row is in {}", symbol, currency, row.currency)
        } else {
            format!("No {}/AUD rate for {}", currency, ex_date)
        };
        return Ok(error_row(row.line, symbol, &ex_date, reason));
    };
    let Some(total) = row.total.or_else(|| row.per_share.zip(quantity).map(|(p, q)| p * q)) else {
        return Ok(error_row(row.line, symbol, &ex_date, "No total_amount, nor dividend_per_share and units, to record".to_string()));
    };
    let per_share = row.per_share.or_else(|| quantity.filter(|q| *q > 0.0).map(|q| total / q));

    let excluded: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM dividend_exclusions WHERE symbol = ?1 AND ex_date = ?2",
            params![symbol, ex_date],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    if excluded > 0 {
        warnings.push("This dividend was deleted before, as not received".to_string());
    }

    // A fetched event a few days off is the same dividend under Yahoo's date:
    // link it, or the next refresh would record it again.
    let source_ex_date: Option<String> = conn
        .query_row(
            "SELECT ex_date FROM dividend_events
              WHERE symbol = ?1 AND ex_date <> ?2
                AND ABS(julianday(ex_date) - julianday(?2)) <= ?3
              ORDER BY ABS(julianday(ex_date) - julianday(?2)) LIMIT 1",
            params![symbol, ex_date, MATCH_WINDOW_DAYS],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;

    let cash_account_id = dividend_account_for_currency(conn, &currency);
    let account_name: Option<String> = match cash_account_id {
        Some(id) => conn
            .query_row("SELECT name FROM cash_accounts WHERE id = ?1", params![id], |r| r.get(0))
            .optional()
            .map_err(|e| e.to_string())?,
        None => None,
    };
    if cash_account_id.is_none() {
        warnings.push(format!("No dividend account is set for {}, so no cash is credited", currency));
    }

    let fields = DividendFields {
        date: ex_date.clone(),
        payment_date: row.payment_date.map(|d| d.to_string()),
        dividend_type: row.dividend_type.clone(),
        quantity,
        price: per_share.map(|p| p * rate),
        original_price: (currency != "AUD").then_some(per_share).flatten(),
        amount: Some(total * rate),
        franked_amount: if currency == "AUD" { row.franked } else { None },
        franking_credit: if currency == "AUD" { row.franking_credit } else { None },
        withholding_amount: row.withholding,
    };
    let empty = DividendFields {
        date: ex_date.clone(),
        payment_date: None,
        dividend_type: None,
        quantity: None,
        price: None,
        original_price: None,
        amount: None,
        franked_amount: None,
        franking_credit: None,
        withholding_amount: None,
    };
    let mut changes = field_changes(&empty, &fields, currency != "AUD");
    if let Some(name) = &account_name {
        changes.push(ImportChange { field: "Credited to", from: None, to: Some(name.clone()) });
    }
    Ok(ImportPlanRow {
        line: row.line,
        symbol: symbol.to_string(),
        ex_date,
        status: "create",
        transaction_id: None,
        matched_date: None,
        selected: warnings.is_empty(),
        changes,
        warnings,
        error: None,
        applied: false,
        write: Some(DividendWrite::Create {
            symbol: symbol.to_string(),
            currency,
            fx_rate,
            cash_account_id,
            source_ex_date,
            fields,
        }),
    })
}

/// Write the chosen lines of a plan, all together or not at all.
pub(crate) fn apply_dividend_import(db_path: &Path, plan: &mut [ImportPlanRow], lines: &HashSet<usize>) -> Result<(usize, usize), String> {
    with_tx(db_path, |conn| {
        let (mut updated, mut created) = (0, 0);
        for row in plan.iter_mut().filter(|r| lines.contains(&r.line)) {
            let Some(write) = &row.write else { continue };
            let id = match write {
                DividendWrite::Update { id, old_date, fields } => {
                    conn.execute(
                        "UPDATE holdings_transactions
                            SET date = ?2, payment_date = ?3, dividend_type = ?4, quantity = ?5, price = ?6,
                                original_price = ?7, amount = ?8, franked_amount = ?9, franking_credit = ?10,
                                withholding_amount = ?11
                          WHERE id = ?1",
                        params![
                            id, fields.date, fields.payment_date, fields.dividend_type, fields.quantity, fields.price,
                            fields.original_price, fields.amount, fields.franked_amount, fields.franking_credit,
                            fields.withholding_amount,
                        ],
                    )
                    .map_err(|e| e.to_string())?;
                    keep_source_event(conn, *id, &row.symbol, old_date)?;
                    updated += 1;
                    *id
                }
                DividendWrite::Create { symbol, currency, fx_rate, cash_account_id, source_ex_date, fields } => {
                    conn.execute(
                        "INSERT INTO holdings_transactions (symbol, transaction_type, date, quantity, price, amount, notes,
                            created_at, currency, original_price, fx_rate, cash_account_id, withholding_amount,
                            franked_amount, franking_credit, payment_date, dividend_type, source_ex_date)
                         VALUES (?1, 'dividend', ?2, ?3, ?4, ?5, 'Imported from a dividend statement', ?6, ?7, ?8, ?9,
                            ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
                        params![
                            symbol, fields.date, fields.quantity, fields.price, fields.amount, Utc::now().to_rfc3339(),
                            currency, fields.original_price, fx_rate, cash_account_id, fields.withholding_amount,
                            fields.franked_amount, fields.franking_credit, fields.payment_date, fields.dividend_type,
                            source_ex_date,
                        ],
                    )
                    .map_err(|e| e.to_string())?;
                    created += 1;
                    conn.last_insert_rowid()
                }
            };
            // The cash leg follows the new amount, tax withheld and payment date.
            sync_trade_cash_leg(conn, id)?;
            row.transaction_id = Some(id);
            row.applied = true;
        }
        Ok((updated, created))
    })
}

#[derive(Deserialize)]
pub(crate) struct DividendImportPayload {
    /// The CSV file's text.
    pub(crate) csv: String,
    /// False (the default) only previews.
    pub(crate) apply: Option<bool>,
    /// The file's line numbers to write when applying.
    pub(crate) lines: Option<Vec<usize>>,
    #[serde(default)]
    pub(crate) options: ImportOptions,
}

#[utoipa::path(post, path = "/api/v1/dividends/import", tag = "dividends",
    responses((status = 200, description = "Preview, or apply, dividend details from a CSV file")))]
#[post("/api/dividends/import")]
pub(crate) async fn import_dividends(db_path: web::Data<PathBuf>, payload: web::Json<DividendImportPayload>) -> impl Responder {
    let mut plan = match plan_dividend_import(&db_path, &payload.csv, payload.options) {
        Ok(plan) => plan,
        Err(err) => {
            // The file itself is unusable — a missing column, an unclosed
            // quote — which is the user's to fix, not a server failure.
            let _ = insert_event_log(&db_path, "warn", "dividend_import", "api", None, &err);
            return err_bad_request(err);
        }
    };
    let apply = payload.apply.unwrap_or(false);
    let (mut updated, mut created) = (0, 0);
    if apply {
        let lines: HashSet<usize> = payload.lines.clone().unwrap_or_default().into_iter().collect();
        match apply_dividend_import(&db_path, &mut plan, &lines) {
            Ok(counts) => (updated, created) = counts,
            Err(err) => {
                let _ = insert_event_log(&db_path, "error", "dividend_import", "api", None, &err);
                return err_internal(err);
            }
        }
        let _ = insert_event_log(&db_path, "info", "dividend_import", "api", None, &format!(
            "Dividend import: {} updated, {} recorded", updated, created
        ));
    }
    let count = |status: &str| plan.iter().filter(|r| r.status == status).count();
    HttpResponse::Ok().json(serde_json::json!({
        "applied": apply,
        "updated": updated,
        "created": created,
        "counts": {
            "update": count("update"),
            "create": count("create"),
            "unchanged": count("unchanged"),
            "error": count("error"),
        },
        "rows": plan,
    }))
}

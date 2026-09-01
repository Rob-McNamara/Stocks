//! Portfolio calculation engine — FIFO lot matching, realised/unrealised P/L
//! and dividend attribution.
//!
//! Ported line-for-line from `web/src/utils/fifo.ts`, `holdings.ts` and the
//! SoldStocks screen, together with the fifo.test.ts suite, so that every
//! client (web, iOS, Android) consumes one authoritative implementation via
//! the /api/portfolio/* endpoints.
//!
//! Semantics preserved from the TypeScript engine:
//! - Transactions sort by date (lexicographic ISO date) then id.
//! - Purchases/sales participate in FIFO only when quantity AND price are set
//!   (calc_remaining_by_lot needs only quantity, matching calcRemainingByLot).
//! - A dividend dollar is counted exactly once: on the holdings side while any
//!   shares remain, on the sold side once the position is fully closed.
//! - `dividends_total` (pre-computed by the API from dividend_events) wins
//!   over manually recorded dividend transactions when positive.

use chrono::NaiveDate;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxType {
    Purchase,
    Sale,
    Dividend,
    Other,
}

impl TxType {
    pub fn parse(s: &str) -> TxType {
        match s {
            "purchase" => TxType::Purchase,
            "sale" => TxType::Sale,
            "dividend" => TxType::Dividend,
            _ => TxType::Other,
        }
    }
}

/// The minimal transaction view the engine needs. `price` is always AUD;
/// `native_price` is the original-currency price for international stocks
/// (falls back to `price` when absent).
#[derive(Debug, Clone)]
pub struct PortfolioTx {
    pub id: i64,
    pub symbol: String,
    pub tx_type: TxType,
    /// YYYY-MM-DD — lexicographic order equals chronological order.
    pub date: String,
    pub quantity: Option<f64>,
    pub price: Option<f64>,
    pub native_price: Option<f64>,
    pub amount: Option<f64>,
    pub brokerage: Option<f64>,
    pub dividends_total: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Lot {
    pub quantity: f64,
    pub price: f64,
}

/// One day of the portfolio's value, all figures in AUD.
#[derive(Debug, Clone, PartialEq)]
pub struct DailyValue {
    pub date: String,
    /// Market value of shares held at the close.
    pub stocks: f64,
    /// Cash across every account counted toward the portfolio.
    pub cash: f64,
    /// Net money that entered (+) or left (−) the portfolio that day.
    ///
    /// Contributions and withdrawals only. Dividends, interest, fees, trades
    /// and currency conversions are *not* flows — they are the portfolio
    /// earning, spending or rearranging its own money, which is exactly what
    /// the return is meant to measure.
    pub flow: f64,
}

impl DailyValue {
    pub fn total(&self) -> f64 {
        self.stocks + self.cash
    }
}

/// Time-weighted return over the series, as a fraction (0.07 = +7%).
///
/// Each day contributes `(V_d − F_d) / V_{d−1} − 1`, and the daily factors are
/// chained. Removing the day's flow before the comparison is what stops a
/// deposit registering as a gain: adding $1,000 raises `V_d` and `F_d` equally,
/// so the ratio is unchanged.
///
/// Days where the previous value was zero or negative contribute nothing —
/// there is no capital to earn a return on, which is the normal state before
/// the first contribution lands.
pub fn time_weighted_return(series: &[DailyValue]) -> Option<f64> {
    let mut factor = 1.0f64;
    let mut counted = 0usize;

    for pair in series.windows(2) {
        let previous = pair[0].total();
        let current = &pair[1];
        if previous <= 0.0 {
            continue;
        }
        factor *= (current.total() - current.flow) / previous;
        counted += 1;
    }

    if counted == 0 { None } else { Some(factor - 1.0) }
}

/// Total contributed (+) or withdrawn (−) across the series.
pub fn net_contributions(series: &[DailyValue]) -> f64 {
    series.iter().map(|p| p.flow).sum()
}

/// Sort transactions chronologically, then by id for same-day stability.
pub fn sort_transactions(txs: &[PortfolioTx]) -> Vec<PortfolioTx> {
    let mut sorted = txs.to_vec();
    sorted.sort_by(|a, b| a.date.cmp(&b.date).then(a.id.cmp(&b.id)));
    sorted
}

/// Apply a FIFO sale against a mutable lot queue.
/// Mutates lots in place. Returns the cost basis consumed.
pub fn apply_fifo_sale(lots: &mut Vec<Lot>, quantity: f64) -> f64 {
    let mut remaining = quantity;
    let mut cost_basis = 0.0;
    while remaining > 0.0 && !lots.is_empty() {
        let used = remaining.min(lots[0].quantity);
        cost_basis += used * lots[0].price;
        lots[0].quantity -= used;
        remaining -= used;
        if lots[0].quantity <= 0.0 {
            lots.remove(0);
        }
    }
    cost_basis
}

#[derive(Debug, Clone)]
pub struct SymbolSummary {
    pub symbol: String,
    /// Remaining lots after all sales (AUD prices)
    pub lots: Vec<Lot>,
    pub remaining_shares: f64,
    /// Cost basis of remaining shares (AUD), brokerage included
    pub remaining_cost: f64,
    /// Cost basis of remaining shares in the stock's native currency
    pub native_remaining_cost: f64,
    pub total_sold_qty: f64,
    /// Realised P/L from all sales (proceeds - cost basis - brokerage), excluding dividends
    pub realised_pl: f64,
    /// Dividend total read from dividends_total (pre-computed from dividend_events)
    pub dividends_total: f64,
}

/// The lots a purchase opens, with brokerage folded into the per-share cost.
///
/// The fee is part of what the shares cost. Leaving it out overstates profit by
/// the fee, and asymmetrically so: a sale's fee *is* deducted from its
/// proceeds, so only the buy side was being given away free.
///
/// `brokerage` is AUD by the engine's convention (the same one `realised_pl`
/// follows), so the native lot converts it back at the transaction's own
/// implied rate rather than mixing currencies inside one lot.
fn purchase_lots(tx: &PortfolioTx, qty: f64, price: f64) -> (Lot, Lot) {
    let fee_per_share = if qty > 0.0 { tx.brokerage.unwrap_or(0.0) / qty } else { 0.0 };
    let native_price = tx.native_price.unwrap_or(price);
    let native_fee_per_share = if price > 0.0 {
        fee_per_share * native_price / price
    } else {
        fee_per_share
    };
    (
        Lot { quantity: qty, price: price + fee_per_share },
        Lot { quantity: qty, price: native_price + native_fee_per_share },
    )
}

/// Calculate the FIFO summary for a single symbol's transactions.
pub fn calc_symbol_summary(txs: &[PortfolioTx]) -> SymbolSummary {
    let sorted = sort_transactions(txs);
    let symbol = sorted.first().map(|t| t.symbol.clone()).unwrap_or_default();

    let mut lots: Vec<Lot> = Vec::new();
    let mut native_lots: Vec<Lot> = Vec::new();
    let mut realised_pl = 0.0;
    let mut total_sold_qty = 0.0;
    let mut dividends_total = 0.0;

    for tx in &sorted {
        match (tx.tx_type, tx.quantity, tx.price) {
            (TxType::Purchase, Some(qty), Some(price)) => {
                let (lot, native_lot) = purchase_lots(tx, qty, price);
                lots.push(lot);
                native_lots.push(native_lot);
            }
            (TxType::Sale, Some(qty), Some(price)) => {
                let cost_basis = apply_fifo_sale(&mut lots, qty);
                apply_fifo_sale(&mut native_lots, qty);
                realised_pl += qty * price - tx.brokerage.unwrap_or(0.0) - cost_basis;
                total_sold_qty += qty;
            }
            _ => {}
        }
        if tx.dividends_total > 0.0 {
            dividends_total = tx.dividends_total;
        }
    }

    let remaining_shares = lots.iter().map(|l| l.quantity).sum();
    let remaining_cost = lots.iter().map(|l| l.quantity * l.price).sum();
    let native_remaining_cost = native_lots.iter().map(|l| l.quantity * l.price).sum();

    SymbolSummary { symbol, lots, remaining_shares, remaining_cost, native_remaining_cost, total_sold_qty, realised_pl, dividends_total }
}

/// One sale, with the cost of the shares it actually consumed.
#[derive(Debug, Clone, PartialEq)]
pub struct SaleCost {
    pub date: String,
    pub quantity: f64,
    /// Sale price per share, in the symbol's own currency.
    pub native_price: f64,
    /// What those particular shares cost, per share and native, brokerage
    /// included. `None` when the sale consumed no lot — a transaction recorded
    /// without its matching purchase.
    pub native_cost_per_share: Option<f64>,
    pub brokerage: Option<f64>,
}

/// Walk a symbol's transactions and cost each sale against the lots it consumed.
///
/// The FIFO queue is stateful, so which shares a sale consumes depends on every
/// sale before it. That makes per-sale cost something only a full walk can
/// answer, and the walk has to be this one — a second implementation would be
/// free to disagree with the holdings screens about the same trade.
///
/// Native throughout: a sale and the purchase behind it are the same instrument
/// on the same exchange, and converting either through a moving rate would put
/// a currency move inside a comparison meant to isolate the stock's own.
pub fn sale_costs(txs: &[PortfolioTx]) -> Vec<SaleCost> {
    let sorted = sort_transactions(txs);
    let mut native_lots: Vec<Lot> = Vec::new();
    let mut sales = Vec::new();

    for tx in &sorted {
        match (tx.tx_type, tx.quantity, tx.price) {
            (TxType::Purchase, Some(qty), Some(price)) => {
                let (_, native_lot) = purchase_lots(tx, qty, price);
                native_lots.push(native_lot);
            }
            (TxType::Sale, Some(qty), Some(price)) if qty > 0.0 => {
                let cost = apply_fifo_sale(&mut native_lots, qty);
                sales.push(SaleCost {
                    date: tx.date.clone(),
                    quantity: qty,
                    native_price: tx.native_price.unwrap_or(price),
                    // A sale with no lot behind it consumes nothing and costs
                    // nothing, which is not the same as having cost zero.
                    native_cost_per_share: if cost > 0.0 { Some(cost / qty) } else { None },
                    brokerage: tx.brokerage,
                });
            }
            _ => {}
        }
    }
    sales
}

/// Effective dividends for a symbol: the API-computed dividends_total when
/// positive, otherwise the sum of manually recorded dividend transactions.
/// Cost basis and dividends measured from a baseline date rather than from the
/// original purchase.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RebasedBasis {
    /// What the still-held shares were worth at the baseline, plus the actual
    /// cost of anything bought since.
    pub cost: f64,
    /// Dividends received on or after the baseline date.
    pub dividends: f64,
}

/// Re-strike a holding's cost basis at `basis_date`.
///
/// A position held for decades swamps any recent move — CSL at +2500% since
/// 2001 hides a 36% fall since 2025 — so this answers "how has it done lately"
/// without touching the transaction record, which stays the account of what was
/// actually bought and paid.
///
/// Shares already held on the baseline date are valued at `basis_price`.
/// Anything bought afterwards is valued at what it actually cost, because for
/// those shares the purchase *is* the baseline. Dividends paid before the date
/// belong to the earlier period and are excluded; counting them would credit
/// the new period with income it did not earn.
pub fn rebase_at(
    txs: &[PortfolioTx],
    remaining_shares: f64,
    basis_date: &str,
    basis_price: f64,
) -> RebasedBasis {
    let sorted = sort_transactions(txs);

    let mut net_before = 0.0;
    let mut later_shares = 0.0;
    let mut later_cost = 0.0;
    let mut dividends = 0.0;

    for tx in &sorted {
        let before = tx.date.as_str() < basis_date;
        let qty = tx.quantity.unwrap_or(0.0);
        match tx.tx_type {
            TxType::Purchase => {
                if before {
                    net_before += qty;
                } else {
                    later_shares += qty;
                    later_cost += qty * tx.price.unwrap_or(0.0) + tx.brokerage.unwrap_or(0.0);
                }
            }
            TxType::Sale => {
                if before {
                    net_before -= qty;
                } else {
                    later_shares -= qty;
                }
            }
            TxType::Dividend => {
                if !before {
                    dividends += tx.amount.unwrap_or(0.0);
                }
            }
            TxType::Other => {}
        }
    }

    // A sale after the baseline can eat into the shares that were held at it,
    // so the split is reconciled against what is actually left rather than
    // trusted from the running totals.
    let held_at_basis = net_before.clamp(0.0, remaining_shares.max(0.0));
    let from_later = (remaining_shares - held_at_basis).max(0.0);
    let later_avg = if later_shares > 0.0 { later_cost / later_shares } else { 0.0 };

    RebasedBasis {
        cost: held_at_basis * basis_price + from_later * later_avg,
        dividends,
    }
}

pub fn symbol_dividends(sorted: &[PortfolioTx]) -> f64 {
    let mut from_total = 0.0;
    let mut manual = 0.0;
    for tx in sorted {
        if tx.dividends_total > 0.0 {
            from_total = tx.dividends_total;
        } else if tx.tx_type == TxType::Dividend
            && let Some(amount) = tx.amount {
                manual += amount;
            }
    }
    if from_total > 0.0 { from_total } else { manual }
}

/// Net shares held at the close of `date` (YYYY-MM-DD): purchases add,
/// sales subtract, applied in (date, id) order and clamped at zero.
/// Dividend transactions don't change the share count. A purchase on the
/// ex-date itself counts as held.
pub fn shares_on_date(txs: &[PortfolioTx], date: &str) -> f64 {
    let sorted = sort_transactions(txs);
    let mut shares = 0.0;
    for tx in &sorted {
        if tx.date.as_str() > date {
            break;
        }
        match tx.tx_type {
            TxType::Purchase => shares += tx.quantity.unwrap_or(0.0),
            TxType::Sale => shares -= tx.quantity.unwrap_or(0.0),
            _ => {}
        }
    }
    shares.max(0.0)
}

/// A payment implied by a per-share dividend event.
#[derive(Debug, Clone)]
pub struct ImpliedDividendPayment {
    /// YYYY-MM-DD ex-dividend date
    pub ex_date: String,
    pub amount_per_share: f64,
    pub shares_held: f64,
    pub total_payment: f64,
}

/// Payments implied by per-share dividend events, given as
/// `(ex_date, amount_per_share)` pairs: shares held at each ex-date ×
/// amount. Events on dates with no shares held yield no payment.
pub fn implied_dividend_payments(txs: &[PortfolioTx], events: &[(String, f64)]) -> Vec<ImpliedDividendPayment> {
    let sorted = sort_transactions(txs);
    events
        .iter()
        .filter_map(|(ex_date, amount)| {
            let shares_held = shares_on_date(&sorted, ex_date);
            (shares_held > 0.0).then(|| ImpliedDividendPayment {
                ex_date: ex_date.clone(),
                amount_per_share: *amount,
                shares_held,
                total_payment: shares_held * amount,
            })
        })
        .collect()
}

/// Full per-symbol position: remaining lots plus the sold-side aggregates,
/// with dividends attributed once (holdings side while shares remain, sold
/// side once fully closed).
#[derive(Debug, Clone)]
pub struct SymbolPosition {
    pub symbol: String,
    pub remaining_shares: f64,
    pub remaining_cost: f64,
    pub native_remaining_cost: f64,
    /// Effective symbol dividends (see symbol_dividends)
    pub dividends: f64,
    /// Realised trading P/L from sales, excluding dividends
    pub sold_trade_pl: f64,
    /// Dividends attributed to the sold side (non-zero only when fully closed)
    pub sold_dividends: f64,
    pub sold_proceeds: f64,
    pub total_sold_qty: f64,
}

impl SymbolPosition {
    /// Sold-side P/L including its dividend share
    pub fn sold_pl(&self) -> f64 {
        self.sold_trade_pl + self.sold_dividends
    }
}

pub fn calc_symbol_position(txs: &[PortfolioTx]) -> SymbolPosition {
    let sorted = sort_transactions(txs);
    let symbol = sorted.first().map(|t| t.symbol.clone()).unwrap_or_default();
    let dividends = symbol_dividends(&sorted);

    let mut lots: Vec<Lot> = Vec::new();
    let mut native_lots: Vec<Lot> = Vec::new();
    let mut sold_trade_pl = 0.0;
    let mut sold_proceeds = 0.0;
    let mut total_sold_qty = 0.0;

    for tx in &sorted {
        match (tx.tx_type, tx.quantity, tx.price) {
            (TxType::Purchase, Some(qty), Some(price)) => {
                let (lot, native_lot) = purchase_lots(tx, qty, price);
                lots.push(lot);
                native_lots.push(native_lot);
            }
            (TxType::Sale, Some(qty), Some(price)) => {
                let cost_basis = apply_fifo_sale(&mut lots, qty);
                apply_fifo_sale(&mut native_lots, qty);
                sold_trade_pl += qty * price - tx.brokerage.unwrap_or(0.0) - cost_basis;
                sold_proceeds += qty * price;
                total_sold_qty += qty;
            }
            _ => {}
        }
    }

    let remaining_shares: f64 = lots.iter().map(|l| l.quantity).sum();
    let remaining_cost = lots.iter().map(|l| l.quantity * l.price).sum();
    let native_remaining_cost = native_lots.iter().map(|l| l.quantity * l.price).sum();
    let sold_dividends = if remaining_shares == 0.0 && total_sold_qty > 0.0 { dividends } else { 0.0 };

    SymbolPosition {
        symbol,
        remaining_shares,
        remaining_cost,
        native_remaining_cost,
        dividends,
        sold_trade_pl,
        sold_dividends,
        sold_proceeds,
        total_sold_qty,
    }
}

/// Group transactions by symbol, preserving first-seen symbol order.
pub fn group_by_symbol(txs: &[PortfolioTx]) -> Vec<(String, Vec<PortfolioTx>)> {
    let mut order: Vec<String> = Vec::new();
    let mut groups: HashMap<String, Vec<PortfolioTx>> = HashMap::new();
    for tx in txs {
        if !groups.contains_key(&tx.symbol) {
            order.push(tx.symbol.clone());
        }
        groups.entry(tx.symbol.clone()).or_default().push(tx.clone());
    }
    order.into_iter().map(|s| { let g = groups.remove(&s).unwrap_or_default(); (s, g) }).collect()
}

/// For each purchase transaction, how many of its shares remain unsold after
/// FIFO matching. Keyed by transaction id; 0 means fully consumed by sales.
pub fn calc_remaining_by_lot(transactions: &[PortfolioTx]) -> HashMap<i64, f64> {
    let mut result: HashMap<i64, f64> = HashMap::new();

    for (_symbol, txs) in group_by_symbol(transactions) {
        let sorted = sort_transactions(&txs);
        let mut lots: Vec<(i64, f64)> = Vec::new();

        for tx in &sorted {
            match (tx.tx_type, tx.quantity) {
                (TxType::Purchase, Some(qty)) => {
                    lots.push((tx.id, qty));
                    result.insert(tx.id, qty);
                }
                (TxType::Sale, Some(qty)) => {
                    let mut remaining = qty;
                    while remaining > 0.0 && !lots.is_empty() {
                        let used = remaining.min(lots[0].1);
                        lots[0].1 -= used;
                        result.insert(lots[0].0, lots[0].1);
                        remaining -= used;
                        if lots[0].1 <= 0.0 {
                            lots.remove(0);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    result
}

/// Symbols with net shares > 0, in first-seen order.
pub fn get_active_holding_symbols(transactions: &[PortfolioTx]) -> Vec<String> {
    let mut order: Vec<String> = Vec::new();
    let mut net: HashMap<String, f64> = HashMap::new();
    for tx in transactions {
        if !net.contains_key(&tx.symbol) {
            order.push(tx.symbol.clone());
        }
        let entry = net.entry(tx.symbol.clone()).or_insert(0.0);
        if let Some(qty) = tx.quantity
            && qty != 0.0 {
                match tx.tx_type {
                    TxType::Purchase => *entry += qty,
                    TxType::Sale => *entry -= qty,
                    _ => {}
                }
            }
    }
    order.into_iter().filter(|s| net.get(s).copied().unwrap_or(0.0) > 0.0).collect()
}

/// Date of the earliest purchase lot that still has unsold shares.
pub fn get_earliest_remaining_purchase_date(transactions: &[PortfolioTx], symbol: &str) -> Option<String> {
    let txs: Vec<PortfolioTx> = transactions.iter().filter(|t| t.symbol == symbol).cloned().collect();
    let sorted = sort_transactions(&txs);
    let mut lots: Vec<(String, f64)> = Vec::new();
    for tx in &sorted {
        match (tx.tx_type, tx.quantity) {
            (TxType::Purchase, Some(qty)) if qty != 0.0 => lots.push((tx.date.clone(), qty)),
            (TxType::Sale, Some(qty)) if qty != 0.0 => {
                let mut remaining = qty;
                while remaining > 0.0 && !lots.is_empty() {
                    let used = remaining.min(lots[0].1);
                    lots[0].1 -= used;
                    remaining -= used;
                    if lots[0].1 <= 0.0 {
                        lots.remove(0);
                    }
                }
            }
            _ => {}
        }
    }
    lots.first().map(|(date, _)| date.clone())
}

/// One realised sale, as shown on the Sold Stocks screen.
#[derive(Debug, Clone)]
pub struct SoldEntry {
    pub symbol: String,
    pub date: String,
    pub quantity: f64,
    pub avg_purchase_price: f64,
    pub sale_price: f64,
    pub brokerage: f64,
    pub dividends: f64,
    pub days_held: i64,
    pub realised_pl: f64,
}

/// Per-sale realised entries for one symbol's transactions. Dividends attach
/// to sales (proportionally by quantity) only once the position is fully
/// closed — while shares remain they belong to the holdings side.
pub fn calc_sold_entries(txs: &[PortfolioTx]) -> Vec<SoldEntry> {
    let sorted = sort_transactions(txs);
    let dividends = symbol_dividends(&sorted);
    let total_sold_qty: f64 = sorted
        .iter()
        .filter(|t| t.tx_type == TxType::Sale)
        .filter_map(|t| t.quantity)
        .sum();

    let mut lots: Vec<(f64, f64, String)> = Vec::new(); // (quantity, cost_per_share, date)
    let mut sales: Vec<SoldEntry> = Vec::new();

    for tx in &sorted {
        match (tx.tx_type, tx.quantity, tx.price) {
            (TxType::Purchase, Some(qty), Some(price)) if qty != 0.0 && price != 0.0 => {
                // Same cost basis the rest of the engine uses: the fee is part
                // of what the shares cost, so a sale is measured against it.
                let (lot, _) = purchase_lots(tx, qty, price);
                lots.push((qty, lot.price, tx.date.clone()));
            }
            (TxType::Sale, Some(qty), Some(price)) if qty != 0.0 && price != 0.0 => {
                let mut remaining = qty;
                let mut cost_basis = 0.0;
                let mut earliest = tx.date.clone();
                while remaining > 0.0 && !lots.is_empty() {
                    let used = remaining.min(lots[0].0);
                    if lots[0].2 < earliest {
                        earliest = lots[0].2.clone();
                    }
                    cost_basis += used * lots[0].1;
                    remaining -= used;
                    lots[0].0 -= used;
                    if lots[0].0 <= 0.0 {
                        lots.remove(0);
                    }
                }
                let brokerage = tx.brokerage.unwrap_or(0.0);
                let sale_proceeds = qty * price - brokerage - cost_basis;
                let days_held = match (
                    NaiveDate::parse_from_str(&tx.date, "%Y-%m-%d"),
                    NaiveDate::parse_from_str(&earliest, "%Y-%m-%d"),
                ) {
                    (Ok(sale_date), Ok(purchase_date)) => (sale_date - purchase_date).num_days(),
                    _ => 0,
                };
                sales.push(SoldEntry {
                    symbol: tx.symbol.clone(),
                    date: tx.date.clone(),
                    quantity: qty,
                    avg_purchase_price: if qty > 0.0 { cost_basis / qty } else { 0.0 },
                    sale_price: price,
                    brokerage,
                    dividends: 0.0,
                    days_held,
                    realised_pl: sale_proceeds,
                });
            }
            _ => {}
        }
    }

    let remaining_shares: f64 = lots.iter().map(|l| l.0).sum();
    if remaining_shares == 0.0 && total_sold_qty > 0.0 && dividends > 0.0 {
        for sale in &mut sales {
            let share = (sale.quantity / total_sold_qty) * dividends;
            sale.dividends = share;
            sale.realised_pl += share;
        }
    }

    sales
}

// ---------------------------------------------------------------------------
// Tests — ported from web/src/utils/fifo.test.ts so the Rust engine is
// provably identical to the TypeScript engine it replaces.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    fn day(date: &str, stocks: f64, cash: f64, flow: f64) -> DailyValue {
        DailyValue { date: date.to_string(), stocks, cash, flow }
    }

    fn tx(id: i64, kind: &str, date: &str, quantity: f64, price: f64, amount: Option<f64>) -> PortfolioTx {
        PortfolioTx {
            id,
            symbol: "TST.AX".to_string(),
            tx_type: TxType::parse(kind),
            date: date.to_string(),
            quantity: Some(quantity),
            price: Some(price),
            native_price: Some(price),
            amount,
            brokerage: None,
            dividends_total: 0.0,
        }
    }

    /// Each sale is costed against the lots it actually consumed, so a symbol
    /// bought twice at different prices and sold twice reports two different
    /// costs rather than one blended average.
    #[test]
    fn sale_costs_follow_the_fifo_queue() {
        let txs = vec![
            tx(1, "purchase", "2025-01-01", 100.0, 10.0, None),
            tx(2, "purchase", "2025-02-01", 100.0, 20.0, None),
            tx(3, "sale", "2025-03-01", 100.0, 15.0, None), // eats the $10 lot
            tx(4, "sale", "2025-04-01", 100.0, 25.0, None), // eats the $20 lot
        ];
        let sales = sale_costs(&txs);

        assert_eq!(sales.len(), 2);
        assert_eq!(sales[0].date, "2025-03-01");
        assert_eq!(sales[0].native_cost_per_share, Some(10.0));
        assert_eq!(sales[0].native_price, 15.0);
        assert_eq!(sales[1].native_cost_per_share, Some(20.0), "not the 15.0 blended average");
    }

    /// A sale spanning two lots is costed on the weighted blend of both, which
    /// is what FIFO actually consumed.
    #[test]
    fn a_sale_spanning_two_lots_blends_them() {
        let txs = vec![
            tx(1, "purchase", "2025-01-01", 100.0, 10.0, None),
            tx(2, "purchase", "2025-02-01", 100.0, 20.0, None),
            tx(3, "sale", "2025-03-01", 150.0, 30.0, None),
        ];
        let sales = sale_costs(&txs);
        // 100 × $10 + 50 × $20 = $2,000 over 150 shares.
        assert!((sales[0].native_cost_per_share.unwrap() - 13.333_333_333).abs() < 1e-6);
    }

    /// Brokerage on the buy is part of what the shares cost, so it has to reach
    /// the per-sale figure the same way it reaches the holdings screens.
    #[test]
    fn sale_costs_include_purchase_brokerage() {
        let mut buy = tx(1, "purchase", "2025-01-01", 100.0, 10.0, None);
        buy.brokerage = Some(50.0);
        let sales = sale_costs(&[buy, tx(2, "sale", "2025-03-01", 100.0, 15.0, None)]);

        assert_eq!(sales[0].native_cost_per_share, Some(10.5), "$10 plus 50c a share of fee");
    }

    /// A sale recorded without its purchase consumes no lot. That is a gap in
    /// the records, not a free acquisition, so it must not read as zero cost.
    #[test]
    fn a_sale_with_no_matching_purchase_has_no_cost() {
        let sales = sale_costs(&[tx(1, "sale", "2025-03-01", 100.0, 15.0, None)]);

        assert_eq!(sales.len(), 1);
        assert_eq!(sales[0].native_cost_per_share, None);
    }

    /// Brokerage on a buy is part of what the shares cost. Leaving it out
    /// overstated profit by the fee — and asymmetrically, since a sale's fee
    /// was always deducted from its proceeds.
    #[test]
    fn purchase_brokerage_lands_in_the_cost_basis() {
        let mut buy = tx(1, "purchase", "2025-01-10", 100.0, 10.0, None);
        buy.brokerage = Some(20.0);
        let summary = calc_symbol_summary(&[buy]);

        assert_eq!(summary.remaining_shares, 100.0);
        assert!((summary.remaining_cost - 1020.0).abs() < 1e-9, "1000 paid for shares plus a 20 fee");
        // Per share, the fee spreads across the parcel.
        assert!((summary.lots[0].price - 10.2).abs() < 1e-9);
    }

    /// The buy and sell fees now bite symmetrically: a round trip at an
    /// unchanged price is a loss of both fees, not of one.
    #[test]
    fn a_round_trip_at_the_same_price_loses_both_fees() {
        let mut buy = tx(1, "purchase", "2025-01-10", 100.0, 10.0, None);
        buy.brokerage = Some(20.0);
        let mut sell = tx(2, "sale", "2025-06-10", 100.0, 10.0, None);
        sell.brokerage = Some(15.0);

        let summary = calc_symbol_summary(&[buy, sell]);
        assert!((summary.realised_pl - -35.0).abs() < 1e-9, "expected -35, got {}", summary.realised_pl);
        assert_eq!(summary.remaining_shares, 0.0);
    }

    /// FIFO consumes the fee with the shares it was paid on, so a partial sale
    /// carries only its share of it.
    #[test]
    fn brokerage_follows_its_own_lot_through_a_partial_sale() {
        let mut buy = tx(1, "purchase", "2025-01-10", 100.0, 10.0, None);
        buy.brokerage = Some(20.0);
        let sell = tx(2, "sale", "2025-06-10", 40.0, 12.0, None);

        let summary = calc_symbol_summary(&[buy, sell]);
        // 40 sold at 12 against a 10.20 basis; 60 left at 10.20.
        assert!((summary.realised_pl - 72.0).abs() < 1e-9, "expected 72, got {}", summary.realised_pl);
        assert!((summary.remaining_cost - 612.0).abs() < 1e-9);
    }

    /// The fee is recorded in AUD, so the native lot has to convert it rather
    /// than adding dollars to a price quoted in another currency.
    #[test]
    fn native_cost_converts_the_fee_at_the_trades_own_rate() {
        let mut buy = tx(1, "purchase", "2025-01-10", 10.0, 150.0, None);
        buy.native_price = Some(100.0); // 1.5 AUD per unit of native currency
        buy.brokerage = Some(30.0); // AUD
        let summary = calc_symbol_summary(&[buy]);

        assert!((summary.remaining_cost - 1530.0).abs() < 1e-9, "AUD: 1500 + 30");
        // The same fee is 20 in native terms, so 2 per share on top of 100.
        assert!((summary.native_remaining_cost - 1020.0).abs() < 1e-9,
            "native: 1000 + 20, got {}", summary.native_remaining_cost);
    }

    /// A transaction with no fee recorded must be unchanged by any of this.
    #[test]
    fn a_purchase_without_brokerage_is_unaffected() {
        let summary = calc_symbol_summary(&[tx(1, "purchase", "2025-01-10", 100.0, 10.0, None)]);
        assert!((summary.remaining_cost - 1000.0).abs() < 1e-9);
    }

    /// The case this exists for: a decades-old holding whose original cost
    /// tells you nothing about how it has done lately.
    #[test]
    fn rebase_values_held_shares_at_the_baseline_price() {
        // 50 shares bought in 2001 at 7.34; baseline 2025 price 281.18.
        let txs = vec![tx(1, "purchase", "2001-07-01", 50.0, 7.34, None)];
        let basis = rebase_at(&txs, 50.0, "2025-01-01", 281.18);
        assert!((basis.cost - 14059.0).abs() < 1e-6, "50 × 281.18, not 50 × 7.34");
        assert_eq!(basis.dividends, 0.0);
    }

    /// Dividends paid before the baseline belong to the earlier period —
    /// counting them would credit the new one with income it did not earn.
    #[test]
    fn rebase_counts_only_dividends_from_the_baseline_on() {
        let txs = vec![
            tx(1, "purchase", "2001-07-01", 50.0, 7.34, None),
            tx(2, "dividend", "2024-09-09", 50.0, 0.0, Some(108.73)),
            tx(3, "dividend", "2025-03-10", 50.0, 0.0, Some(103.64)),
            tx(4, "dividend", "2026-03-10", 50.0, 0.0, Some(90.49)),
        ];
        let basis = rebase_at(&txs, 50.0, "2025-01-01", 281.18);
        assert!((basis.dividends - 194.13).abs() < 1e-6, "the 2024 payment is excluded");
    }

    /// A share bought after the baseline was never worth the baseline price —
    /// for it, the purchase *is* the baseline.
    #[test]
    fn rebase_values_later_purchases_at_what_they_cost() {
        let txs = vec![
            tx(1, "purchase", "2001-07-01", 50.0, 7.34, None),
            tx(2, "purchase", "2025-06-01", 10.0, 300.0, None),
        ];
        let basis = rebase_at(&txs, 60.0, "2025-01-01", 281.18);
        // 50 × 281.18 baseline + 10 × 300 actual
        assert!((basis.cost - (14059.0 + 3000.0)).abs() < 1e-6);
    }

    /// A sale after the baseline eats into the shares held at it, so the basis
    /// has to follow what is actually left rather than the original count.
    #[test]
    fn rebase_shrinks_with_a_later_sale() {
        let txs = vec![
            tx(1, "purchase", "2001-07-01", 50.0, 7.34, None),
            tx(2, "sale", "2025-06-01", 20.0, 300.0, None),
        ];
        let basis = rebase_at(&txs, 30.0, "2025-01-01", 281.18);
        assert!((basis.cost - 30.0 * 281.18).abs() < 1e-6, "only the 30 still held are valued");
    }

    /// A holding bought entirely after the baseline has no baseline shares, so
    /// its rebased cost is simply what it cost.
    #[test]
    fn rebase_of_a_wholly_later_holding_is_its_actual_cost() {
        let txs = vec![tx(1, "purchase", "2025-06-01", 10.0, 42.0, None)];
        let basis = rebase_at(&txs, 10.0, "2025-01-01", 281.18);
        assert!((basis.cost - 420.0).abs() < 1e-6);
    }

    #[test]
    fn twr_measures_pure_market_movement() {
        // 1000 → 1100 with no flows is +10%
        let series = vec![day("2026-01-01", 1000.0, 0.0, 0.0), day("2026-01-02", 1100.0, 0.0, 0.0)];
        assert!((time_weighted_return(&series).unwrap() - 0.10).abs() < 1e-12);
    }

    /// The requirement this whole design exists for: a contribution raises the
    /// balance without registering as growth.
    #[test]
    fn twr_ignores_contributions() {
        let series = vec![
            day("2026-01-01", 0.0, 1000.0, 0.0),
            day("2026-01-02", 0.0, 2000.0, 1000.0), // deposit only
        ];
        assert!(time_weighted_return(&series).unwrap().abs() < 1e-12, "a deposit is not a gain");

        // A deposit landing the same day as a real gain leaves only the gain
        let mixed = vec![
            day("2026-01-01", 0.0, 1000.0, 0.0),
            day("2026-01-02", 0.0, 2100.0, 1000.0), // +100 earned, +1000 added
        ];
        assert!((time_weighted_return(&mixed).unwrap() - 0.10).abs() < 1e-12);
    }

    #[test]
    fn twr_ignores_withdrawals() {
        let series = vec![
            day("2026-01-01", 0.0, 1000.0, 0.0),
            day("2026-01-02", 0.0, 400.0, -600.0),
        ];
        assert!(time_weighted_return(&series).unwrap().abs() < 1e-12);
    }

    /// Chaining is what makes the result independent of when money arrived:
    /// the same market moves give the same answer whatever the deposits.
    #[test]
    fn twr_chains_daily_and_is_unaffected_by_contribution_timing() {
        let quiet = vec![
            day("2026-01-01", 1000.0, 0.0, 0.0),
            day("2026-01-02", 1100.0, 0.0, 0.0), // +10%
            day("2026-01-03", 990.0, 0.0, 0.0),  // −10%
        ];
        let expected = 1.10 * 0.90 - 1.0; // −1%
        assert!((time_weighted_return(&quiet).unwrap() - expected).abs() < 1e-12);

        // Same moves, but $5,000 arrives before the down day
        let with_deposit = vec![
            day("2026-01-01", 1000.0, 0.0, 0.0),
            day("2026-01-02", 1100.0, 5000.0, 5000.0),
            day("2026-01-03", 990.0, 4500.0, 0.0), // both fall 10%
        ];
        assert!((time_weighted_return(&with_deposit).unwrap() - expected).abs() < 1e-12);
    }

    #[test]
    fn twr_is_none_until_there_is_capital_to_measure() {
        assert_eq!(time_weighted_return(&[]), None);
        assert_eq!(time_weighted_return(&[day("2026-01-01", 0.0, 100.0, 100.0)]), None);
        // The funding day itself has no prior capital, so nothing is chained
        let opening = vec![day("2026-01-01", 0.0, 0.0, 0.0), day("2026-01-02", 0.0, 1000.0, 1000.0)];
        assert_eq!(time_weighted_return(&opening), None);
    }

    #[test]
    fn net_contributions_sums_flows_both_ways() {
        let series = vec![
            day("2026-01-01", 0.0, 1000.0, 1000.0),
            day("2026-02-01", 0.0, 2000.0, 1000.0),
            day("2026-03-01", 0.0, 1500.0, -500.0),
        ];
        assert!((net_contributions(&series) - 1500.0).abs() < 1e-12);
    }

    fn make_tx(id: i64, tx_type: &str, date: &str, quantity: Option<f64>, price: Option<f64>) -> PortfolioTx {
        PortfolioTx {
            id,
            symbol: "TST.AX".to_string(),
            tx_type: TxType::parse(tx_type),
            date: date.to_string(),
            quantity,
            price,
            native_price: None,
            amount: None,
            brokerage: None,
            dividends_total: 0.0,
        }
    }

    fn with_symbol(mut tx: PortfolioTx, symbol: &str) -> PortfolioTx {
        tx.symbol = symbol.to_string();
        tx
    }

    fn with_brokerage(mut tx: PortfolioTx, brokerage: f64) -> PortfolioTx {
        tx.brokerage = Some(brokerage);
        tx
    }

    fn with_dividends_total(mut tx: PortfolioTx, total: f64) -> PortfolioTx {
        tx.dividends_total = total;
        tx
    }

    fn with_amount(mut tx: PortfolioTx, amount: f64) -> PortfolioTx {
        tx.amount = Some(amount);
        tx
    }

    fn with_fx(mut tx: PortfolioTx, native_price: f64) -> PortfolioTx {
        tx.native_price = Some(native_price);
        tx
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 0.005
    }

    // -- sortTransactions ---------------------------------------------------

    #[test]
    fn sorts_by_date_ascending() {
        let txs = vec![
            make_tx(1, "purchase", "2024-06-01", Some(100.0), Some(10.0)),
            make_tx(2, "purchase", "2024-01-01", Some(50.0), Some(9.0)),
        ];
        let sorted = sort_transactions(&txs);
        assert_eq!(sorted[0].date, "2024-01-01");
    }

    #[test]
    fn breaks_ties_by_id() {
        let txs = vec![
            make_tx(3, "sale", "2024-01-01", Some(50.0), Some(12.0)),
            make_tx(1, "purchase", "2024-01-01", Some(100.0), Some(10.0)),
        ];
        let sorted = sort_transactions(&txs);
        assert_eq!(sorted[0].id, 1);
    }

    // -- applyFifoSale ------------------------------------------------------

    #[test]
    fn consumes_single_lot_fully() {
        let mut lots = vec![Lot { quantity: 100.0, price: 10.0 }];
        let cost_basis = apply_fifo_sale(&mut lots, 100.0);
        assert_eq!(cost_basis, 1000.0);
        assert!(lots.is_empty());
    }

    #[test]
    fn consumes_single_lot_partially() {
        let mut lots = vec![Lot { quantity: 100.0, price: 10.0 }];
        let cost_basis = apply_fifo_sale(&mut lots, 40.0);
        assert_eq!(cost_basis, 400.0);
        assert_eq!(lots[0].quantity, 60.0);
    }

    #[test]
    fn consumes_across_multiple_lots_in_order() {
        let mut lots = vec![Lot { quantity: 50.0, price: 10.0 }, Lot { quantity: 50.0, price: 20.0 }];
        let cost_basis = apply_fifo_sale(&mut lots, 75.0);
        // 50 @ $10 + 25 @ $20 = $1000
        assert_eq!(cost_basis, 1000.0);
        assert_eq!(lots.len(), 1);
        assert_eq!(lots[0].quantity, 25.0);
        assert_eq!(lots[0].price, 20.0);
    }

    #[test]
    fn oversell_consumes_what_is_available() {
        let mut lots = vec![Lot { quantity: 30.0, price: 10.0 }];
        let cost_basis = apply_fifo_sale(&mut lots, 50.0);
        assert_eq!(cost_basis, 300.0); // only 30 available
        assert!(lots.is_empty());
    }

    // -- calcSymbolSummary --------------------------------------------------

    #[test]
    fn summary_single_purchase_no_sales() {
        let txs = vec![make_tx(1, "purchase", "2024-01-01", Some(100.0), Some(10.0))];
        let s = calc_symbol_summary(&txs);
        assert_eq!(s.remaining_shares, 100.0);
        assert_eq!(s.remaining_cost, 1000.0);
        assert_eq!(s.realised_pl, 0.0);
        assert_eq!(s.total_sold_qty, 0.0);
    }

    #[test]
    fn summary_partial_sale() {
        let txs = vec![
            make_tx(1, "purchase", "2024-01-01", Some(100.0), Some(10.0)),
            make_tx(2, "sale", "2024-06-01", Some(40.0), Some(15.0)),
        ];
        let s = calc_symbol_summary(&txs);
        assert_eq!(s.remaining_shares, 60.0);
        assert!(close(s.remaining_cost, 600.0));
        // proceeds = 40 * 15 = 600; cost = 40 * 10 = 400; P/L = 200
        assert!(close(s.realised_pl, 200.0));
    }

    #[test]
    fn summary_full_sale() {
        let txs = vec![
            make_tx(1, "purchase", "2024-01-01", Some(100.0), Some(10.0)),
            make_tx(2, "sale", "2024-06-01", Some(100.0), Some(15.0)),
        ];
        let s = calc_symbol_summary(&txs);
        assert_eq!(s.remaining_shares, 0.0);
        assert_eq!(s.remaining_cost, 0.0);
        assert!(close(s.realised_pl, 500.0));
    }

    #[test]
    fn summary_full_sale_at_loss() {
        let txs = vec![
            make_tx(1, "purchase", "2024-01-01", Some(100.0), Some(10.0)),
            make_tx(2, "sale", "2024-06-01", Some(100.0), Some(7.0)),
        ];
        let s = calc_symbol_summary(&txs);
        assert!(close(s.realised_pl, -300.0));
    }

    #[test]
    fn summary_brokerage_reduces_realised_pl() {
        let txs = vec![
            make_tx(1, "purchase", "2024-01-01", Some(100.0), Some(10.0)),
            with_brokerage(make_tx(2, "sale", "2024-06-01", Some(100.0), Some(15.0)), 9.95),
        ];
        let s = calc_symbol_summary(&txs);
        assert!(close(s.realised_pl, 500.0 - 9.95));
    }

    #[test]
    fn summary_multiple_purchases_fifo_order() {
        let txs = vec![
            make_tx(1, "purchase", "2024-01-01", Some(50.0), Some(10.0)),
            make_tx(2, "purchase", "2024-03-01", Some(50.0), Some(20.0)),
            make_tx(3, "sale", "2024-06-01", Some(50.0), Some(15.0)),
        ];
        let s = calc_symbol_summary(&txs);
        // cost basis = 50 * 10 = 500; proceeds = 50 * 15 = 750; P/L = 250
        assert!(close(s.realised_pl, 250.0));
        assert_eq!(s.remaining_shares, 50.0);
        assert!(close(s.remaining_cost, 1000.0));
    }

    #[test]
    fn summary_multiple_sales_consume_sequentially() {
        let txs = vec![
            make_tx(1, "purchase", "2024-01-01", Some(100.0), Some(10.0)),
            make_tx(2, "sale", "2024-04-01", Some(60.0), Some(15.0)),
            make_tx(3, "sale", "2024-08-01", Some(40.0), Some(20.0)),
        ];
        let s = calc_symbol_summary(&txs);
        assert_eq!(s.remaining_shares, 0.0);
        // Sale 1: 60*15 - 60*10 = 300; Sale 2: 40*20 - 40*10 = 400
        assert!(close(s.realised_pl, 700.0));
    }

    #[test]
    fn summary_reads_dividends_total() {
        let txs = vec![with_dividends_total(make_tx(1, "purchase", "2024-01-01", Some(100.0), Some(10.0)), 55.5)];
        let s = calc_symbol_summary(&txs);
        assert_eq!(s.dividends_total, 55.5);
    }

    // -- calcPortfolioPL ----------------------------------------------------

    #[test]
    fn an_open_position_carries_its_cost_and_nothing_realised() {
        let txs = vec![make_tx(1, "purchase", "2024-01-01", Some(100.0), Some(10.0))];
        let pos = calc_symbol_position(&txs);
        assert_eq!(pos.remaining_shares, 100.0);
        assert!(close(pos.remaining_cost, 1000.0));
        assert_eq!(pos.sold_pl(), 0.0);
        // What the portfolio total then makes of it: 100 × 15 − 1000.
        assert!(close(pos.remaining_shares * 15.0 - pos.remaining_cost + pos.dividends, 500.0));
    }

    /// An unpriced holding is valued at zero by the caller, so its whole cost
    /// shows as a loss. The position still reports that cost in full.
    #[test]
    fn an_unpriced_position_still_reports_its_cost() {
        let txs = vec![make_tx(1, "purchase", "2024-01-01", Some(100.0), Some(10.0))];
        let pos = calc_symbol_position(&txs);
        assert!(close(pos.remaining_cost, 1000.0));
        assert!(close(0.0 - pos.remaining_cost + pos.dividends, -1000.0));
    }

    #[test]
    fn a_closed_position_is_all_realised_and_holds_nothing() {
        let txs = vec![
            make_tx(1, "purchase", "2024-01-01", Some(100.0), Some(10.0)),
            make_tx(2, "sale", "2024-06-01", Some(100.0), Some(15.0)),
        ];
        let pos = calc_symbol_position(&txs);
        assert!(close(pos.sold_pl(), 500.0));
        assert_eq!(pos.remaining_shares, 0.0, "nothing left to count toward holdings");
        assert_eq!(pos.remaining_cost, 0.0);
    }

    #[test]
    fn a_partial_sale_splits_realised_from_unrealised() {
        let txs = vec![
            make_tx(1, "purchase", "2024-01-01", Some(100.0), Some(10.0)),
            make_tx(2, "sale", "2024-06-01", Some(40.0), Some(15.0)),
        ];
        let pos = calc_symbol_position(&txs);
        // 60 remain at a $600 cost; the 40 sold returned 600 against a 400 cost.
        assert_eq!(pos.remaining_shares, 60.0);
        assert!(close(pos.remaining_cost, 600.0));
        assert!(close(pos.sold_pl(), 200.0));
        // At $18 the two sides come to 480 + 200.
        assert!(close(pos.remaining_shares * 18.0 - pos.remaining_cost + pos.sold_pl(), 680.0));
    }

    #[test]
    fn dividends_belong_to_the_holding_while_it_is_open() {
        let txs = vec![with_dividends_total(make_tx(1, "purchase", "2024-01-01", Some(100.0), Some(10.0)), 50.0)];
        let pos = calc_symbol_position(&txs);
        assert!(close(pos.dividends, 50.0));
        assert_eq!(pos.sold_dividends, 0.0, "nothing has been sold, so nothing is attributed to the sold side");
    }

    #[test]
    fn dividends_move_to_the_sold_side_once_the_position_closes() {
        let txs = vec![
            with_dividends_total(make_tx(1, "purchase", "2024-01-01", Some(100.0), Some(10.0)), 60.0),
            make_tx(2, "sale", "2024-06-01", Some(100.0), Some(10.0)), // sold at cost
        ];
        let pos = calc_symbol_position(&txs);
        assert!(close(pos.sold_dividends, 60.0));
        assert!(close(pos.sold_pl(), 60.0), "sold at cost, so the dividends are the whole return");
    }

    /// The double-count trap: a partly sold holding must not have its dividends
    /// attributed to both sides.
    #[test]
    fn a_partial_sale_does_not_count_dividends_twice() {
        let txs = vec![
            with_dividends_total(make_tx(1, "purchase", "2024-01-01", Some(100.0), Some(10.0)), 50.0),
            make_tx(2, "sale", "2024-06-01", Some(40.0), Some(10.0)), // sold at cost
        ];
        let pos = calc_symbol_position(&txs);
        assert!(close(pos.dividends, 50.0));
        assert_eq!(pos.sold_dividends, 0.0, "still open, so none of it is realised");
        // Sold at cost and held at cost: the whole return is the $50, once.
        assert!(close(pos.remaining_shares * 10.0 - pos.remaining_cost + pos.dividends + pos.sold_pl(), 50.0));
    }

    /// Each symbol is positioned independently; the endpoint sums them. A gain
    /// on one and a loss on the other must net, not cancel out inside a symbol.
    #[test]
    fn positions_are_independent_per_symbol() {
        let aaa = calc_symbol_position(&[with_symbol(
            make_tx(1, "purchase", "2024-01-01", Some(100.0), Some(10.0)), "AAA.AX")]);
        let bbb = calc_symbol_position(&[with_symbol(
            make_tx(2, "purchase", "2024-01-01", Some(50.0), Some(20.0)), "BBB.AX")]);

        let aaa_pl = aaa.remaining_shares * 12.0 - aaa.remaining_cost;
        let bbb_pl = bbb.remaining_shares * 18.0 - bbb.remaining_cost;
        assert!(close(aaa_pl, 200.0));
        assert!(close(bbb_pl, -100.0));
        assert!(close(aaa_pl + bbb_pl, 100.0));
    }

    // -- calcRemainingByLot -------------------------------------------------

    #[test]
    fn remaining_single_purchase() {
        let txs = vec![make_tx(1, "purchase", "2024-01-01", Some(100.0), Some(10.0))];
        let r = calc_remaining_by_lot(&txs);
        assert_eq!(r[&1], 100.0);
    }

    #[test]
    fn remaining_partial_sale_reduces_earliest_lot() {
        let txs = vec![
            make_tx(1, "purchase", "2024-01-01", Some(100.0), Some(10.0)),
            make_tx(2, "sale", "2024-06-01", Some(40.0), Some(15.0)),
        ];
        let r = calc_remaining_by_lot(&txs);
        assert_eq!(r[&1], 60.0);
    }

    #[test]
    fn remaining_full_sale_sets_zero() {
        let txs = vec![
            make_tx(1, "purchase", "2024-01-01", Some(100.0), Some(10.0)),
            make_tx(2, "sale", "2024-06-01", Some(100.0), Some(15.0)),
        ];
        let r = calc_remaining_by_lot(&txs);
        assert_eq!(r[&1], 0.0);
    }

    #[test]
    fn remaining_sale_spans_two_lots() {
        let txs = vec![
            make_tx(1, "purchase", "2024-01-01", Some(60.0), Some(10.0)),
            make_tx(2, "purchase", "2024-03-01", Some(60.0), Some(20.0)),
            make_tx(3, "sale", "2024-06-01", Some(80.0), Some(25.0)),
        ];
        let r = calc_remaining_by_lot(&txs);
        assert_eq!(r[&1], 0.0);
        assert_eq!(r[&2], 40.0);
    }

    #[test]
    fn remaining_ignores_dividend_transactions() {
        let txs = vec![
            make_tx(1, "purchase", "2024-01-01", Some(100.0), Some(10.0)),
            with_amount(make_tx(2, "dividend", "2024-06-01", None, None), 50.0),
        ];
        let r = calc_remaining_by_lot(&txs);
        assert_eq!(r[&1], 100.0);
        assert!(!r.contains_key(&2));
    }

    #[test]
    fn remaining_multiple_symbols_independent() {
        let txs = vec![
            with_symbol(make_tx(1, "purchase", "2024-01-01", Some(100.0), Some(10.0)), "AAA.AX"),
            with_symbol(make_tx(2, "sale", "2024-06-01", Some(100.0), Some(15.0)), "AAA.AX"),
            with_symbol(make_tx(3, "purchase", "2024-01-01", Some(50.0), Some(20.0)), "BBB.AX"),
        ];
        let r = calc_remaining_by_lot(&txs);
        assert_eq!(r[&1], 0.0);
        assert_eq!(r[&3], 50.0);
    }

    // -- International (USD) — the SPCX regression cases ---------------------

    fn spcx_purchase(id: i64, date: &str, qty: f64, aud_price: f64, usd_price: f64) -> PortfolioTx {
        with_fx(with_symbol(make_tx(id, "purchase", date, Some(qty), Some(aud_price)), "SPCX"), usd_price)
    }

    fn spcx_sale(id: i64, date: &str, qty: f64, aud_price: f64, usd_price: f64) -> PortfolioTx {
        with_fx(with_symbol(make_tx(id, "sale", date, Some(qty), Some(aud_price)), "SPCX"), usd_price)
    }

    #[test]
    fn usd_purchase_has_positive_remaining() {
        let txs = vec![spcx_purchase(1, "2026-01-15", 50.0, 2.244, 1.50)];
        let r = calc_remaining_by_lot(&txs);
        assert_eq!(r[&1], 50.0);
    }

    #[test]
    fn usd_partial_sale_remaining_positive() {
        let txs = vec![
            spcx_purchase(1, "2026-01-15", 100.0, 2.244, 1.50),
            spcx_sale(2, "2026-06-01", 40.0, 2.80, 1.87),
        ];
        let r = calc_remaining_by_lot(&txs);
        assert_eq!(r[&1], 60.0);
    }

    #[test]
    fn usd_full_sale_remaining_zero() {
        let txs = vec![
            spcx_purchase(1, "2026-01-15", 50.0, 2.244, 1.50),
            spcx_sale(2, "2026-06-01", 50.0, 2.80, 1.87),
        ];
        let r = calc_remaining_by_lot(&txs);
        assert_eq!(r[&1], 0.0);
    }

    #[test]
    fn usd_summary_uses_aud_prices() {
        let txs = vec![
            spcx_purchase(1, "2026-01-15", 100.0, 2.244, 1.50),
            spcx_sale(2, "2026-06-01", 100.0, 2.80, 1.87),
        ];
        let s = calc_symbol_summary(&txs);
        // P/L = 100 * (2.80 - 2.244) = 55.60 AUD
        assert!(close(s.realised_pl, 55.60));
        assert_eq!(s.remaining_shares, 0.0);
    }

    #[test]
    fn a_usd_holding_positions_on_its_aud_cost() {
        let pos = calc_symbol_position(&[spcx_purchase(1, "2026-01-15", 50.0, 2.244, 1.50)]);
        assert_eq!(pos.remaining_shares, 50.0);
        assert!(close(pos.remaining_cost, 50.0 * 2.244), "cost is the AUD figure, not the USD one");
        assert!(close(pos.remaining_shares * 2.80 - pos.remaining_cost, 27.80));
    }

    /// A foreign and a domestic holding are positioned the same way, because
    /// both arrive already converted to AUD.
    #[test]
    fn usd_and_aud_positions_are_measured_alike() {
        let spcx = calc_symbol_position(&[spcx_purchase(1, "2026-01-15", 50.0, 2.244, 1.50)]);
        let cba = calc_symbol_position(&[with_symbol(
            make_tx(2, "purchase", "2026-02-01", Some(100.0), Some(45.0)), "CBA.AX")]);

        let spcx_pl = spcx.remaining_shares * 2.80 - spcx.remaining_cost;
        let cba_pl = cba.remaining_shares * 48.0 - cba.remaining_cost;
        assert!(close(spcx_pl, 27.80));
        assert!(close(cba_pl, 300.0));
        assert!(close(spcx_pl + cba_pl, 327.80));
    }

    // -- Full-dataset regression (SPCX missing from Active Holdings) ---------

    fn real_transactions() -> Vec<PortfolioTx> {
        fn t(id: i64, sym: &str, ty: &str, date: &str, qty: f64, price: f64) -> PortfolioTx {
            with_symbol(make_tx(id, ty, date, Some(qty), Some(price)), sym)
        }
        vec![
            t(2, "GMG.AX", "purchase", "2025-07-25", 45.0, 34.75),
            t(3, "AX1.AX", "purchase", "2025-07-21", 1000.0, 1.48),
            t(4, "PWH.AX", "purchase", "2025-01-07", 260.0, 7.8),
            t(5, "ADH.AX", "purchase", "2025-04-04", 750.0, 2.0),
            t(6, "COH.AX", "purchase", "2025-01-07", 5.0, 302.5),
            t(7, "DMP.AX", "purchase", "2025-01-08", 50.0, 28.95),
            t(8, "MTO.AX", "purchase", "2025-11-07", 400.0, 3.64),
            t(9, "NHF.AX", "purchase", "2025-01-09", 270.0, 5.5),
            t(10, "SIQ.AX", "purchase", "2025-03-17", 200.0, 6.95),
            t(11, "SOL.AX", "purchase", "2026-01-02", 156.0, 34.31),
            t(12, "GMG.AX", "sale", "2026-05-26", 45.0, 28.77),
            t(13, "AX1.AX", "sale", "2026-05-25", 1000.0, 0.56),
            t(14, "NDQ.AX", "purchase", "2025-01-02", 60.0, 50.52),
            t(15, "IPG.AX", "purchase", "2025-01-07", 520.0, 3.85),
            t(16, "CRYP.AX", "purchase", "2025-01-07", 190.0, 7.96),
            t(17, "CCLD.AX", "purchase", "2025-01-07", 100.0, 15.38),
            t(18, "MOAT.AX", "purchase", "2025-01-07", 12.0, 131.5),
            t(19, "HACK.AX", "purchase", "2025-01-07", 110.0, 14.1),
            t(20, "VAS.AX", "purchase", "2025-01-08", 40.0, 101.96),
            t(21, "VTS.AX", "purchase", "2025-01-08", 7.0, 467.7),
            t(22, "TWE.AX", "purchase", "2025-01-08", 140.0, 11.0),
            t(23, "APE.AX", "purchase", "2025-01-08", 160.0, 12.342),
            t(24, "CTD.AX", "purchase", "2025-01-08", 120.0, 12.93),
            t(25, "VSO.AX", "purchase", "2025-01-09", 15.0, 67.3),
            t(26, "NCK.AX", "purchase", "2025-01-13", 100.0, 14.99),
            t(27, "SUL.AX", "purchase", "2025-01-17", 100.0, 15.2),
            t(28, "SUL.AX", "purchase", "2025-02-28", 100.0, 14.3),
            t(29, "ELD.AX", "purchase", "2025-03-19", 200.0, 6.9),
            t(30, "JLG.AX", "purchase", "2025-03-28", 700.0, 2.25),
            t(31, "SRV.AX", "purchase", "2025-04-04", 300.0, 5.3),
            t(32, "IVV.AX", "purchase", "2025-04-09", 30.0, 54.5),
            t(33, "QLTY.AX", "purchase", "2025-04-09", 50.0, 28.3),
            t(34, "XRF.AX", "purchase", "2025-07-01", 850.0, 1.78),
            t(35, "SKS.AX", "purchase", "2025-10-15", 350.0, 4.2),
            t(36, "SKS.AX", "purchase", "2025-11-10", 450.0, 3.37),
            t(37, "NUGG.AX", "purchase", "2026-02-02", 25.0, 68.0),
            t(38, "VAE.AX", "purchase", "2026-02-02", 20.0, 97.5),
            t(39, "ETPMPM.AX", "purchase", "2026-03-09", 3.0, 471.0),
            t(40, "DTEC.AX", "purchase", "2026-03-11", 75.0, 19.2),
            t(41, "NUGG.AX", "purchase", "2026-04-20", 25.0, 66.0),
            t(42, "NXT.AX", "purchase", "2026-05-19", 100.0, 14.5),
            t(43, "JLG.AX", "sale", "2025-08-01", 700.0, 3.91),
            t(44, "ELD.AX", "sale", "2026-05-18", 200.0, 6.0),
            t(45, "SKS.AX", "sale", "2026-05-19", 800.0, 7.88385),
            t(46, "APE.AX", "sale", "2026-05-20", 160.0, 21.816563),
            t(47, "PWH.AX", "sale", "2026-05-20", 260.0, 6.18),
            t(48, "IPG.AX", "sale", "2026-05-21", 520.0, 5.67),
            t(49, "SRV.AX", "sale", "2026-05-21", 300.0, 6.18),
            t(50, "NCK.AX", "sale", "2026-05-22", 1000.0, 1.32),
            t(51, "SUL.AX", "sale", "2026-05-25", 200.0, 11.091),
            t(52, "XRF.AX", "sale", "2026-05-26", 850.0, 1.78),
            t(53, "TWE.AX", "sale", "2026-06-01", 400.0, 4.200025),
            t(54, "NXT.AX", "sale", "2026-06-09", 100.0, 15.065),
            t(55, "IVV.AX", "sale", "2026-06-12", 30.0, 70.08),
            t(56, "DMP.AX", "sale", "2026-06-12", 50.0, 15.98),
            t(57, "COH.AX", "sale", "2026-06-12", 5.0, 103.88),
            t(58, "MTO.AX", "sale", "2026-06-12", 400.0, 2.44),
            with_fx(t(59, "TSM", "purchase", "2026-06-09", 3.0, 612.022028113604), 431.53),
            with_fx(t(60, "TXG", "purchase", "2026-06-09", 40.0, 43.3995566082001), 30.59),
            {
                let mut tx = with_fx(t(61, "SPCX", "purchase", "2026-06-07", 6.0, 191.66611790657), 135.0);
                tx.dividends_total = -0.0;
                tx
            },
        ]
    }

    #[test]
    fn full_dataset_spcx_has_positive_remaining() {
        let r = calc_remaining_by_lot(&real_transactions());
        assert!(r[&61] > 0.0);
        assert_eq!(r[&61], 6.0);
    }

    #[test]
    fn full_dataset_spcx_passes_active_filter() {
        let txs = real_transactions();
        let r = calc_remaining_by_lot(&txs);
        let active: Vec<&PortfolioTx> = txs
            .iter()
            .filter(|tx| tx.tx_type == TxType::Purchase && r.get(&tx.id).copied().unwrap_or(0.0) > 0.0)
            .collect();
        let spcx = active.iter().find(|tx| tx.symbol == "SPCX");
        assert!(spcx.is_some());
        assert_eq!(spcx.unwrap().id, 61);
    }

    #[test]
    fn full_dataset_usd_purchases_appear_active() {
        let txs = real_transactions();
        let r = calc_remaining_by_lot(&txs);
        let symbols: Vec<&str> = txs
            .iter()
            .filter(|tx| tx.tx_type == TxType::Purchase && r.get(&tx.id).copied().unwrap_or(0.0) > 0.0)
            .map(|tx| tx.symbol.as_str())
            .collect();
        assert!(symbols.contains(&"TSM"));
        assert!(symbols.contains(&"TXG"));
        assert!(symbols.contains(&"SPCX"));
    }

    #[test]
    fn full_dataset_fully_sold_excluded() {
        let txs = real_transactions();
        let r = calc_remaining_by_lot(&txs);
        let symbols: Vec<&str> = txs
            .iter()
            .filter(|tx| tx.tx_type == TxType::Purchase && r.get(&tx.id).copied().unwrap_or(0.0) > 0.0)
            .map(|tx| tx.symbol.as_str())
            .collect();
        assert!(!symbols.contains(&"GMG.AX"));
        assert!(!symbols.contains(&"AX1.AX"));
        assert!(!symbols.contains(&"COH.AX"));
        assert!(!symbols.contains(&"JLG.AX"));
    }

    #[test]
    fn negative_zero_dividends_total_is_harmless() {
        let txs: Vec<PortfolioTx> = real_transactions().into_iter().filter(|t| t.symbol == "SPCX").collect();
        let r = calc_remaining_by_lot(&txs);
        assert_eq!(r[&61], 6.0);
    }

    // -- holdings.ts ports --------------------------------------------------

    #[test]
    fn active_symbols_net_positive_only() {
        let txs = vec![
            with_symbol(make_tx(1, "purchase", "2024-01-01", Some(100.0), Some(10.0)), "AAA.AX"),
            with_symbol(make_tx(2, "sale", "2024-06-01", Some(100.0), Some(15.0)), "AAA.AX"),
            with_symbol(make_tx(3, "purchase", "2024-01-01", Some(50.0), Some(20.0)), "BBB.AX"),
        ];
        let active = get_active_holding_symbols(&txs);
        assert_eq!(active, vec!["BBB.AX".to_string()]);
    }

    #[test]
    fn earliest_remaining_purchase_date_skips_consumed_lots() {
        let txs = vec![
            make_tx(1, "purchase", "2024-01-01", Some(50.0), Some(10.0)),
            make_tx(2, "purchase", "2024-03-01", Some(50.0), Some(12.0)),
            make_tx(3, "sale", "2024-06-01", Some(50.0), Some(15.0)), // consumes lot 1
        ];
        assert_eq!(get_earliest_remaining_purchase_date(&txs, "TST.AX"), Some("2024-03-01".to_string()));
    }

    // -- calc_sold_entries (SoldStocks screen port) ---------------------------

    #[test]
    fn sold_entries_basic_sale() {
        let txs = vec![
            make_tx(1, "purchase", "2024-01-01", Some(100.0), Some(10.0)),
            with_brokerage(make_tx(2, "sale", "2024-01-31", Some(100.0), Some(15.0)), 10.0),
        ];
        let entries = calc_sold_entries(&txs);
        assert_eq!(entries.len(), 1);
        let e = &entries[0];
        assert!(close(e.avg_purchase_price, 10.0));
        assert!(close(e.realised_pl, 100.0 * 15.0 - 10.0 - 1000.0));
        assert_eq!(e.days_held, 30);
    }

    #[test]
    fn sold_entries_dividends_only_when_fully_closed() {
        // Partial: dividends stay with the holding side
        let partial = vec![
            with_dividends_total(make_tx(1, "purchase", "2024-01-01", Some(100.0), Some(10.0)), 50.0),
            make_tx(2, "sale", "2024-06-01", Some(40.0), Some(10.0)),
        ];
        let entries = calc_sold_entries(&partial);
        assert!(close(entries[0].dividends, 0.0));

        // Fully closed: dividends distributed proportionally across sales
        let closed = vec![
            with_dividends_total(make_tx(1, "purchase", "2024-01-01", Some(100.0), Some(10.0)), 60.0),
            make_tx(2, "sale", "2024-04-01", Some(25.0), Some(10.0)),
            make_tx(3, "sale", "2024-06-01", Some(75.0), Some(10.0)),
        ];
        let entries = calc_sold_entries(&closed);
        assert!(close(entries[0].dividends, 15.0));
        assert!(close(entries[1].dividends, 45.0));
        assert!(close(entries[0].realised_pl, 15.0));
        assert!(close(entries[1].realised_pl, 45.0));
    }

    // -------------------------------------------------------------------------
    // shares_on_date / implied_dividend_payments — the dividend-eligibility
    // ledger walk shared by the API and the dividends daemon.
    // -------------------------------------------------------------------------

    #[test]
    fn shares_on_date_walks_the_ledger() {
        let txs = vec![
            make_tx(1, "purchase", "2026-01-05", Some(100.0), Some(10.0)),
            make_tx(2, "sale", "2026-03-02", Some(40.0), Some(12.0)),
            make_tx(3, "purchase", "2026-05-01", Some(10.0), Some(11.0)),
        ];
        assert!(close(shares_on_date(&txs, "2026-01-04"), 0.0), "before first purchase");
        assert!(close(shares_on_date(&txs, "2026-01-05"), 100.0), "the purchase day itself counts");
        assert!(close(shares_on_date(&txs, "2026-02-01"), 100.0));
        assert!(close(shares_on_date(&txs, "2026-03-02"), 60.0), "sale applies on its day");
        assert!(close(shares_on_date(&txs, "2026-06-01"), 70.0));
    }

    #[test]
    fn shares_on_date_clamps_oversold_ledgers_at_zero() {
        let txs = vec![
            make_tx(1, "purchase", "2026-01-05", Some(10.0), Some(10.0)),
            make_tx(2, "sale", "2026-02-01", Some(15.0), Some(12.0)), // recorded over-sell
        ];
        assert!(close(shares_on_date(&txs, "2026-03-01"), 0.0));
    }

    #[test]
    fn implied_payments_skip_ineligible_ex_dates() {
        let txs = vec![
            make_tx(1, "purchase", "2026-01-05", Some(100.0), Some(10.0)),
            make_tx(2, "sale", "2026-03-02", Some(100.0), Some(12.0)),
        ];
        let events = vec![
            ("2026-01-01".to_string(), 0.50), // before purchase — no shares
            ("2026-02-01".to_string(), 0.50), // 100 shares held
            ("2026-04-01".to_string(), 0.50), // fully sold — no shares
        ];
        let payments = implied_dividend_payments(&txs, &events);
        assert_eq!(payments.len(), 1);
        assert_eq!(payments[0].ex_date, "2026-02-01");
        assert!(close(payments[0].shares_held, 100.0));
        assert!(close(payments[0].total_payment, 50.0));
    }
}

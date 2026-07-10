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
    /// Cost basis of remaining shares (AUD)
    pub remaining_cost: f64,
    /// Cost basis of remaining shares in the stock's native currency
    pub native_remaining_cost: f64,
    pub total_sold_qty: f64,
    /// Realised P/L from all sales (proceeds - cost basis - brokerage), excluding dividends
    pub realised_pl: f64,
    /// Dividend total read from dividends_total (pre-computed from dividend_events)
    pub dividends_total: f64,
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
                lots.push(Lot { quantity: qty, price });
                native_lots.push(Lot { quantity: qty, price: tx.native_price.unwrap_or(price) });
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

/// Effective dividends for a symbol: the API-computed dividends_total when
/// positive, otherwise the sum of manually recorded dividend transactions.
pub fn symbol_dividends(sorted: &[PortfolioTx]) -> f64 {
    let mut from_total = 0.0;
    let mut manual = 0.0;
    for tx in sorted {
        if tx.dividends_total > 0.0 {
            from_total = tx.dividends_total;
        } else if tx.tx_type == TxType::Dividend {
            if let Some(amount) = tx.amount {
                manual += amount;
            }
        }
    }
    if from_total > 0.0 { from_total } else { manual }
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
                lots.push(Lot { quantity: qty, price });
                native_lots.push(Lot { quantity: qty, price: tx.native_price.unwrap_or(price) });
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

#[derive(Debug, Clone, PartialEq)]
pub struct PortfolioPl {
    pub holdings_pl: f64,
    pub sold_pl: f64,
    pub total_pl: f64,
    pub total_value: f64,
    pub stock_count: usize,
}

/// Portfolio-level P/L matching what Holdings + Sold Stocks screens show
/// combined. `price_map` holds the current AUD price per symbol (None when
/// unavailable — the position then contributes zero value).
pub fn calc_portfolio_pl(transactions: &[PortfolioTx], price_map: &HashMap<String, Option<f64>>) -> PortfolioPl {
    let mut holdings_pl = 0.0;
    let mut sold_pl = 0.0;
    let mut total_value = 0.0;
    let mut stock_count = 0;

    for (symbol, txs) in group_by_symbol(transactions) {
        let pos = calc_symbol_position(&txs);

        if pos.remaining_shares > 0.0 {
            stock_count += 1;
            let price = price_map.get(&symbol).copied().flatten();
            let current_value = match price {
                Some(p) if p != 0.0 => pos.remaining_shares * p,
                _ => 0.0,
            };
            total_value += current_value;
            holdings_pl += current_value - pos.remaining_cost + pos.dividends;
        }

        sold_pl += pos.sold_pl();
    }

    PortfolioPl { holdings_pl, sold_pl, total_pl: holdings_pl + sold_pl, total_value, stock_count }
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
        if let Some(qty) = tx.quantity {
            if qty != 0.0 {
                match tx.tx_type {
                    TxType::Purchase => *entry += qty,
                    TxType::Sale => *entry -= qty,
                    _ => {}
                }
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
                lots.push((qty, price, tx.date.clone()));
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

    fn price_map(pairs: &[(&str, Option<f64>)]) -> HashMap<String, Option<f64>> {
        pairs.iter().map(|(s, p)| (s.to_string(), *p)).collect()
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
    fn portfolio_active_holding_with_price() {
        let txs = vec![make_tx(1, "purchase", "2024-01-01", Some(100.0), Some(10.0))];
        let result = calc_portfolio_pl(&txs, &price_map(&[("TST.AX", Some(15.0))]));
        assert!(close(result.holdings_pl, 500.0));
        assert_eq!(result.sold_pl, 0.0);
        assert!(close(result.total_pl, 500.0));
        assert!(close(result.total_value, 1500.0));
        assert_eq!(result.stock_count, 1);
    }

    #[test]
    fn portfolio_no_price_contributes_zero_value() {
        let txs = vec![make_tx(1, "purchase", "2024-01-01", Some(100.0), Some(10.0))];
        let result = calc_portfolio_pl(&txs, &price_map(&[("TST.AX", None)]));
        assert_eq!(result.total_value, 0.0);
        assert!(close(result.holdings_pl, -1000.0));
    }

    #[test]
    fn portfolio_fully_sold_adds_to_sold_pl() {
        let txs = vec![
            make_tx(1, "purchase", "2024-01-01", Some(100.0), Some(10.0)),
            make_tx(2, "sale", "2024-06-01", Some(100.0), Some(15.0)),
        ];
        let result = calc_portfolio_pl(&txs, &HashMap::new());
        assert!(close(result.sold_pl, 500.0));
        assert_eq!(result.holdings_pl, 0.0);
        assert_eq!(result.stock_count, 0);
    }

    #[test]
    fn portfolio_partial_sale_splits_realised_unrealised() {
        let txs = vec![
            make_tx(1, "purchase", "2024-01-01", Some(100.0), Some(10.0)),
            make_tx(2, "sale", "2024-06-01", Some(40.0), Some(15.0)),
        ];
        // 60 remain @ cost $600, price $18 → unrealised = 60*18 - 600 = 480
        // 40 sold: proceeds 600, cost 400 → realised = 200
        let result = calc_portfolio_pl(&txs, &price_map(&[("TST.AX", Some(18.0))]));
        assert!(close(result.holdings_pl, 480.0));
        assert!(close(result.sold_pl, 200.0));
        assert!(close(result.total_pl, 680.0));
    }

    #[test]
    fn portfolio_dividends_to_holdings_for_active_position() {
        let txs = vec![with_dividends_total(make_tx(1, "purchase", "2024-01-01", Some(100.0), Some(10.0)), 50.0)];
        let result = calc_portfolio_pl(&txs, &price_map(&[("TST.AX", Some(10.0))]));
        // price = cost, so unrealised = 0; P/L = dividends = 50
        assert!(close(result.holdings_pl, 50.0));
    }

    #[test]
    fn portfolio_dividends_to_sold_when_fully_closed() {
        let txs = vec![
            with_dividends_total(make_tx(1, "purchase", "2024-01-01", Some(100.0), Some(10.0)), 60.0),
            make_tx(2, "sale", "2024-06-01", Some(100.0), Some(10.0)), // sold at cost
        ];
        let result = calc_portfolio_pl(&txs, &HashMap::new());
        assert!(close(result.sold_pl, 60.0));
    }

    #[test]
    fn portfolio_partial_sale_dividends_counted_once() {
        let txs = vec![
            with_dividends_total(make_tx(1, "purchase", "2024-01-01", Some(100.0), Some(10.0)), 50.0),
            make_tx(2, "sale", "2024-06-01", Some(40.0), Some(10.0)), // sold at cost
        ];
        // 60 remain at price == cost → unrealised 0 + dividends 50; sold at cost → 0.
        // Total P/L must be exactly $50, not $100.
        let result = calc_portfolio_pl(&txs, &price_map(&[("TST.AX", Some(10.0))]));
        assert!(close(result.holdings_pl, 50.0));
        assert!(close(result.sold_pl, 0.0));
        assert!(close(result.total_pl, 50.0));
    }

    #[test]
    fn portfolio_multiple_symbols_summed() {
        let txs = vec![
            with_symbol(make_tx(1, "purchase", "2024-01-01", Some(100.0), Some(10.0)), "AAA.AX"),
            with_symbol(make_tx(2, "purchase", "2024-01-01", Some(50.0), Some(20.0)), "BBB.AX"),
        ];
        let result = calc_portfolio_pl(&txs, &price_map(&[("AAA.AX", Some(12.0)), ("BBB.AX", Some(18.0))]));
        // AAA: 100*(12-10) = 200; BBB: 50*(18-20) = -100
        assert!(close(result.total_pl, 100.0));
        assert_eq!(result.stock_count, 2);
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
    fn usd_active_holding_counted_in_portfolio() {
        let txs = vec![spcx_purchase(1, "2026-01-15", 50.0, 2.244, 1.50)];
        let result = calc_portfolio_pl(&txs, &price_map(&[("SPCX", Some(2.80))]));
        assert_eq!(result.stock_count, 1);
        assert!(close(result.total_value, 50.0 * 2.80));
        assert!(close(result.holdings_pl, 27.80));
    }

    #[test]
    fn usd_and_aud_stocks_coexist() {
        let txs = vec![
            spcx_purchase(1, "2026-01-15", 50.0, 2.244, 1.50),
            with_symbol(make_tx(2, "purchase", "2026-02-01", Some(100.0), Some(45.0)), "CBA.AX"),
        ];
        let result = calc_portfolio_pl(&txs, &price_map(&[("SPCX", Some(2.80)), ("CBA.AX", Some(48.0))]));
        assert_eq!(result.stock_count, 2);
        // SPCX: 50*(2.80-2.244)=27.80  CBA: 100*(48-45)=300
        assert!(close(result.holdings_pl, 327.80));
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
}

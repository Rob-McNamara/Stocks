//! Portfolio valuation: the shared context every portfolio screen is built on,
//! the holdings, overview, lots, sold, risk and hindsight endpoints, the value
//! history, and the stored-bar indicators they read.

use super::*;

/// A symbol's or currency's closes, oldest first, with a cursor for the sweep.
pub(crate) struct SeriesCursor {
    pub(crate) points: Vec<(String, f64)>,
    pub(crate) index: usize,
    pub(crate) current: Option<f64>,
}

impl SeriesCursor {
    pub(crate) fn new(points: Vec<(String, f64)>) -> Self {
        Self { points, index: 0, current: None }
    }

    /// Advance to `date`, returning the latest close at or before it. Values
    /// carry forward, so weekends, holidays and missing bars hold the last
    /// traded price rather than dropping the holding out of the valuation.
    pub(crate) fn value_on(&mut self, date: &str) -> Option<f64> {
        while self.index < self.points.len() && self.points[self.index].0.as_str() <= date {
            self.current = Some(self.points[self.index].1);
            self.index += 1;
        }
        self.current
    }

    /// True once `date` is past the last stored bar — where a delisted or
    /// suspended holding stops having real prices and a manual valuation, if
    /// one is set, takes over.
    pub(crate) fn is_past_end(&self, date: &str) -> bool {
        match self.points.last() {
            Some((last, _)) => date > last.as_str(),
            None => true,
        }
    }
}

#[derive(Serialize)]
pub(crate) struct HistoryPoint {
    pub(crate) date: String,
    pub(crate) stocks: f64,
    pub(crate) cash: f64,
    pub(crate) total: f64,
    pub(crate) flow: f64,
}

#[derive(Deserialize)]
pub(crate) struct HistoryQuery {
    pub(crate) from: Option<String>,
    pub(crate) to: Option<String>,
}

/// Daily portfolio value in AUD, with the external flows needed to measure
/// return.
///
/// A trade that names no cash account is treated as externally funded: the
/// shares appear with no matching cash movement, so without this the purchase
/// would look like value materialising from nowhere and inflate the return.
/// Every transaction recorded before the cash ledger existed is in that state,
/// which is what makes the full history usable rather than only the part after
/// the ledger starts.
///
/// The first element is an **anchor**: the day before the requested window,
/// carrying the value the window opens with. Without it the first day inside
/// the window would have nothing to be compared against, so its movement would
/// drop out of the return, and its own contributions would be double counted —
/// once in the opening value and again in the contribution total.
pub(crate) fn build_portfolio_history(
    conn: &Connection,
    from: Option<&str>,
    to: Option<&str>,
) -> Result<Vec<portfolio::DailyValue>, String> {
    #[derive(Clone)]
    struct Trade {
        date: String,
        symbol: String,
        tx_type: String,
        quantity: f64,
        price_aud: f64,
        brokerage_aud: f64,
        has_cash_leg: bool,
    }

    let mut stmt = conn
        .prepare(
            "SELECT date, symbol, transaction_type, COALESCE(quantity, 0), COALESCE(price, 0),
                    COALESCE(brokerage, 0), cash_account_id
               FROM holdings_transactions ORDER BY date, id",
        )
        .map_err(|e| e.to_string())?;
    let trades: Vec<Trade> = stmt
        .query_map([], |row| {
            Ok(Trade {
                date: row.get(0)?,
                symbol: row.get(1)?,
                tx_type: row.get(2)?,
                quantity: row.get(3)?,
                price_aud: row.get(4)?,
                brokerage_aud: row.get(5)?,
                has_cash_leg: row.get::<_, Option<i64>>(6)?.is_some(),
            })
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?
        .into_iter()
        .collect();

    // Only accounts counted toward the portfolio; an everyday savings account
    // can be tracked without being treated as invested capital.
    let mut account_stmt = conn
        .prepare("SELECT id, currency FROM cash_accounts WHERE include_in_portfolio = 1")
        .map_err(|e| e.to_string())?;
    let accounts: HashMap<i64, String> = account_stmt
        .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)))
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?
        .into_iter()
        .collect();

    let mut cash_stmt = conn
        .prepare("SELECT date, account_id, amount, kind FROM cash_transactions ORDER BY date, id")
        .map_err(|e| e.to_string())?;
    let cash_txs: Vec<(String, i64, f64, String)> = cash_stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)))
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|(_, account_id, _, _)| accounts.contains_key(account_id))
        .collect();

    if trades.is_empty() && cash_txs.is_empty() {
        return Ok(Vec::new());
    }

    let earliest = trades
        .iter()
        .map(|t| t.date.clone())
        .chain(cash_txs.iter().map(|c| c.0.clone()))
        .min()
        .unwrap_or_default();
    let start = from.map(str::to_string).unwrap_or(earliest);
    let end = to.map(str::to_string).unwrap_or_else(today_local_str);
    let (Ok(start_date), Ok(end_date)) = (
        NaiveDate::parse_from_str(&start, "%Y-%m-%d"),
        NaiveDate::parse_from_str(&end, "%Y-%m-%d"),
    ) else {
        return Err("Invalid date range. Use YYYY-MM-DD.".to_string());
    };
    if end_date < start_date {
        return Err("`to` must not be before `from`".to_string());
    }

    // Price and rate cursors, one per symbol and per currency.
    let symbols: Vec<String> = {
        let mut seen: Vec<String> = trades.iter().map(|t| t.symbol.clone()).collect();
        seen.sort();
        seen.dedup();
        seen
    };
    // Same rule as the holdings screens (`build_portfolio_context`): the
    // listing currency, else the foreign currency the trades were recorded in,
    // else AUD — so a symbol missing from symbol_info is not valued as AUD.
    let symbol_currency: HashMap<String, String> = {
        let mut stmt = conn
            .prepare(
                "SELECT h.symbol,
                        COALESCE(NULLIF(UPPER(TRIM(si.currency)), ''),
                                 (SELECT UPPER(TRIM(t.currency)) FROM holdings_transactions t
                                   WHERE t.symbol = h.symbol AND UPPER(TRIM(t.currency)) NOT IN ('', 'AUD') LIMIT 1),
                                 'AUD')
                   FROM (SELECT DISTINCT symbol FROM holdings_transactions) h
                   LEFT JOIN symbol_info si ON si.symbol = h.symbol",
            )
            .map_err(|e| e.to_string())?;
        stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?
            .into_iter()
            .collect()
    };
    let manual = manual_prices(conn);
    let quotes = cached_quote_prices(conn);
    // A stored bar exists for today as soon as the fetcher has run, but it is
    // the day's close-so-far, not the live quote the rest of the app shows. On
    // today's point only, the quote wins; every earlier day stays on its bar.
    let today = today_local_str();
    let mut prices: HashMap<String, SeriesCursor> = symbols
        .iter()
        .map(|s| (s.clone(), SeriesCursor::new(load_close_series(conn, s))))
        .collect();

    let mut currencies: Vec<String> = symbol_currency.values().cloned().collect();
    currencies.extend(accounts.values().cloned());
    currencies.sort();
    currencies.dedup();
    let mut rates: HashMap<String, SeriesCursor> = currencies
        .iter()
        .filter(|c| c.as_str() != "AUD")
        .map(|c| (c.clone(), SeriesCursor::new(load_close_series(conn, &fx_pair_symbol(c)))))
        .collect();

    let mut shares: HashMap<String, f64> = HashMap::new();
    let mut balances: HashMap<i64, f64> = HashMap::new();
    let mut trade_index = 0usize;
    let mut cash_index = 0usize;
    let mut series = Vec::new();

    // Start a day early to produce the anchor described above.
    let mut date = start_date.pred_opt().unwrap_or(start_date);
    while date <= end_date {
        let day = date.format("%Y-%m-%d").to_string();
        let mut flow = 0.0;

        // Apply everything dated on or before today that has not been applied
        // yet, so transactions before `from` are folded into the opening state.
        while trade_index < trades.len() && trades[trade_index].date <= day {
            let trade = &trades[trade_index];
            let signed = match trade.tx_type.as_str() {
                "purchase" => trade.quantity,
                "sale" => -trade.quantity,
                _ => 0.0,
            };
            *shares.entry(trade.symbol.clone()).or_insert(0.0) += signed;

            // Externally funded trades count as money in or out on their day.
            if !trade.has_cash_leg && trade.date == day {
                match trade.tx_type.as_str() {
                    "purchase" => flow += trade.quantity * trade.price_aud + trade.brokerage_aud,
                    "sale" => flow -= trade.quantity * trade.price_aud - trade.brokerage_aud,
                    _ => {}
                }
            }
            trade_index += 1;
        }

        while cash_index < cash_txs.len() && cash_txs[cash_index].0 <= day {
            let (tx_date, account_id, amount, kind) = &cash_txs[cash_index];
            *balances.entry(*account_id).or_insert(0.0) += amount;
            if tx_date == &day && matches!(kind.as_str(), "deposit" | "withdrawal" | "opening_balance") {
                let currency = accounts.get(account_id).cloned().unwrap_or_else(|| "AUD".to_string());
                let rate = if currency == "AUD" {
                    Some(1.0)
                } else {
                    rates.get_mut(&currency).and_then(|c| c.value_on(&day))
                };
                flow += amount * rate.unwrap_or(0.0);
            }
            cash_index += 1;
        }

        let mut stocks = 0.0;
        for (symbol, held) in &shares {
            if *held <= 0.0 {
                continue;
            }
            // Past the last real bar there is no close for the day, so the
            // live quote stands in, then a manual valuation, then the last
            // stored close. That is the order the holdings endpoint uses, and
            // matching it is what keeps the chart's final point equal to the
            // Stock Value shown above it.
            let latest = || quotes.get(symbol).copied().or_else(|| manual.get(symbol).copied());
            let close = match prices.get_mut(symbol) {
                Some(cursor) => {
                    let stored = cursor.value_on(&day);
                    if day == today || cursor.is_past_end(&day) { latest().or(stored) } else { stored }
                }
                None => latest(),
            };
            let Some(close) = close else { continue };
            let currency = symbol_currency.get(symbol).cloned().unwrap_or_else(|| "AUD".to_string());
            // The FX bars stop with the price bars, and past that point the
            // holdings endpoint converts at the live rate. Following it here is
            // what makes the two agree: with a stale daily bar instead, every
            // foreign holding lands a fraction out and the chart's last point
            // drifts from the Stock Value beside it.
            let rate = if currency == "AUD" {
                Some(1.0)
            } else {
                let live = || quotes.get(&fx_pair_symbol(&currency)).copied();
                match rates.get_mut(&currency) {
                    Some(cursor) => {
                        let stored = cursor.value_on(&day);
                        if day == today || cursor.is_past_end(&day) { live().or(stored) } else { stored }
                    }
                    None => live(),
                }
            };
            let Some(rate) = rate else { continue };
            stocks += held * close * rate;
        }

        let mut cash = 0.0;
        for (account_id, balance) in &balances {
            let currency = accounts.get(account_id).cloned().unwrap_or_else(|| "AUD".to_string());
            let rate = if currency == "AUD" {
                Some(1.0)
            } else {
                rates.get_mut(&currency).and_then(|c| c.value_on(&day))
            };
            let Some(rate) = rate else { continue };
            cash += balance * rate;
        }

        series.push(portfolio::DailyValue { date: day, stocks, cash, flow });
        date = match date.succ_opt() {
            Some(d) => d,
            None => break,
        };
    }

    Ok(series)
}

#[utoipa::path(get, path = "/api/v1/portfolio/history", tag = "portfolio", responses((status = 200, description = "Daily portfolio value and time-weighted return")))]
#[get("/api/portfolio/history")]
pub(crate) async fn get_portfolio_history(db_path: web::Data<PathBuf>, query: web::Query<HistoryQuery>) -> impl Responder {
    let conn = match open_db(db_path.as_ref()) {
        Ok(c) => c,
        Err(err) => return err_internal(err.to_string()),
    };
    // The floor applies to every range, not just "All": a 5-year window reaches
    // just as far back into the unpriced years as an unbounded one.
    let floor: Option<String> = load_config(&db_path)
        .ok()
        .and_then(|items| {
            items
                .into_iter()
                .find(|item| item.key == PORTFOLIO_HISTORY_START)
                .map(|item| item.value.trim().to_string())
        })
        .filter(|value| !value.is_empty());
    // ISO dates order lexicographically, so the later of the two is the max.
    let from = match (query.from.as_deref(), floor.as_deref()) {
        (Some(requested), Some(floor)) => Some(requested.max(floor)),
        (None, floor) => floor,
        (requested, None) => requested,
    };

    let series = match build_portfolio_history(&conn, from, query.to.as_deref()) {
        Ok(s) => s,
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "portfolio_history", "api", None, &err);
            return err_bad_request(err);
        }
    };

    // Element 0 is the anchor: the value carried into the window, which is not
    // part of it. Returns chain across the whole vector so the first real day
    // still counts; contributions and the reported range cover the window only.
    let twr = portfolio::time_weighted_return(&series);
    let opening_value = series.first().map(|p| p.total()).unwrap_or(0.0);
    let window = if series.is_empty() { &series[..] } else { &series[1..] };
    let contributions = portfolio::net_contributions(window);
    let end_value = window.last().map(|p| p.total()).unwrap_or(opening_value);

    HttpResponse::Ok().json(serde_json::json!({
        "series": window.iter().map(|p| HistoryPoint {
            date: p.date.clone(),
            stocks: p.stocks,
            cash: p.cash,
            total: p.total(),
            flow: p.flow,
        }).collect::<Vec<_>>(),
        "summary": {
            "start_date": window.first().map(|p| p.date.clone()),
            "end_date": window.last().map(|p| p.date.clone()),
            "opening_value": opening_value,
            "end_value": end_value,
            "net_contributions": contributions,
            // What the portfolio earned: the change in value that contributions
            // do not account for. opening + contributions + gain = end.
            "gain": end_value - opening_value - contributions,
            "twr_pct": twr.map(|r| r * 100.0),
        },
        // Published so the client can hide range buttons the floor makes
        // identical to each other, rather than offering three ways to ask for
        // the same window.
        "start_floor": floor,
    }))
}

/// Latest N-day simple moving average from stored daily closes (no network).
pub(crate) fn stored_sma(conn: &Connection, symbol: &str, period: usize) -> Option<f64> {
    let mut stmt = conn
        .prepare("SELECT close FROM prices WHERE symbol = ?1 AND close IS NOT NULL ORDER BY date DESC LIMIT ?2")
        .ok()?;
    let closes: Vec<f64> = stmt
        .query_map(params![symbol, period as i64], |row| row.get::<_, f64>(0))
        .ok()?
        .flatten()
        .collect();
    if closes.len() < period {
        return None;
    }
    Some(closes.iter().sum::<f64>() / period as f64)
}

/// Exponential moving average over *weekly* closes.
///
/// A 40-week EMA is not a 200-day EMA. Both span roughly the same calendar, but
/// the weekly one is computed from one close per week, so it steps once a week
/// and is far less sensitive to a single day's move. Chartists mean the weekly
/// figure, so it is what gets computed.
///
/// Weeks start on Monday and take that week's last available close, matching
/// `toWeeklyBars` in the web client — the table and the chart's Week interval
/// must not disagree about what a week is.
pub(crate) fn stored_weekly_ema(conn: &Connection, symbol: &str, period: usize) -> Option<f64> {
    if period == 0 {
        return None;
    }
    // Daily rows to read before collapsing. An EMA needs history well beyond
    // its period to settle, and a week costs ~5 rows, so 40 weeks of settled
    // average needs years of dailies behind it.
    const LOOKBACK_DAYS: i64 = 3000;
    let mut stmt = conn
        .prepare(
            "SELECT date, close FROM prices
              WHERE symbol = ?1 AND close IS NOT NULL
              ORDER BY date DESC LIMIT ?2",
        )
        .ok()?;
    let mut rows: Vec<(String, f64)> = stmt
        .query_map(params![symbol, LOOKBACK_DAYS], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?))
        })
        .ok()?
        .flatten()
        .collect();
    rows.reverse(); // oldest first, so each week's last close wins

    let mut weekly: Vec<f64> = Vec::new();
    let mut current_week: Option<NaiveDate> = None;
    for (date, close) in rows {
        let Ok(parsed) = NaiveDate::parse_from_str(&date, "%Y-%m-%d") else { continue };
        let monday = parsed.week(chrono::Weekday::Mon).first_day();
        if current_week == Some(monday) {
            // Same week — this later close replaces the earlier one.
            *weekly.last_mut()? = close;
        } else {
            current_week = Some(monday);
            weekly.push(close);
        }
    }

    if weekly.len() < period {
        return None;
    }
    let k = 2.0 / (period as f64 + 1.0);
    let mut ema = weekly[..period].iter().sum::<f64>() / period as f64;
    for close in &weekly[period..] {
        ema = close * k + ema * (1.0 - k);
    }
    Some(ema)
}

/// The cost basis and dividends a holding's performance is measured against,
/// and the baseline it came from when one is set.
///
/// A per-symbol baseline *replaces* the purchase rather than sitting beside it:
/// for a holding bought decades ago the original price is a record, not a
/// useful denominator. Both the holdings endpoint and the overview aggregate go
/// through here, so the Holdings screen and the Dashboard total can never
/// disagree about the same position.
///
/// The stored basis price is native, like the closes it came from, so it is
/// converted at today's rate — an approximation for a foreign holding, but
/// consistent with the current value it is measured against.
pub(crate) fn effective_basis(
    ctx: &PortfolioContext,
    symbol: &str,
    txs: &[portfolio::PortfolioTx],
    remaining_shares: f64,
    purchase_cost: f64,
    purchase_dividends: f64,
) -> (f64, f64, Option<(String, f64)>) {
    let fields = ctx.fields.get(symbol);
    let date = fields.and_then(|f| f.get(PL_BASIS_DATE)).map(|d| d.trim()).filter(|d| !d.is_empty());
    let native = fields
        .and_then(|f| f.get(PL_BASIS_PRICE))
        .and_then(|p| p.trim().parse::<f64>().ok())
        .filter(|p| *p > 0.0);
    if let (Some(date), Some(native)) = (date, native) {
        // Without a rate the baseline cannot be expressed in AUD, so the
        // purchase stands in rather than a native figure posing as AUD.
        let Some(native_aud) = ctx.to_aud(symbol, native) else {
            return (purchase_cost, purchase_dividends, None);
        };
        let basis = portfolio::rebase_at(txs, remaining_shares, date, native_aud);
        // A zero basis would divide the percentage by nothing; fall back to the
        // purchase rather than reporting an infinite return.
        if basis.cost > 0.0 {
            return (basis.cost, basis.dividends, Some((date.to_string(), native)));
        }
    }
    (purchase_cost, purchase_dividends, None)
}

/// Latest value of a dashboard `indicator:` field, in the symbol's own
/// currency — the same basis as the stored closes it is derived from, so it
/// needs no FX conversion to compare against a native price.
///
/// Keys are matched exactly rather than parsed for a period: the Configuration
/// screen offers a fixed set, and an explicit list keeps a typo from silently
/// producing an empty dashboard table.
pub(crate) fn indicator_value(conn: &Connection, symbol: &str, key: &str) -> Option<f64> {
    match key {
        "sma50" => stored_sma(conn, symbol, 50),
        "sma150" => stored_sma(conn, symbol, 150),
        "ema40w" => stored_weekly_ema(conn, symbol, 40),
        _ => None,
    }
}

/// Stored daily bars for one symbol, newest-last, straight from the database.
///
/// Deliberately not `fetch_price_history`: that tops up from Yahoo when the
/// stored window looks short, and a crossover list spanning a few hundred
/// watchlist symbols would turn one dashboard load into a burst of fetches.
/// Whatever has been ingested is what the indicator is built from.
pub(crate) fn load_local_history(conn: &Connection, symbol: &str, days: i64) -> Vec<PriceHistoryPoint> {
    let Ok(mut stmt) = conn.prepare(
        "SELECT date, open, high, low, close, volume FROM prices
          WHERE symbol = ?1 AND close IS NOT NULL
          ORDER BY date DESC LIMIT ?2",
    ) else {
        return Vec::new();
    };
    let Ok(rows) = stmt.query_map(params![symbol, days], |row| {
        Ok(PriceHistoryPoint {
            date: row.get(0)?,
            open: row.get(1)?,
            high: row.get(2)?,
            low: row.get(3)?,
            close: row.get(4)?,
            volume: row.get(5)?,
        })
    }) else {
        return Vec::new();
    };
    let mut history: Vec<PriceHistoryPoint> = rows.flatten().collect();
    history.reverse();
    history
}

/// The reference series a crossover is measured against, aligned one-to-one
/// with `history` so a crossing can be counted in trading days.
///
/// `constant` covers a stored field — a breakthrough price does not move, so
/// its series is that number repeated, and "days above" reads as days since the
/// price cleared it.
pub(crate) fn crossover_reference(
    history: &[PriceHistoryPoint],
    field_key: &str,
    constant: Option<f64>,
) -> Option<Vec<Option<f64>>> {
    use stocks::indicators as ind;
    let points = indicator_points(history);
    match field_key {
        "sma50" => Some(ind::calculate_sma(&points, 50)),
        "sma150" => Some(ind::calculate_sma(&points, 150)),
        "ema40w" => weekly_ema_series(history, 40),
        _ => constant.map(|c| vec![Some(c); history.len()]),
    }
}

/// A weekly EMA spread back over the daily bars it covers, so a weekly
/// indicator can still be crossed in days.
///
/// Each day carries the EMA of the last *completed* week before it. Using the
/// running week's own value would let a bar be compared against an average that
/// includes its own close — the count would then shift retroactively as the
/// week finished.
pub(crate) fn weekly_ema_series(history: &[PriceHistoryPoint], period: usize) -> Option<Vec<Option<f64>>> {
    if period == 0 {
        return None;
    }
    // Collapse to one close per week, remembering which week each daily bar is
    // in so the finished EMA can be mapped back onto the dailies.
    let mut weeks: Vec<NaiveDate> = Vec::new();
    let mut weekly_closes: Vec<f64> = Vec::new();
    let mut bar_week: Vec<Option<usize>> = Vec::with_capacity(history.len());
    for bar in history {
        let parsed = NaiveDate::parse_from_str(&bar.date, "%Y-%m-%d").ok();
        match (parsed, bar.close) {
            (Some(date), Some(close)) => {
                let monday = date.week(chrono::Weekday::Mon).first_day();
                if weeks.last() == Some(&monday) {
                    *weekly_closes.last_mut()? = close;
                } else {
                    weeks.push(monday);
                    weekly_closes.push(close);
                }
                bar_week.push(Some(weeks.len() - 1));
            }
            _ => bar_week.push(None),
        }
    }
    if weekly_closes.len() < period {
        return None;
    }

    // EMA over the weekly closes: index i holds the value once week i closes.
    let k = 2.0 / (period as f64 + 1.0);
    let mut weekly_ema: Vec<Option<f64>> = vec![None; weekly_closes.len()];
    let mut ema = weekly_closes[..period].iter().sum::<f64>() / period as f64;
    weekly_ema[period - 1] = Some(ema);
    for i in period..weekly_closes.len() {
        ema = weekly_closes[i] * k + ema * (1.0 - k);
        weekly_ema[i] = Some(ema);
    }

    Some(
        bar_week
            .iter()
            .map(|w| w.and_then(|i| if i == 0 { None } else { weekly_ema[i - 1] }))
            .collect(),
    )
}

pub(crate) struct EffectivePrice {
    pub(crate) native: Option<f64>,
    pub(crate) aud: Option<f64>,
    pub(crate) source: &'static str, // "cache" | "manual" | "none"
    pub(crate) price_date: Option<String>,
    pub(crate) change: Option<f64>,
    pub(crate) change_percent: Option<f64>,
    pub(crate) volume: Option<i64>,
}

pub(crate) struct PortfolioContext {
    pub(crate) groups: Vec<(String, Vec<PortfolioTx>)>,
    pub(crate) prices: HashMap<String, EffectivePrice>,
    pub(crate) info: HashMap<String, SymbolInfo>,
    pub(crate) fields: HashMap<String, HashMap<String, String>>,
    pub(crate) intl: HashMap<String, bool>,
    pub(crate) etf: HashMap<String, bool>,
    /// true when every purchase for the symbol was recorded in AUD — such
    /// stocks are displayed in AUD even if they trade in a foreign currency
    pub(crate) all_aud: HashMap<String, bool>,
    /// The currency each symbol trades in: `symbol_info`, else the currency
    /// its own transactions were recorded in, else AUD.
    pub(crate) currency: HashMap<String, String>,
    pub(crate) fx_rates: HashMap<String, Option<f64>>,
}

impl PortfolioContext {
    pub(crate) fn currency_of(&self, symbol: &str) -> String {
        self.currency.get(symbol).cloned().unwrap_or_else(|| "AUD".to_string())
    }

    /// `value`, in the symbol's own currency, converted to AUD — or `None`
    /// when no rate is available. It used to hand the native figure back
    /// unchanged, so a USD holding with no rate was silently valued as though
    /// its dollars were Australian.
    pub(crate) fn to_aud(&self, symbol: &str, value: f64) -> Option<f64> {
        let currency = self.currency_of(symbol);
        if currency == "AUD" {
            return Some(value);
        }
        self.fx_rates
            .get(&currency)
            .copied()
            .flatten()
            .filter(|rate| *rate != 0.0)
            .map(|rate| value * rate)
    }

    pub(crate) fn sector_of(&self, symbol: &str) -> Option<String> {
        self.fields
            .get(symbol)
            .and_then(|f| f.get("sector").cloned())
            .filter(|s| !s.is_empty())
    }
}

pub(crate) async fn build_portfolio_context(db_path: &PathBuf) -> Result<PortfolioContext, String> {
    let rows = fetch_holdings(db_path)?;
    let txs = to_portfolio_txs(&rows);
    let groups = portfolio::group_by_symbol(&txs);
    let symbols: Vec<String> = groups.iter().map(|(s, _)| s.clone()).collect();

    let config: HashMap<String, String> = load_config(db_path)?.into_iter().map(|c| (c.key, c.value)).collect();
    let fields = load_holdings_symbol_fields(db_path)?;

    let conn = open_db(db_path).map_err(|e| e.to_string())?;
    let mut info: HashMap<String, SymbolInfo> = HashMap::new();
    {
        let mut stmt = conn
            .prepare("SELECT symbol, instrument_type, long_name, currency FROM symbol_info")
            .map_err(|e| e.to_string())?;
        let info_rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        for r in info_rows.flatten() {
            info.insert(r.0, (r.1, r.2, r.3));
        }
    }

    // The currency each symbol trades in. symbol_info is the authority, but it
    // is filled by a background fetch that can fail; without a fallback a US
    // stock missing its row was valued as AUD. Its own foreign-currency
    // transactions say what it trades in just as well.
    let mut currency_map: HashMap<String, String> = HashMap::new();
    for symbol in &symbols {
        let from_info = info.get(symbol).and_then(|i| i.2.clone()).map(|c| c.trim().to_uppercase()).filter(|c| !c.is_empty());
        let from_txs = rows
            .iter()
            .filter(|t| &t.symbol == symbol)
            .map(|t| t.currency.trim().to_uppercase())
            .find(|c| !c.is_empty() && c != "AUD");
        currency_map.insert(symbol.clone(), from_info.or(from_txs).unwrap_or_else(|| "AUD".to_string()));
    }

    // Effective classification per symbol: instrument_type_* config override,
    // and the "purchased entirely in AUD ⇒ domestic" rule from the UI.
    let mut intl = HashMap::new();
    let mut etf = HashMap::new();
    let mut all_aud_map = HashMap::new();
    for symbol in &symbols {
        let purchases: Vec<&HoldingTransaction> = rows
            .iter()
            .filter(|t| &t.symbol == symbol && t.transaction_type == "purchase")
            .collect();
        let all_aud = !purchases.is_empty() && purchases.iter().all(|t| t.currency == "AUD");
        all_aud_map.insert(symbol.clone(), all_aud);
        let is_intl = !all_aud && currency_map.get(symbol).is_some_and(|c| c != "AUD");
        intl.insert(symbol.clone(), is_intl);
        let itype = config
            .get(&format!("instrument_type_{}", symbol))
            .cloned()
            .filter(|v| !v.is_empty())
            .or_else(|| info.get(symbol).and_then(|i| i.0.clone()))
            .unwrap_or_default();
        etf.insert(symbol.clone(), itype == "ETF" || itype == "MUTUALFUND");
    }

    let currencies: Vec<String> = currency_map
        .values()
        .filter(|c| c.as_str() != "AUD")
        .cloned()
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    let fx_rates = resolve_fx_rates(db_path, &currencies).await;
    for currency in &currencies {
        if fx_rates.get(currency).copied().flatten().is_none() {
            log_event_on_conn(
                &conn,
                "warn",
                "fx_fetch",
                None,
                &format!("No {}/AUD rate available; holdings in {} are shown without an AUD value", currency, currency),
            );
        }
    }

    let cached = load_cached_prices(db_path, &symbols)?;
    let cached_map: HashMap<String, CurrentPrice> = cached.into_iter().map(|p| (p.symbol.clone(), p)).collect();
    // Last traded bar per symbol, for anything the live feed no longer quotes.
    let last_closes = latest_closes(db_path, &symbols);
    let mut prices: HashMap<String, EffectivePrice> = HashMap::new();
    for symbol in &symbols {
        let c = cached_map.get(symbol);
        let mut native = c.and_then(|p| p.price).filter(|p| *p > 0.0);
        let mut source = if native.is_some() { "cache" } else { "none" };
        let mut price_date = c.and_then(|p| p.price_date.clone());
        // Same rule as the value chart's `manual_prices`: a manual price that
        // isn't a positive number is no price. A stray "0" used to value the
        // holding at nothing here while the chart ignored it.
        if native.is_none()
            && let Some(v) = config
                .get(&format!("manual_price_{}", symbol))
                .and_then(|manual| manual.trim().parse::<f64>().ok())
                .filter(|v| v.is_finite() && *v > 0.0)
        {
            native = Some(v);
            source = "manual";
        }
        // A delisted symbol has no quote and often no manual override either.
        // Its last traded price is the only honest figure available — better
        // than nothing, which would drop the holding out of the portfolio
        // total, and far better than the feed's zero. `source` says where it
        // came from so the UI can mark it stale.
        if native.is_none()
            && let Some((close, date)) = last_closes.get(symbol) {
                native = Some(*close);
                source = "last_close";
                price_date = Some(date.clone());
            }
        // No rate means no AUD price, rather than the native price passed off
        // as AUD; `fx_missing` on the holding says why the value is absent.
        let currency = currency_map.get(symbol).map(String::as_str).unwrap_or("AUD");
        let aud = match native {
            Some(n) if currency != "AUD" => fx_rates
                .get(currency)
                .copied()
                .flatten()
                .filter(|rate| *rate != 0.0)
                .map(|rate| n * rate),
            other => other,
        };
        prices.insert(symbol.clone(), EffectivePrice {
            native,
            aud,
            source,
            price_date,
            change: c.and_then(|p| p.change),
            change_percent: c.and_then(|p| p.change_percent),
            volume: c.and_then(|p| p.volume),
        });
    }

    Ok(PortfolioContext { groups, prices, info, fields, intl, etf, all_aud: all_aud_map, currency: currency_map, fx_rates })
}

/// Daily bars for one symbol from `from` onward, shaped for the hindsight
/// engine. The floor keeps the query to the span actually being measured
/// instead of every bar the symbol has ever had.
pub(crate) fn load_bars_since(conn: &Connection, symbol: &str, from: &str) -> Vec<hindsight::Bar> {
    let mut stmt = match conn.prepare(
        "SELECT date, high, low, close FROM prices
          WHERE symbol = ?1 AND date >= ?2 AND close IS NOT NULL
          ORDER BY date",
    ) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    stmt.query_map(params![symbol, from], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<f64>>(1)?,
            row.get::<_, Option<f64>>(2)?,
            row.get::<_, f64>(3)?,
        ))
    })
    .map(|rows| {
        rows.flatten()
            .filter_map(|(date, high, low, close)| {
                Some(hindsight::Bar {
                    date: NaiveDate::parse_from_str(&date, "%Y-%m-%d").ok()?,
                    high,
                    low,
                    close,
                })
            })
            .collect()
    })
    .unwrap_or_default()
}

/// One sale, and what the shares did afterwards.
#[derive(Serialize)]
pub(crate) struct HindsightRow {
    pub(crate) symbol: String,
    /// Native throughout — the row never converts, so the client must not
    /// assume AUD when rendering it.
    pub(crate) currency: String,
    pub(crate) sale_date: String,
    pub(crate) quantity: f64,
    pub(crate) sale_price: f64,
    /// FIFO cost of the shares this sale consumed, brokerage included. `None`
    /// where the matching purchase was never recorded.
    pub(crate) purchase_price: Option<f64>,
    /// What the trade itself made, per share, as a percentage. The six points
    /// are measured against the sale price; this is the only figure looking
    /// backward to the purchase.
    pub(crate) realised_pct: Option<f64>,
    pub(crate) delisted_on: Option<String>,
    pub(crate) points: hindsight::Points,
}

/// Every sale, priced at six later moments.
///
/// Built on `build_portfolio_context` rather than its own price lookup so the
/// "current" column is literally the same number the Holdings screen shows. A
/// second resolver would be free to drift, and a screen whose whole purpose is
/// comparison cannot afford to disagree with the one it is compared against.
#[utoipa::path(get, path = "/api/v1/hindsight", tag = "portfolio", responses((status = 200, description = "Sold positions priced at six later moments")))]
#[get("/api/hindsight")]
pub(crate) async fn get_hindsight(db_path: web::Data<PathBuf>) -> impl Responder {
    let ctx = match build_portfolio_context(&db_path).await {
        Ok(c) => c,
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "hindsight", "api", None, &err);
            return err_internal(err);
        }
    };
    let conn = match open_db(db_path.as_ref()) {
        Ok(c) => c,
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "hindsight", "api", None, &err.to_string());
            return err_internal(err.to_string());
        }
    };
    let dead = dead_symbols(&conn);
    let today = today_local();

    let mut rows: Vec<HindsightRow> = Vec::new();
    for (symbol, txs) in &ctx.groups {
        let sales = portfolio::sale_costs(txs);
        if sales.is_empty() {
            continue;
        }
        // One read per symbol, floored at its earliest sale less the lookback
        // window, so a weekend target still finds the Friday before it.
        let floor = sales
            .iter()
            .map(|s| s.date.as_str())
            .min()
            .and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
            .and_then(|d| d.checked_sub_days(chrono::Days::new(7)))
            .map(|d| d.to_string())
            .unwrap_or_default();
        let bars = load_bars_since(&conn, symbol, &floor);

        let current = ctx.prices.get(symbol).and_then(|p| p.native);
        let delisted_on = dead
            .get(symbol)
            .and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok());

        for sale in sales {
            let Ok(sale_date) = NaiveDate::parse_from_str(&sale.date, "%Y-%m-%d") else {
                let _ = insert_event_log(&db_path, "warn", "hindsight", "api", Some(symbol),
                    &format!("Sale dated '{}' is not a usable date and was skipped", sale.date));
                continue;
            };
            let s = hindsight::Sale {
                date: sale_date,
                price: sale.native_price,
                quantity: sale.quantity,
            };
            rows.push(HindsightRow {
                symbol: symbol.clone(),
                currency: ctx.currency_of(symbol),
                sale_date: sale.date.clone(),
                quantity: sale.quantity,
                sale_price: sale.native_price,
                purchase_price: sale.native_cost_per_share,
                realised_pct: sale
                    .native_cost_per_share
                    .filter(|c| *c > 0.0)
                    .map(|cost| (sale.native_price - cost) / cost * 100.0),
                delisted_on: delisted_on.map(|d| d.to_string()),
                points: hindsight::price_points(&s, &bars, current, today, delisted_on),
            });
        }
    }

    // Most recent sale first: the trades still worth second-guessing are the
    // ones whose windows are still filling in.
    rows.sort_by(|a, b| b.sale_date.cmp(&a.sale_date).then_with(|| a.symbol.cmp(&b.symbol)));
    HttpResponse::Ok().json(rows)
}

#[utoipa::path(get, path = "/api/v1/portfolio/holdings", tag = "portfolio", responses((status = 200, description = "Get portfolio holdings")))]
#[get("/api/portfolio/holdings")]
pub(crate) async fn get_portfolio_holdings(db_path: web::Data<PathBuf>) -> impl Responder {
    let ctx = match build_portfolio_context(&db_path).await {
        Ok(c) => c,
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "portfolio_fetch", "api", None, &err);
            return err_internal(err);
        }
    };
    let conn = match open_db(db_path.as_ref()) {
        Ok(c) => c,
        Err(err) => return err_internal(err.to_string()),
    };

    let mut holdings = Vec::new();
    for (symbol, txs) in &ctx.groups {
        let summary = portfolio::calc_symbol_summary(txs);
        if summary.remaining_shares <= 0.0 {
            continue;
        }
        let dividends = portfolio::symbol_dividends(&portfolio::sort_transactions(txs));
        let ep = ctx.prices.get(symbol);
        let price_aud = ep.and_then(|p| p.aud);
        let current_value = price_aud.filter(|p| *p != 0.0).map(|p| summary.remaining_shares * p).unwrap_or(0.0);
        let sym_fields = ctx.fields.get(symbol);
        let fields: HashMap<&String, &String> = sym_fields
            .map(|f| f.iter().filter(|(k, _)| k.as_str() != "_notes").collect())
            .unwrap_or_default();
        // Stored bars are native, so the averages are too: reported both ways,
        // like the price, so a holding can be read in the currency it trades in.
        let native_sma50 = stored_sma(&conn, symbol, 50);
        let sma50 = native_sma50.and_then(|v| ctx.to_aud(symbol, v));
        let native_sma150 = stored_sma(&conn, symbol, 150);
        let sma150 = native_sma150.and_then(|v| ctx.to_aud(symbol, v));
        let native_ema40w = stored_weekly_ema(&conn, symbol, 40);
        let ema40w = native_ema40w.and_then(|v| ctx.to_aud(symbol, v));
        let itype = ctx.info.get(symbol).and_then(|i| i.0.clone());
        // Effective stop loss (manual field, or the trailing-sell trigger) in
        // the symbol's native currency, matching native_current_price.
        let (stop_loss, is_trailing_sell) =
            match effective_stop_loss(&conn, symbol, sym_fields, ep.and_then(|p| p.native), |p| p) {
                Some((sl, trailing)) => (Some(sl), trailing),
                None => (None, false),
            };

        let (invested, dividends, rebased) = effective_basis(
            &ctx,
            symbol,
            txs,
            summary.remaining_shares,
            summary.remaining_cost,
            dividends,
        );
        let (avg_cost, native_avg_cost) = match rebased.as_ref() {
            Some((_, native)) => (ctx.to_aud(symbol, *native), Some(*native)),
            None => (
                (summary.remaining_shares > 0.0)
                    .then(|| summary.remaining_cost / summary.remaining_shares),
                (summary.remaining_shares > 0.0)
                    .then(|| summary.native_remaining_cost / summary.remaining_shares),
            ),
        };
        let pl = current_value - invested + dividends;
        holdings.push(serde_json::json!({
            "symbol": symbol,
            "long_name": ctx.info.get(symbol).and_then(|i| i.1.clone()),
            "instrument_type": itype,
            "is_etf": ctx.etf.get(symbol).copied().unwrap_or(false),
            "is_international": ctx.intl.get(symbol).copied().unwrap_or(false),
            "currency": ctx.currency_of(symbol),
            "sector": ctx.sector_of(symbol),
            "notes": sym_fields.and_then(|f| f.get("_notes").cloned()),
            "fields": fields,
            "shares": summary.remaining_shares,
            "invested": invested,
            "avg_cost": avg_cost,
            "native_avg_cost": native_avg_cost,
            "current_price": price_aud,
            "native_current_price": ep.and_then(|p| p.native),
            "price_source": ep.map(|p| p.source).unwrap_or("none"),
            // A price that exists but could not be converted: no rate for the
            // currency. Its AUD figures are absent rather than wrong.
            "fx_missing": ep.is_some_and(|p| p.native.is_some() && p.aud.is_none()),
            "price_date": ep.and_then(|p| p.price_date.clone()),
            "change": ep.and_then(|p| p.change),
            "change_percent": ep.and_then(|p| p.change_percent),
            // The quote's change is per share in the stock's own currency, so
            // the day's money is converted here — a screen totalling it adds
            // up holdings in several currencies at once.
            "day_pl": ep
                .and_then(|p| p.change)
                .and_then(|c| ctx.to_aud(symbol, c))
                .map(|c| summary.remaining_shares * c),
            "volume": ep.and_then(|p| p.volume),
            "current_value": current_value,
            "dividends": dividends,
            "pl": pl,
            "pl_pct": if invested > 0.0 { Some(pl / invested * 100.0) } else { None },
            // Present when the figures above are measured from a baseline
            // rather than from the purchase, so the client can say which.
            "basis_date": rebased.as_ref().map(|(date, _)| date.clone()),
            "basis_price": rebased.as_ref().map(|(_, native)| *native),
            "sma50": sma50,
            "native_sma50": native_sma50,
            "sma150": sma150,
            "native_sma150": native_sma150,
            "ema40w": ema40w,
            "native_ema40w": native_ema40w,
            "stop_loss": stop_loss,
            "is_trailing_sell": is_trailing_sell,
        }));
    }

    HttpResponse::Ok().json(serde_json::json!({ "holdings": holdings, "fx_rates": ctx.fx_rates }))
}

/// Effective stop-loss for a holding: the manual `stop_loss` symbol field if
/// set, otherwise the trailing-sell trigger — the highest close since
/// `trailing_sell_date` (plus the current price) minus `trailing_sell_pct`.
/// Returns `(price, is_trailing)`. `convert` maps stored native closes into
/// the caller's currency and must match the currency of `current_price`.
pub(crate) fn effective_stop_loss(
    conn: &Connection,
    symbol: &str,
    sym_fields: Option<&std::collections::HashMap<String, String>>,
    current_price: Option<f64>,
    convert: impl Fn(f64) -> f64,
) -> Option<(f64, bool)> {
    let manual = sym_fields
        .and_then(|f| f.get("stop_loss"))
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|v| *v != 0.0);
    // The manual stop is entered in the stock's own trading currency, like the
    // closes the trailing stop is built from, so both go through `convert`.
    // Returning it unconverted made one figure mean two things: the holdings
    // card read 100 as USD while the risk screen, displaying an AUD-purchased
    // US stock in AUD, read the same 100 as AUD.
    if let Some(sl) = manual {
        return Some((convert(sl), false));
    }
    let pct = sym_fields
        .and_then(|f| f.get("trailing_sell_pct"))
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|v| *v > 0.0)?;
    let mut reference = current_price;
    if let Some(since) = sym_fields.and_then(|f| f.get("trailing_sell_date")).filter(|d| !d.is_empty()) {
        // A trailing stop ratchets on the highest price actually *reached*, so
        // the peak is taken from the intraday high — that is what brokers trail
        // on. Using the close instead understates the peak whenever a bar spikes
        // and gives back the gain (TXG: high 60.69 vs close 58.48, a $1.80
        // difference in the resulting stop).
        //
        // COALESCE falls back to the close for bars predating OHLC ingest that
        // the backfill could not reach, so a missing high degrades to the old
        // behaviour for that bar rather than dropping it from the peak.
        let mut peaks: Vec<f64> = conn
            .prepare(
                "SELECT COALESCE(high, close) FROM prices
                  WHERE symbol = ?1 AND COALESCE(high, close) IS NOT NULL AND date >= ?2",
            )
            .ok()
            .and_then(|mut stmt| {
                stmt.query_map(params![symbol, since], |row| row.get::<_, f64>(0))
                    .ok()
                    .map(|r| r.flatten().map(&convert).collect())
            })
            .unwrap_or_default();
        if let Some(c) = current_price {
            peaks.push(c);
        }
        if !peaks.is_empty() {
            reference = peaks.into_iter().reduce(f64::max);
        }
    }
    let r = reference.filter(|r| *r != 0.0)?;
    Some((r * (1.0 - pct / 100.0), true))
}

#[derive(Deserialize)]
pub(crate) struct PortfolioOverviewQuery {
    /// Per-list sort override, as comma-separated `list_key:asc|desc` pairs
    /// (e.g. `stop_losses:desc`). Overrides the `sort` in each list's config.
    ///
    /// This has to be a server-side concern: each list is ranked and then
    /// truncated to its `limit` before being sent, so reversing the order in
    /// the browser would only reverse the rows that survived the cut. Sorting
    /// here changes *which* rows are selected.
    pub(crate) list_sort: Option<String>,
}

/// Parse a `list_sort` parameter into list_key → direction. Unknown or
/// malformed pairs are ignored so a bad query degrades to configured order
/// rather than failing the whole dashboard.
pub(crate) fn parse_list_sort(raw: Option<&str>) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let Some(raw) = raw else { return out };
    for pair in raw.split(',') {
        if let Some((key, dir)) = pair.split_once(':') {
            let dir = dir.trim().to_ascii_lowercase();
            if dir == "asc" || dir == "desc" {
                out.insert(key.trim().to_string(), dir);
            }
        }
    }
    out
}

#[utoipa::path(get, path = "/api/v1/portfolio/overview", tag = "portfolio", responses((status = 200, description = "Get portfolio overview")))]
#[get("/api/portfolio/overview")]
pub(crate) async fn get_portfolio_overview(
    db_path: web::Data<PathBuf>,
    query: web::Query<PortfolioOverviewQuery>,
) -> impl Responder {
    let list_sort_overrides = parse_list_sort(query.list_sort.as_deref());
    let ctx = match build_portfolio_context(&db_path).await {
        Ok(c) => c,
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "portfolio_fetch", "api", None, &err);
            return err_internal(err);
        }
    };

    #[derive(Default)]
    struct Agg {
        count: usize,
        value: f64,
        dividends: f64,
        pl: f64,
        cost: f64,
    }
    impl Agg {
        fn add(&mut self, value: f64, dividends: f64, pl: f64, cost: f64) {
            self.count += 1;
            self.value += value;
            self.dividends += dividends;
            self.pl += pl;
            self.cost += cost;
        }
        fn json(&self) -> serde_json::Value {
            serde_json::json!({ "count": self.count, "value": self.value, "dividends": self.dividends, "pl": self.pl, "cost": self.cost })
        }
    }

    let mut holdings_agg = Agg::default();
    let mut equity_agg = Agg::default();
    let mut etf_agg = Agg::default();
    let mut sector_aggs: HashMap<String, Agg> = HashMap::new();
    let mut sold_agg = Agg::default();
    let mut sold_pl = 0.0;

    for (symbol, txs) in &ctx.groups {
        let pos = portfolio::calc_symbol_position(txs);

        if pos.remaining_shares > 0.0 {
            let price = ctx.prices.get(symbol).and_then(|p| p.aud).filter(|p| *p != 0.0);
            let current_value = price.map(|p| pos.remaining_shares * p).unwrap_or(0.0);
            // Same basis the Holdings screen reports, so the totals here are the
            // sum of what that screen shows rather than a second opinion.
            let (cost, dividends, _) = effective_basis(
                &ctx,
                symbol,
                txs,
                pos.remaining_shares,
                pos.remaining_cost,
                pos.dividends,
            );
            let sym_pl = current_value - cost + dividends;
            holdings_agg.add(current_value, dividends, sym_pl, cost);
            if ctx.etf.get(symbol).copied().unwrap_or(false) {
                etf_agg.add(current_value, dividends, sym_pl, cost);
            } else {
                equity_agg.add(current_value, dividends, sym_pl, cost);
            }
            let sector = ctx.sector_of(symbol).unwrap_or_else(|| "Unallocated".to_string());
            sector_aggs.entry(sector).or_default().add(current_value, dividends, sym_pl, cost);
        }

        let sym_sold_pl = pos.sold_pl();
        if pos.sold_proceeds > 0.0 {
            sold_agg.add(
                pos.sold_proceeds,
                pos.sold_dividends,
                sym_sold_pl,
                pos.sold_proceeds - sym_sold_pl + pos.sold_dividends,
            );
        }
        sold_pl += sym_sold_pl;
    }

    let mut sectors: Vec<(String, Agg)> = sector_aggs.into_iter().collect();
    sectors.sort_by(|a, b| b.1.value.partial_cmp(&a.1.value).unwrap_or(std::cmp::Ordering::Equal));
    let sectors_json: Vec<serde_json::Value> = sectors
        .into_iter()
        .map(|(name, agg)| {
            let mut v = agg.json();
            v["name"] = serde_json::json!(name);
            v
        })
        .collect();

    // ------------------------------------------------------------------
    // Dashboard lists — previously computed in the browser from N price
    // history requests; now derived server-side.
    // ------------------------------------------------------------------
    let config: HashMap<String, String> = load_config(&db_path)
        .map(|c| c.into_iter().map(|i| (i.key, i.value)).collect())
        .unwrap_or_default();

    // Worst holdings vs their 150-day SMA
    let mut worst_holdings: Vec<serde_json::Value> = Vec::new();
    if let Ok(conn) = open_db(db_path.as_ref()) {
        let mut scored: Vec<(f64, serde_json::Value)> = Vec::new();
        for (symbol, txs) in &ctx.groups {
            let pos = portfolio::calc_symbol_position(txs);
            if pos.remaining_shares <= 0.0 {
                continue;
            }
            let price = ctx.prices.get(symbol).and_then(|p| p.aud).filter(|p| *p != 0.0);
            let sma150 = stored_sma(&conn, symbol, 150).and_then(|v| ctx.to_aud(symbol, v));
            if let (Some(p), Some(s)) = (price, sma150)
                && s != 0.0 {
                    let pct = (p - s) / s * 100.0;
                    scored.push((pct, serde_json::json!({ "symbol": symbol, "price": p, "sma150": s, "pct_diff": pct })));
                }
        }
        scored.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        worst_holdings = scored.into_iter().take(15).map(|(_, v)| v).collect();
    }

    // Watchlist rows are needed for both best-watchlist and custom lists
    let watchlist_rows = load_watchlist_symbols(&db_path, None).unwrap_or_default();
    let mut watch_unique: Vec<String> = Vec::new();
    let mut watch_seen = std::collections::HashSet::new();
    for r in &watchlist_rows {
        if watch_seen.insert(r.symbol.clone()) {
            watch_unique.push(r.symbol.clone());
        }
    }
    let watch_prices: HashMap<String, CurrentPrice> = load_cached_prices(&db_path, &watch_unique)
        .unwrap_or_default()
        .into_iter()
        .map(|p| (p.symbol.clone(), p))
        .collect();

    // Best watchlist — most recently crossed above their 50-day SMA
    let histories = fetch_histories(&db_path, &watch_unique, 300).await;
    let mut best: Vec<(i64, serde_json::Value)> = Vec::new();
    {
        use stocks::indicators as ind;
        for sym in &watch_unique {
            let Some(p) = watch_prices.get(sym).and_then(|x| x.price) else { continue };
            let Some(hist) = histories.get(sym) else { continue };
            let points = indicator_points(hist);
            let sma50_arr = ind::calculate_sma(&points, 50);
            let Some(sma50) = ind::latest_sma(&sma50_arr) else { continue };
            if p <= sma50 {
                continue;
            }
            let stats = ind::crossover_stats(&points, &sma50_arr, watch_prices.get(sym).and_then(|x| x.volume));
            best.push((stats.days, serde_json::json!({
                "symbol": sym,
                "price": p,
                "sma50": sma50,
                "sma50_trend": ind::sma_trend(&sma50_arr, 5),
                "days_since_50sma": stats.days,
                "volume_pct_50sma": stats.volume_pct,
            })));
        }
    }
    best.sort_by_key(|(days, _)| *days);
    let best_watchlist: Vec<serde_json::Value> = best.into_iter().take(15).map(|(_, v)| v).collect();

    // Custom dashboard lists: price vs a user-defined field
    #[derive(Deserialize)]
    struct DashboardListDef {
        key: String,
        label: String,
        source: String,
        field_key: String,
        operator: String,
        /// What the field is compared against: "price" (default) or "volume".
        /// Volume is a raw share count with no currency, so it is never
        /// converted the way a price is.
        #[serde(default)]
        compare: Option<String>,
        #[serde(default)]
        limit: Option<usize>,
        #[serde(default)]
        sort: Option<String>,
    }
    #[derive(Deserialize)]
    struct FieldDef {
        key: String,
        label: String,
    }
    let list_defs: Vec<DashboardListDef> = config_json(&db_path, &config, "dashboard_custom_lists");
    let holdings_field_defs: Vec<FieldDef> = config_json(&db_path, &config, "holdings_custom_fields");
    let watchlist_field_defs: Vec<FieldDef> = config_json(&db_path, &config, "watchlist_custom_fields");

    // Needed to derive trailing stop-loss triggers for the stop_loss lists;
    // on failure those lists degrade to manual stop losses only.
    let list_conn = match open_db(db_path.as_ref()) {
        Ok(c) => Some(c),
        Err(err) => {
            let _ = insert_event_log(&db_path, "warn", "portfolio_fetch", "api", None, &format!("Custom lists: DB unavailable for trailing stop losses: {}", err));
            None
        }
    };
    // Crossover operators need the whole aligned series, not just the latest
    // indicator value, so their history is read up front — but only when a list
    // actually asks for one, leaving every other dashboard load untouched.
    const CROSS_OPS: [&str; 3] = ["days_above", "days_below", "volume_cross_pct"];
    let cross_defs: Vec<&DashboardListDef> = list_defs
        .iter()
        .filter(|d| CROSS_OPS.contains(&d.operator.as_str()))
        .collect();
    // A 40-week EMA needs years of bars to settle; the same lookback as
    // `stored_weekly_ema` keeps a "days above" list agreeing with the plain
    // "above" list on the same indicator.
    let cross_window: i64 = if cross_defs.iter().any(|d| d.field_key.ends_with(":ema40w")) { 3000 } else { 600 };
    let mut cross_symbols: std::collections::HashSet<String> = std::collections::HashSet::new();
    for def in &cross_defs {
        if def.source == "holdings" || def.source == "both" {
            for (symbol, txs) in &ctx.groups {
                if portfolio::calc_symbol_position(txs).remaining_shares > 0.0 {
                    cross_symbols.insert(symbol.clone());
                }
            }
        }
        if def.source == "watchlist" || def.source == "both" {
            cross_symbols.extend(watch_unique.iter().cloned());
        }
    }
    let cross_histories: HashMap<String, Vec<PriceHistoryPoint>> = match list_conn.as_ref() {
        Some(conn) => cross_symbols
            .iter()
            .map(|symbol| (symbol.clone(), load_local_history(conn, symbol, cross_window)))
            .collect(),
        None => HashMap::new(),
    };
    if !cross_defs.is_empty() && cross_histories.is_empty() {
        let _ = insert_event_log(&db_path, "warn", "portfolio_fetch", "api", None, "Crossover lists configured but no price history could be read; those lists will be empty");
    }

    let cross_stats = |symbol: &str,
                       field_key: &str,
                       constant: Option<f64>,
                       direction: stocks::indicators::CrossDirection,
                       today_volume: Option<i64>|
     -> Option<stocks::indicators::CrossoverStats> {
        let history = cross_histories.get(symbol)?;
        let reference = crossover_reference(history, field_key, constant)?;
        let points = indicator_points(history);
        Some(stocks::indicators::crossover_stats_dir(&points, &reference, today_volume, direction))
    };

    // Indicators are read per symbol from `prices`, and a weekly EMA sweeps
    // thousands of rows. Two lists on the same indicator would pay that twice,
    // so results are memoised for the life of the request.
    let indicator_cache: std::cell::RefCell<HashMap<(String, String), Option<f64>>> =
        std::cell::RefCell::new(HashMap::new());
    let cached_indicator = |symbol: &str, key: &str| -> Option<f64> {
        let cache_key = (symbol.to_string(), key.to_string());
        if let Some(hit) = indicator_cache.borrow().get(&cache_key) {
            return *hit;
        }
        let value = list_conn.as_ref().and_then(|conn| indicator_value(conn, symbol, key));
        indicator_cache.borrow_mut().insert(cache_key, value);
        value
    };

    let custom_lists: Vec<serde_json::Value> = list_defs
        .iter()
        .map(|def| {
            let (field_source, field_key) = def.field_key.split_once(':').unwrap_or(("", ""));
            // An indicator is derived from price history, so it exists for any
            // symbol regardless of which table the symbol lives in. `source`
            // alone then decides which branches run.
            let is_indicator = field_source == "indicator";
            struct Entry {
                symbol: String,
                price: f64,
                /// The side of the comparison the diff is measured from —
                /// equal to `price` unless the list compares volume.
                compare_value: f64,
                /// Which table this symbol came from. Per entry rather than per
                /// list because an indicator list sourced from "both" mixes
                /// holdings and watchlist rows, and each navigates elsewhere.
                origin: &'static str,
                /// Trading days since the crossing, on a crossover list only.
                days: Option<i64>,
                /// Volume on the crossing day vs the preceding 20-day average.
                volume_cross_pct: Option<f64>,
                field_value: f64,
                diff: f64,
                pct_diff: f64,
                currency: Option<String>,
                is_trailing: bool,
            }
            let compare_volume = def.compare.as_deref() == Some("volume");
            // "Volume on cross" is the breakout case measured a different way,
            // so it shares a direction with days_above and differs only in what
            // the list is ranked by.
            let cross_dir = match def.operator.as_str() {
                "days_above" | "volume_cross_pct" => Some(stocks::indicators::CrossDirection::Above),
                "days_below" => Some(stocks::indicators::CrossDirection::Below),
                _ => None,
            };
            // The column the list is ranked by, so the client knows which
            // header to make sortable and which figures to show.
            let metric = match def.operator.as_str() {
                "days_above" | "days_below" => "days",
                "volume_cross_pct" => "volume_cross_pct",
                _ => "pct_diff",
            };
            let matches_op = |diff: f64| match def.operator.as_str() {
                "above" | "pct_below" | "days_above" | "volume_cross_pct" => diff > 0.0,
                "below" | "pct_above" | "days_below" => diff < 0.0,
                _ => false,
            };
            let mut entries: Vec<Entry> = Vec::new();

            if (def.source == "holdings" || def.source == "both") && (is_indicator || field_source == "holdings") {
                for (symbol, txs) in &ctx.groups {
                    let pos = portfolio::calc_symbol_position(txs);
                    if pos.remaining_shares <= 0.0 {
                        continue;
                    }
                    let Some(price) = ctx.prices.get(symbol).and_then(|p| p.native) else { continue };
                    // A volume list still needs the price above: the stop-loss
                    // fallback below is priced, and the price stays on the
                    // entry as context.
                    let compare_value = if compare_volume {
                        match ctx.prices.get(symbol).and_then(|p| p.volume).filter(|v| *v > 0) {
                            Some(v) => v as f64,
                            None => continue,
                        }
                    } else {
                        price
                    };
                    // The built-in stop_loss field falls back to the
                    // trailing-sell trigger, so holdings protected by a
                    // trailing stop appear in stop-loss lists too. Prices
                    // here are native, so closes need no conversion.
                    let (fv, is_trailing) = if is_indicator {
                        match cached_indicator(symbol, field_key).filter(|v| *v > 0.0) {
                            Some(v) => (v, false),
                            None => continue,
                        }
                    } else if field_key == "stop_loss" && let Some(conn) = list_conn.as_ref() {
                        match effective_stop_loss(conn, symbol, ctx.fields.get(symbol), Some(price), |p| p) {
                            Some((sl, trailing)) if sl > 0.0 => (sl, trailing),
                            _ => continue,
                        }
                    } else {
                        let Some(fv) = ctx.fields.get(symbol).and_then(|f| f.get(field_key)).and_then(|v| v.parse::<f64>().ok()).filter(|v| *v > 0.0) else { continue };
                        (fv, false)
                    };
                    let diff = compare_value - fv;
                    if matches_op(diff) {
                        // A row that cannot be dated has nothing to rank on, so
                        // it is dropped rather than shown with a blank column.
                        let (days, volume_cross_pct) = match cross_dir {
                            Some(dir) => {
                                let constant = if is_indicator { None } else { Some(fv) };
                                let volume = ctx.prices.get(symbol).and_then(|p| p.volume);
                                match cross_stats(symbol, field_key, constant, dir, volume) {
                                    Some(stats) => (Some(stats.days), stats.volume_pct),
                                    None => continue,
                                }
                            }
                            None => (None, None),
                        };
                        entries.push(Entry {
                            symbol: symbol.clone(),
                            price,
                            compare_value,
                            field_value: fv,
                            diff,
                            pct_diff: diff / fv * 100.0,
                            currency: ctx.info.get(symbol).and_then(|i| i.2.clone()),
                            is_trailing,
                            origin: "holdings",
                            days,
                            volume_cross_pct,
                        });
                    }
                }
            }

            if (def.source == "watchlist" || def.source == "both") && (is_indicator || field_source == "watchlist") {
                for row in &watchlist_rows {
                    if entries.iter().any(|e| e.symbol == row.symbol) {
                        continue;
                    }
                    let Some(price) = watch_prices.get(&row.symbol).and_then(|p| p.price) else { continue };
                    let compare_value = if compare_volume {
                        match watch_prices.get(&row.symbol).and_then(|p| p.volume).filter(|v| *v > 0) {
                            Some(v) => v as f64,
                            None => continue,
                        }
                    } else {
                        price
                    };
                    let fv = if is_indicator {
                        cached_indicator(&row.symbol, field_key)
                    } else {
                        match field_key {
                            "breakthrough_price" => row.breakthrough_price,
                            "stop_loss_price" => row.stop_loss_price,
                            _ => row.custom_fields.get(field_key).and_then(|v| v.parse::<f64>().ok()),
                        }
                    };
                    let Some(fv) = fv.filter(|v| *v > 0.0) else { continue };
                    let diff = compare_value - fv;
                    if matches_op(diff) {
                        let (days, volume_cross_pct) = match cross_dir {
                            Some(dir) => {
                                let constant = if is_indicator { None } else { Some(fv) };
                                let volume = watch_prices.get(&row.symbol).and_then(|p| p.volume);
                                match cross_stats(&row.symbol, field_key, constant, dir, volume) {
                                    Some(stats) => (Some(stats.days), stats.volume_pct),
                                    None => continue,
                                }
                            }
                            None => (None, None),
                        };
                        entries.push(Entry {
                            symbol: row.symbol.clone(),
                            price,
                            compare_value,
                            field_value: fv,
                            diff,
                            pct_diff: diff / fv * 100.0,
                            currency: None,
                            is_trailing: false,
                            origin: "watchlist",
                            days,
                            volume_cross_pct,
                        });
                    }
                }
            }

            // A request-time override beats the list's configured direction, so
            // clicking the Difference header re-ranks before the truncate below
            // and can surface rows that were previously cut.
            let sort_dir = list_sort_overrides
                .get(&def.key)
                .map(|s| s.as_str())
                .or(def.sort.as_deref());
            let pct_op = def.operator == "pct_above" || def.operator == "pct_below";
            entries.sort_by(|a, b| {
                // A missing figure sorts last in the requested direction rather
                // than drifting to the top of a reversed list.
                let cmp = match metric {
                    "days" => a.days.unwrap_or(i64::MAX).cmp(&b.days.unwrap_or(i64::MAX)),
                    "volume_cross_pct" => a
                        .volume_cross_pct
                        .unwrap_or(f64::MAX)
                        .partial_cmp(&b.volume_cross_pct.unwrap_or(f64::MAX))
                        .unwrap_or(std::cmp::Ordering::Equal),
                    _ if pct_op => a.pct_diff.abs().partial_cmp(&b.pct_diff.abs()).unwrap_or(std::cmp::Ordering::Equal),
                    _ => a.pct_diff.partial_cmp(&b.pct_diff).unwrap_or(std::cmp::Ordering::Equal),
                };
                if sort_dir == Some("desc") { cmp.reverse() } else { cmp }
            });
            let limit = def.limit.unwrap_or(15);
            let truncated = entries.len() > limit;
            entries.truncate(limit);

            let builtin_labels: HashMap<&str, &str> = HashMap::from([
                ("sma50", "50-Day SMA"),
                ("sma150", "150-Day SMA"),
                ("ema40w", "40-Week EMA"),
                ("breakthrough_price", "Breakthrough Price"),
                ("stop_loss_price", "Stop Loss Price"),
                ("stop_loss", "Stop Loss Price"),
                ("trailing_sell_pct", "Trailing Sell %"),
            ]);
            let field_defs = if field_source == "holdings" { &holdings_field_defs } else { &watchlist_field_defs };
            let field_label = field_defs
                .iter()
                .find(|f| f.key == field_key)
                .map(|f| f.label.clone())
                .or_else(|| builtin_labels.get(field_key).map(|s| s.to_string()))
                .unwrap_or_else(|| field_key.to_string());

            serde_json::json!({
                "key": def.key,
                "label": def.label,
                "source": def.source,
                // Where the entry symbols actually live (holdings vs watchlist),
                // derived from the field_key prefix. Drives click navigation.
                "field_source": field_source,
                "operator": def.operator,
                // Which side the diff is measured from, so the client can label
                // and format that column as a price or a share count.
                "compare": if compare_volume { "volume" } else { "price" },
                // Which column the rows are ranked by: "pct_diff", "days" or
                // "volume_cross_pct".
                "metric": metric,
                "field_label": field_label,
                // The direction actually applied — the request override if one
                // was given, else the list's config, else the "asc" default. The
                // client renders its sort indicator from this rather than
                // assuming, so the arrow is right on first load too.
                "sort": sort_dir.unwrap_or("asc"),
                // True when more rows qualified than `limit` allowed through, so
                // the UI can say the view is truncated rather than complete.
                "truncated": truncated,
                "entries": entries.iter().map(|e| serde_json::json!({
                    "symbol": e.symbol,
                    "price": e.price,
                    "compare_value": e.compare_value,
                    "field_value": e.field_value,
                    "diff": e.diff,
                    "pct_diff": e.pct_diff,
                    "currency": e.currency,
                    "is_trailing": e.is_trailing,
                    // Per-entry so a "both"-sourced indicator list sends each
                    // row to the screen its symbol actually lives on.
                    "origin": e.origin,
                    "days": e.days,
                    "volume_cross_pct": e.volume_cross_pct,
                })).collect::<Vec<_>>(),
            })
        })
        .collect();

    HttpResponse::Ok().json(serde_json::json!({
        "totals": {
            "stock_count": holdings_agg.count,
            "total_value": holdings_agg.value,
            "total_pl": holdings_agg.pl + sold_pl,
            "holdings_pl": holdings_agg.pl,
            "sold_pl": sold_pl,
        },
        "breakdowns": {
            "equities": equity_agg.json(),
            "etfs": etf_agg.json(),
            "holdings": holdings_agg.json(),
            "sold": {
                "count": sold_agg.count,
                "value": sold_agg.value,
                "dividends": sold_agg.dividends,
                "pl": sold_pl,
                "cost": sold_agg.cost,
            },
        },
        "sectors": sectors_json,
        "worst_holdings": worst_holdings,
        "best_watchlist": best_watchlist,
        "custom_lists": custom_lists,
    }))
}

#[utoipa::path(get, path = "/api/v1/portfolio/lots", tag = "portfolio", responses((status = 200, description = "Get portfolio lots")))]
#[get("/api/portfolio/lots")]
pub(crate) async fn get_portfolio_lots(db_path: web::Data<PathBuf>) -> impl Responder {
    let ctx = match build_portfolio_context(&db_path).await {
        Ok(c) => c,
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "portfolio_fetch", "api", None, &err);
            return err_internal(err);
        }
    };

    let all_txs: Vec<PortfolioTx> = ctx.groups.iter().flat_map(|(_, txs)| txs.clone()).collect();
    let remaining = portfolio::calc_remaining_by_lot(&all_txs);

    let mut lots = Vec::new();
    for (symbol, txs) in &ctx.groups {
        let price_aud = ctx.prices.get(symbol).and_then(|p| p.aud);
        for tx in txs {
            if tx.tx_type != TxType::Purchase {
                continue;
            }
            let rem = remaining.get(&tx.id).copied().unwrap_or(0.0);
            let (current_value, unrealised_pl) = match (price_aud, tx.price) {
                (Some(p), Some(cost)) if rem > 0.0 => {
                    let value = rem * p;
                    // Only the unsold part of the lot still carries its share
                    // of the fee; the rest went out with the shares sold.
                    let bought = tx.quantity.unwrap_or(rem).max(rem);
                    let brokerage = tx.brokerage.unwrap_or(0.0) * rem / bought;
                    (Some(value), Some(value - (rem * cost + brokerage)))
                }
                _ => (None, None),
            };
            lots.push(serde_json::json!({
                "transaction_id": tx.id,
                "symbol": symbol,
                "date": tx.date,
                "remaining": rem,
                "current_value": current_value,
                "unrealised_pl": unrealised_pl,
            }));
        }
    }

    HttpResponse::Ok().json(serde_json::json!({ "lots": lots }))
}

#[utoipa::path(get, path = "/api/v1/portfolio/sold", tag = "portfolio", responses((status = 200, description = "Get portfolio sold")))]
#[get("/api/portfolio/sold")]
pub(crate) async fn get_portfolio_sold(db_path: web::Data<PathBuf>) -> impl Responder {
    let rows = match fetch_holdings(&db_path) {
        Ok(r) => r,
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "portfolio_fetch", "api", None, &err);
            return err_internal(err);
        }
    };
    let txs = to_portfolio_txs(&rows);
    let mut entries: Vec<portfolio::SoldEntry> = Vec::new();
    for (_, group) in portfolio::group_by_symbol(&txs) {
        entries.extend(portfolio::calc_sold_entries(&group));
    }
    entries.sort_by(|a, b| b.date.cmp(&a.date));

    let total_realised_pl: f64 = entries.iter().map(|e| e.realised_pl).sum();
    let total_cost: f64 = entries.iter().map(|e| e.avg_purchase_price * e.quantity).sum();
    let entries_json: Vec<serde_json::Value> = entries
        .iter()
        .map(|e| serde_json::json!({
            "symbol": e.symbol,
            "date": e.date,
            "quantity": e.quantity,
            "avg_purchase_price": e.avg_purchase_price,
            "sale_price": e.sale_price,
            "brokerage": e.brokerage,
            "dividends": e.dividends,
            "days_held": e.days_held,
            "realised_pl": e.realised_pl,
        }))
        .collect();

    HttpResponse::Ok().json(serde_json::json!({
        "entries": entries_json,
        "total_realised_pl": total_realised_pl,
        "total_cost": total_cost,
    }))
}

/// Risk / stop-loss analysis per active holding — the server-side port of the
/// Analysis screen's row computation. Display-currency rule: a stock purchased
/// entirely in AUD is shown in AUD (market prices converted); otherwise it is
/// shown in its native trading currency.
#[utoipa::path(get, path = "/api/v1/portfolio/risk", tag = "portfolio", responses((status = 200, description = "Get portfolio risk")))]
#[get("/api/portfolio/risk")]
pub(crate) async fn get_portfolio_risk(db_path: web::Data<PathBuf>) -> impl Responder {
    let ctx = match build_portfolio_context(&db_path).await {
        Ok(c) => c,
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "portfolio_fetch", "api", None, &err);
            return err_internal(err);
        }
    };
    let conn = match open_db(db_path.as_ref()) {
        Ok(c) => c,
        Err(err) => return err_internal(err.to_string()),
    };

    let mut rows = Vec::new();
    let mut total_invested = 0.0;
    let mut total_sl_dollar = 0.0;

    for (symbol, txs) in &ctx.groups {
        let summary = portfolio::calc_symbol_summary(txs);
        if summary.remaining_shares <= 0.0 {
            continue;
        }
        let shares = summary.remaining_shares;
        let symbol_currency = ctx.currency_of(symbol);
        let all_aud = ctx.all_aud.get(symbol).copied().unwrap_or(true);
        let display_currency = if all_aud { "AUD".to_string() } else { symbol_currency.clone() };
        let is_foreign = display_currency != "AUD";
        let rate = ctx.fx_rates.get(&symbol_currency).copied().flatten().filter(|r| *r != 0.0);
        let needs_conversion = all_aud && symbol_currency != "AUD" && rate.is_some();
        let to_display = |p: f64| if needs_conversion { p * rate.unwrap() } else { p };

        // Average purchase price in the display currency: AUD lots when
        // purchased in AUD, native-currency lots otherwise.
        let purchase_price = if is_foreign {
            summary.native_remaining_cost / shares
        } else {
            summary.remaining_cost / shares
        };

        let current_price = ctx.prices.get(symbol).and_then(|p| p.native).map(&to_display);
        let pl_pct = match current_price {
            Some(c) if purchase_price != 0.0 && c != 0.0 => Some((c - purchase_price) / purchase_price * 100.0),
            _ => None,
        };

        let sym_fields = ctx.fields.get(symbol);
        let (stop_loss, is_trailing) = match effective_stop_loss(&conn, symbol, sym_fields, current_price, to_display) {
            Some((sl, trailing)) => (Some(sl), trailing),
            None => (None, false),
        };

        let stop_loss_pct = stop_loss
            .filter(|_| purchase_price != 0.0)
            .map(|sl| (sl - purchase_price) / purchase_price * 100.0);
        let sl_dollar_native = stop_loss
            .filter(|_| purchase_price != 0.0 && shares > 0.0)
            .map(|sl| (sl - purchase_price) * shares);
        let stop_loss_dollar = sl_dollar_native.map(|v| match (is_foreign, rate) {
            (true, Some(r)) => v * r,
            _ => v,
        });

        let sma50 = stored_sma(&conn, symbol, 50).map(&to_display);
        let sma150 = stored_sma(&conn, symbol, 150).map(&to_display);
        let ema40w = stored_weekly_ema(&conn, symbol, 40).map(&to_display);
        // Highest price *reached* over the window, so an intraday spike counts.
        // Taking the close instead understates it whenever a bar runs up and
        // gives the gain back — RMS.AX touched 3.79 on a day it closed at 3.67,
        // which put a real purchase at 3.76 above its own "30d High".
        //
        // The row filter stays on `close IS NOT NULL` so the window is still the
        // 30 most recent trading bars, and COALESCE falls back to the close for
        // bars the OHLC backfill could not reach.
        let high30d: Option<f64> = conn
            .prepare(
                "SELECT COALESCE(high, close) FROM prices
                  WHERE symbol = ?1 AND close IS NOT NULL
                  ORDER BY date DESC LIMIT 30",
            )
            .ok()
            .and_then(|mut stmt| {
                stmt.query_map(params![symbol], |row| row.get::<_, f64>(0))
                    .ok()
                    .and_then(|r| r.flatten().map(&to_display).reduce(f64::max))
            });

        let invested = if purchase_price != 0.0 && shares > 0.0 { purchase_price * shares } else { 0.0 };
        total_invested += invested;
        total_sl_dollar += stop_loss_dollar.unwrap_or(0.0);

        rows.push(serde_json::json!({
            "symbol": symbol,
            "currency": display_currency,
            // What the manual stop loss is entered in, which differs from the
            // display currency for a foreign stock bought in AUD.
            "native_currency": symbol_currency,
            "current_price": current_price,
            "purchase_price": if purchase_price != 0.0 { Some(purchase_price) } else { None },
            "pl_pct": pl_pct,
            "stop_loss": stop_loss,
            "is_trailing_sell": is_trailing,
            "stop_loss_pct": stop_loss_pct,
            "stop_loss_dollar": stop_loss_dollar,
            "sma50": sma50,
            "sma150": sma150,
            "ema40w": ema40w,
            "high30d": high30d,
            "total_invested": invested,
            // Needed to express the gap to the stop loss as a position-level
            // dollar amount. Deriving it client-side from total_invested /
            // purchase_price breaks whenever purchase_price is absent or zero.
            "shares": shares,
        }));
    }

    let total_sl_pct = if total_invested > 0.0 { Some(total_sl_dollar / total_invested * 100.0) } else { None };
    HttpResponse::Ok().json(serde_json::json!({
        "rows": rows,
        "totals": {
            "total_invested": total_invested,
            "total_sl_dollar": total_sl_dollar,
            "total_sl_pct": total_sl_pct,
        },
    }))
}

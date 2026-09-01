//! What a stock did *after* it was sold.
//!
//! The holdings screens answer "what did this position make", which stops at
//! the sale. This answers the other question — whether selling was the right
//! call — by pricing the same shares at six later moments and comparing each
//! against what they actually fetched.
//!
//! Price only. Dividends are deliberately excluded: they belong to whoever held
//! the shares, and mixing them in would answer a different question from the
//! one the screen asks.
//!
//! Everything here is native currency. A sale and its follow-up prices are the
//! same instrument on the same exchange, so converting both through a moving FX
//! rate would inject a currency move into a comparison that is meant to isolate
//! the stock's own.

use chrono::{Days, Months, NaiveDate};
use serde::Serialize;

/// One daily bar, with as much of it as the table actually holds.
///
/// `high` and `low` are optional because bars ingested before OHLC collection
/// began carry only a close. Those bars still count toward the peak and the
/// low — degrading to the close is far better than dropping the day.
#[derive(Debug, Clone)]
pub struct Bar {
    pub date: NaiveDate,
    pub high: Option<f64>,
    pub low: Option<f64>,
    pub close: f64,
}

/// The sale being judged. One row per sale, so a symbol bought and sold several
/// times is measured separately each time rather than averaged into a blur.
#[derive(Debug, Clone)]
pub struct Sale {
    pub date: NaiveDate,
    /// Price per share, as recorded on the transaction.
    pub price: f64,
    pub quantity: f64,
}

/// Why a point has no number, so the screen never has to guess.
///
/// The three empty cases are genuinely different to a reader: a window that has
/// not come round yet will fill in on its own, a delisted symbol's never will,
/// and a gap in the bars is a data problem worth fixing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// A real price from a real bar.
    Ok,
    /// The date has not arrived yet.
    Pending,
    /// The symbol stopped trading before this point could be reached.
    Delisted,
    /// The date has passed but no bar is close enough to answer for it.
    NoData,
}

#[derive(Debug, Clone, Serialize)]
pub struct PricePoint {
    pub value: Option<f64>,
    /// The date of the bar actually used, which for a weekend or holiday target
    /// is the trading day before it rather than the target itself.
    pub date: Option<String>,
    pub status: Status,
    /// Native-currency difference against the sale, across the shares sold.
    /// Positive is profit missed, negative is loss avoided.
    pub delta: Option<f64>,
    /// The same comparison per share, as a percentage of the sale price.
    pub pct: Option<f64>,
}

impl PricePoint {
    fn empty(status: Status) -> Self {
        PricePoint { value: None, date: None, status, delta: None, pct: None }
    }

    fn found(value: f64, date: Option<NaiveDate>, sale: &Sale, status: Status) -> Self {
        // A zero or negative sale price would make the percentage meaningless
        // rather than merely large, so it is left empty instead of infinite.
        let pct = if sale.price > 0.0 { Some((value - sale.price) / sale.price * 100.0) } else { None };
        PricePoint {
            value: Some(value),
            date: date.map(|d| d.to_string()),
            status,
            delta: Some((value - sale.price) * sale.quantity),
            pct,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Points {
    pub week1: PricePoint,
    pub week6: PricePoint,
    pub month3: PricePoint,
    pub peak: PricePoint,
    pub low: PricePoint,
    pub current: PricePoint,
}

/// How far back of a target date a bar may sit and still answer for it.
///
/// Targets land on weekends and public holidays, when the honest answer is the
/// last price the market actually made. Seven days covers the longest ordinary
/// closure without letting a window borrow from the one before it — the
/// closest pair, one week and six weeks, are 35 days apart.
///
/// It also does the work of a staleness bound: past a delisting the last real
/// bar would otherwise answer every future window identically, quietly
/// reporting a dead stock's final price as its price a year later.
const LOOKBACK_DAYS: u64 = 7;

/// The most recent bar at or before `target`, provided one is close enough and
/// actually falls after the sale.
///
/// Reads the way the question does: what was it worth on that date. On a
/// Saturday the last price the market made is Friday's close, so the search
/// looks backward rather than forward to the following Monday.
///
/// The sale date itself is excluded, and so is everything before it. A stock
/// that stops trading the day it is sold sits exactly one lookback window from
/// the one-week target, and without this its sale-day close would come back
/// dressed as the price a week later.
fn bar_at(bars: &[Bar], target: NaiveDate, after: NaiveDate) -> Option<&Bar> {
    let floor = target.checked_sub_days(Days::new(LOOKBACK_DAYS))?;
    bars.iter()
        .filter(|b| b.date <= target && b.date >= floor && b.date > after)
        .max_by_key(|b| b.date)
}

/// Price the sale at one later date.
fn point_at(
    sale: &Sale,
    bars: &[Bar],
    target: NaiveDate,
    today: NaiveDate,
    delisted_on: Option<NaiveDate>,
) -> PricePoint {
    // Delisting is checked before the calendar: a window past the last trading
    // day is not "not yet", it is never.
    if delisted_on.is_some_and(|end| target > end) {
        return PricePoint::empty(Status::Delisted);
    }
    if target > today {
        return PricePoint::empty(Status::Pending);
    }
    match bar_at(bars, target, sale.date) {
        Some(bar) => PricePoint::found(bar.close, Some(bar.date), sale, Status::Ok),
        None => PricePoint::empty(Status::NoData),
    }
}

/// The six points, measured from the sale.
///
/// `current` is the live quote where there is one, which is not redundant with
/// the bars: the day's bar holds the close so far, while the quote is what the
/// rest of the app shows. Both feed the peak and the low so a stock at its high
/// right now is not reported as having peaked yesterday.
pub fn price_points(
    sale: &Sale,
    bars: &[Bar],
    current: Option<f64>,
    today: NaiveDate,
    delisted_on: Option<NaiveDate>,
) -> Points {
    let after_sale: Vec<&Bar> = bars.iter().filter(|b| b.date > sale.date).collect();

    // The extremes span everything since the sale, so they are the one pair not
    // tied to a target date — they answer "at best" and "at worst", and the
    // date carried back is when that happened.
    // The third element records whether the extreme came from a real bar or
    // from the standing quote, which is what decides how it is labelled below.
    let mut best: Option<(f64, NaiveDate, bool)> = None;
    let mut worst: Option<(f64, NaiveDate, bool)> = None;
    for bar in &after_sale {
        let high = bar.high.unwrap_or(bar.close);
        let low = bar.low.unwrap_or(bar.close);
        if best.is_none_or(|(v, _, _)| high > v) {
            best = Some((high, bar.date, false));
        }
        if worst.is_none_or(|(v, _, _)| low < v) {
            worst = Some((low, bar.date, false));
        }
    }
    if let Some(price) = current.filter(|p| *p > 0.0) {
        // A delisted symbol's standing price is its last one, so it belongs on
        // the day it stopped trading rather than on today's date.
        let as_of = delisted_on.unwrap_or(today);
        if best.is_none_or(|(v, _, _)| price > v) {
            best = Some((price, as_of, true));
        }
        if worst.is_none_or(|(v, _, _)| price < v) {
            worst = Some((price, as_of, true));
        }
    }

    let extreme = |found: Option<(f64, NaiveDate, bool)>| match found {
        Some((value, date, from_quote)) => {
            // An extreme drawn from a real bar is real history and stays `Ok`
            // even for a symbol since delisted. One that is only the final
            // price is not a peak the stock ever traded through, and saying
            // `Ok` would present a dead stock's last print as its high.
            let status = if from_quote && delisted_on.is_some() { Status::Delisted } else { Status::Ok };
            PricePoint::found(value, Some(date), sale, status)
        }
        // No bar after the sale at all. For a symbol that stopped trading that
        // is the delisting showing through; otherwise the history is missing.
        None if delisted_on.is_some() => PricePoint::empty(Status::Delisted),
        None => PricePoint::empty(Status::NoData),
    };

    let target = |days: Option<NaiveDate>| days.unwrap_or(sale.date);
    Points {
        week1: point_at(sale, bars, target(sale.date.checked_add_days(Days::new(7))), today, delisted_on),
        week6: point_at(sale, bars, target(sale.date.checked_add_days(Days::new(42))), today, delisted_on),
        month3: point_at(sale, bars, target(sale.date.checked_add_months(Months::new(3))), today, delisted_on),
        peak: extreme(best),
        low: extreme(worst),
        current: current_point(sale, current, today, delisted_on),
    }
}

/// A delisted symbol has no current price. Whatever figure is available is its
/// last one, so it is reported with `Delisted` rather than dressed up as live —
/// the value is still worth showing, the label is what stops it misleading.
fn current_point(
    sale: &Sale,
    current: Option<f64>,
    today: NaiveDate,
    delisted_on: Option<NaiveDate>,
) -> PricePoint {
    let status = if delisted_on.is_some() { Status::Delisted } else { Status::Ok };
    match current.filter(|p| *p > 0.0) {
        Some(price) => {
            let date = delisted_on.unwrap_or(today);
            PricePoint::found(price, Some(date), sale, status)
        }
        None if delisted_on.is_some() => PricePoint::empty(Status::Delisted),
        None => PricePoint::empty(Status::NoData),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    fn close_only(date: &str, close: f64) -> Bar {
        Bar { date: d(date), high: None, low: None, close }
    }

    fn ohlc(date: &str, high: f64, low: f64, close: f64) -> Bar {
        Bar { date: d(date), high: Some(high), low: Some(low), close }
    }

    /// 100 shares sold at $10 on a Friday.
    fn sale() -> Sale {
        Sale { date: d("2025-01-03"), price: 10.0, quantity: 100.0 }
    }

    #[test]
    fn prices_each_window_against_the_sale() {
        let bars = vec![
            close_only("2025-01-10", 11.0), // +1 week
            close_only("2025-02-14", 12.0), // +6 weeks
            close_only("2025-04-03", 9.0),  // +3 months
        ];
        let p = price_points(&sale(), &bars, None, d("2025-06-01"), None);

        assert_eq!(p.week1.value, Some(11.0));
        // 100 shares × $1 forgone.
        assert_eq!(p.week1.delta, Some(100.0));
        assert_eq!(p.week1.pct, Some(10.0));
        assert_eq!(p.week6.value, Some(12.0));
        assert_eq!(p.month3.value, Some(9.0));
        // Selling ahead of a fall is a loss avoided, and reads negative.
        assert_eq!(p.month3.delta, Some(-100.0));
    }

    /// Targets land on weekends. The honest answer for a Saturday is the last
    /// price the market actually made, not the following Monday's.
    #[test]
    fn a_weekend_target_reads_back_to_the_last_trading_day() {
        // Sold Wednesday; +1 week lands on Wednesday 2025-01-08 — but suppose
        // the market was shut, and the last bar is the Friday before.
        let s = Sale { date: d("2025-01-01"), price: 10.0, quantity: 10.0 };
        let bars = vec![close_only("2025-01-03", 11.0)];
        let p = price_points(&s, &bars, None, d("2025-06-01"), None);

        assert_eq!(p.week1.status, Status::Ok);
        assert_eq!(p.week1.value, Some(11.0));
        assert_eq!(p.week1.date.as_deref(), Some("2025-01-03"), "reports the bar it actually used");
    }

    /// Past the lookback the answer is "no data", not the last bar lying around.
    /// This is what stops a delisted stock's final price being served as its
    /// price at every window for the rest of time.
    #[test]
    fn a_bar_older_than_the_lookback_does_not_answer_for_a_window() {
        let bars = vec![close_only("2025-01-04", 11.0)]; // 6 days before the +6wk target
        let p = price_points(&sale(), &bars, None, d("2025-06-01"), None);

        assert_eq!(p.week1.status, Status::Ok, "one day after the sale, within a week's lookback");
        assert_eq!(p.week6.status, Status::NoData);
        assert_eq!(p.week6.value, None);
        assert_eq!(p.month3.status, Status::NoData);
    }

    /// A stock that stops trading the day it is sold sits exactly one lookback
    /// from the +1 week target, so without excluding the sale date its own
    /// closing price comes back dressed as the price a week later.
    #[test]
    fn the_sale_days_own_bar_never_answers_for_a_later_window() {
        let bars = vec![close_only("2025-01-03", 10.0)]; // the sale date itself
        let p = price_points(&sale(), &bars, None, d("2025-06-01"), None);

        assert_eq!(p.week1.status, Status::NoData, "the sale day is not a week later");
        assert_eq!(p.week1.value, None);
    }

    /// A window that has not come round yet will fill in on its own. Saying so
    /// is different from saying the data is missing.
    #[test]
    fn a_window_that_has_not_elapsed_is_pending() {
        let s = Sale { date: d("2026-08-25"), price: 10.0, quantity: 10.0 };
        let bars = vec![close_only("2026-09-01", 11.0)];
        let p = price_points(&s, &bars, Some(11.0), d("2026-09-01"), None);

        assert_eq!(p.week1.status, Status::Ok);
        assert_eq!(p.week6.status, Status::Pending);
        assert_eq!(p.month3.status, Status::Pending);
        assert_eq!(p.week6.value, None);
    }

    /// A delisted symbol's windows never arrive, which is not the same as not
    /// having arrived yet — the distinction is the whole reason the mark exists.
    #[test]
    fn windows_past_a_delisting_are_delisted_not_pending() {
        let s = Sale { date: d("2025-08-01"), price: 3.91, quantity: 700.0 };
        let p = price_points(&s, &[], None, d("2026-09-01"), Some(d("2025-08-01")));

        assert_eq!(p.week1.status, Status::Delisted);
        assert_eq!(p.week6.status, Status::Delisted);
        assert_eq!(p.month3.status, Status::Delisted);
        assert_eq!(p.peak.status, Status::Delisted);
        assert_eq!(p.low.status, Status::Delisted);
        assert_eq!(p.current.status, Status::Delisted);
    }

    /// A delisted symbol with a recorded final price still shows the number.
    /// The label is what stops it reading as a live quote.
    #[test]
    fn a_delisted_symbol_reports_its_final_price_as_final() {
        let s = Sale { date: d("2025-08-01"), price: 3.50, quantity: 700.0 };
        let p = price_points(&s, &[], Some(3.91), d("2026-09-01"), Some(d("2025-08-01")));

        assert_eq!(p.current.status, Status::Delisted);
        assert_eq!(p.current.value, Some(3.91));
        assert_eq!(p.current.date.as_deref(), Some("2025-08-01"), "dated to the delisting, not today");
        // 700 × 0.41, which does not land exactly in binary floating point.
        assert!((p.current.delta.unwrap() - 287.0).abs() < 1e-9);
    }

    /// A delisted symbol with no bars after the sale has no peak — only a final
    /// price. Labelling that `Ok` would present a dead stock's last print as a
    /// high it traded through, and date it to today.
    #[test]
    fn a_delisted_symbols_final_price_is_not_reported_as_a_peak() {
        let s = Sale { date: d("2025-08-01"), price: 3.50, quantity: 700.0 };
        let p = price_points(&s, &[], Some(3.91), d("2026-09-01"), Some(d("2025-08-01")));

        assert_eq!(p.peak.status, Status::Delisted);
        assert_eq!(p.peak.value, Some(3.91), "the number is still worth showing");
        assert_eq!(p.peak.date.as_deref(), Some("2025-08-01"), "not today");
        assert_eq!(p.low.status, Status::Delisted);
    }

    /// Bars from before the delisting are real history, so an extreme drawn
    /// from one stays `Ok` even though the symbol has since died.
    #[test]
    fn a_delisted_symbols_traded_peak_is_still_real() {
        let s = Sale { date: d("2025-01-03"), price: 10.0, quantity: 100.0 };
        let bars = vec![ohlc("2025-02-10", 18.0, 8.0, 12.0)];
        let p = price_points(&s, &bars, Some(5.0), d("2026-09-01"), Some(d("2025-08-01")));

        assert_eq!(p.peak.status, Status::Ok);
        assert_eq!(p.peak.value, Some(18.0));
        assert_eq!(p.peak.date.as_deref(), Some("2025-02-10"));
        // The final price undercuts every bar, so the low is the quote's.
        assert_eq!(p.low.status, Status::Delisted);
        assert_eq!(p.low.value, Some(5.0));
    }

    /// The peak is the highest price *reached*, which is the intraday high —
    /// the same reasoning the trailing stop uses. A bar that spikes and gives
    /// the gain back still counts.
    #[test]
    fn the_extremes_use_intraday_highs_and_lows() {
        let bars = vec![
            ohlc("2025-01-10", 14.0, 9.5, 10.5),
            ohlc("2025-01-11", 11.0, 7.0, 8.0),
        ];
        let p = price_points(&sale(), &bars, None, d("2025-06-01"), None);

        assert_eq!(p.peak.value, Some(14.0), "not the 10.5 close");
        assert_eq!(p.peak.date.as_deref(), Some("2025-01-10"));
        assert_eq!(p.low.value, Some(7.0), "not the 8.0 close");
        assert_eq!(p.low.date.as_deref(), Some("2025-01-11"));
    }

    /// Bars ingested before OHLC collection began carry only a close. They must
    /// still count toward the extremes rather than being skipped.
    #[test]
    fn close_only_bars_still_count_toward_the_extremes() {
        let bars = vec![
            ohlc("2025-01-10", 11.0, 10.0, 10.5),
            close_only("2025-01-13", 15.0), // no high, and it beats every recorded one
            close_only("2025-01-14", 6.0),
        ];
        let p = price_points(&sale(), &bars, None, d("2025-06-01"), None);

        assert_eq!(p.peak.value, Some(15.0));
        assert_eq!(p.low.value, Some(6.0));
    }

    /// "Since sold" starts after the sale. Bars from while the stock was held
    /// belong to the holdings screens, and letting them in would credit the
    /// sale with a peak it was never in a position to catch.
    #[test]
    fn the_extremes_ignore_everything_up_to_and_including_the_sale() {
        let bars = vec![
            ohlc("2024-12-20", 99.0, 1.0, 50.0), // while held
            ohlc("2025-01-03", 88.0, 2.0, 10.0), // the sale day itself
            ohlc("2025-01-10", 12.0, 9.0, 11.0), // after
        ];
        let p = price_points(&sale(), &bars, None, d("2025-06-01"), None);

        assert_eq!(p.peak.value, Some(12.0));
        assert_eq!(p.low.value, Some(9.0));
    }

    /// The day's bar holds the close so far while the quote is what the rest of
    /// the app shows, so a stock at its high right now must not be reported as
    /// having peaked yesterday.
    #[test]
    fn the_live_quote_can_set_the_peak_or_the_low() {
        let bars = vec![ohlc("2025-01-10", 12.0, 9.0, 11.0)];
        let today = d("2025-06-01");

        let high = price_points(&sale(), &bars, Some(20.0), today, None);
        assert_eq!(high.peak.value, Some(20.0));
        assert_eq!(high.peak.date.as_deref(), Some("2025-06-01"));
        assert_eq!(high.low.value, Some(9.0), "a new high must not disturb the low");

        let low = price_points(&sale(), &bars, Some(2.0), today, None);
        assert_eq!(low.low.value, Some(2.0));
        assert_eq!(low.peak.value, Some(12.0));
    }

    /// Three calendar months, not ninety days. Selling on 30 November has to
    /// land in February, and February has to clamp to its own last day.
    #[test]
    fn three_months_is_calendar_arithmetic() {
        let s = Sale { date: d("2024-11-30"), price: 10.0, quantity: 1.0 };
        let bars = vec![close_only("2025-02-28", 13.0), close_only("2025-03-01", 99.0)];
        let p = price_points(&s, &bars, None, d("2025-06-01"), None);

        assert_eq!(p.month3.value, Some(13.0), "28 Feb, not 28 Feb + 2 days");
        assert_eq!(p.month3.date.as_deref(), Some("2025-02-28"));
    }

    /// A zero sale price would make the percentage infinite rather than merely
    /// large. The cash difference is still meaningful and is kept.
    #[test]
    fn a_zero_sale_price_yields_no_percentage() {
        let s = Sale { date: d("2025-01-03"), price: 0.0, quantity: 100.0 };
        let bars = vec![close_only("2025-01-10", 5.0)];
        let p = price_points(&s, &bars, None, d("2025-06-01"), None);

        assert_eq!(p.week1.pct, None);
        assert_eq!(p.week1.delta, Some(500.0));
    }

    /// No bars and no delisting mark is a gap in the data, and should read as
    /// one rather than being blamed on the stock.
    #[test]
    fn a_live_symbol_with_no_history_reads_as_missing_data() {
        let p = price_points(&sale(), &[], None, d("2025-06-01"), None);

        assert_eq!(p.peak.status, Status::NoData);
        assert_eq!(p.low.status, Status::NoData);
        assert_eq!(p.current.status, Status::NoData);
        assert_eq!(p.week1.status, Status::NoData);
    }
}

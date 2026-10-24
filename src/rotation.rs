//! Sector rotation: how far each sector moved over a few fixed periods, and how
//! that compares with a benchmark over the same days. The holdings heat map's
//! periods are measured the same way, out to two years.
//!
//! Price return only, as in Hindsight. A dividend paid inside a window shows as
//! the drop on its ex-date, so the higher-yield sectors read a fraction of a
//! percent low over a month — the screen says "price return" rather than
//! pretend otherwise.

use chrono::{Days, Months, NaiveDate};
use serde::Serialize;

/// How far back of a target date a close may sit and still answer for it.
///
/// Targets land on weekends and public holidays, when the honest answer is the
/// last close the market actually made. A longer gap is missing data rather
/// than a closed market, and measuring across it would report a month's move as
/// a week's.
const LOOKBACK_DAYS: u64 = 7;

/// One figure per period, in percent. `None` where there was no close to
/// measure from.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct PeriodReturns {
    #[serde(rename = "1d")]
    pub day: Option<f64>,
    #[serde(rename = "1w")]
    pub week: Option<f64>,
    #[serde(rename = "1m")]
    pub month: Option<f64>,
    #[serde(rename = "3m")]
    pub quarter: Option<f64>,
}

impl PeriodReturns {
    /// How far ahead of `benchmark` each period finished, in percentage points.
    ///
    /// Points rather than a ratio of the two: over periods this short they
    /// barely differ, and "beat the market by 2 points" is how the figure is
    /// read.
    pub fn minus(&self, benchmark: &PeriodReturns) -> PeriodReturns {
        let diff = |own: Option<f64>, other: Option<f64>| own.zip(other).map(|(a, b)| a - b);
        PeriodReturns {
            day: diff(self.day, benchmark.day),
            week: diff(self.week, benchmark.week),
            month: diff(self.month, benchmark.month),
            quarter: diff(self.quarter, benchmark.quarter),
        }
    }
}

/// The last close at or before `target`, provided one is close enough to stand
/// for it. `closes` is oldest first.
fn close_at(closes: &[(NaiveDate, f64)], target: NaiveDate) -> Option<f64> {
    let floor = target.checked_sub_days(Days::new(LOOKBACK_DAYS))?;
    let index = closes.partition_point(|(date, _)| *date <= target);
    let (date, close) = closes.get(index.checked_sub(1)?)?;
    (*date >= floor).then_some(*close)
}

fn pct(from: Option<f64>, to: f64) -> Option<f64> {
    from.filter(|f| *f > 0.0).map(|f| (to / f - 1.0) * 100.0)
}

/// Each period's move up to the close on `as_of`.
///
/// `closes` is one symbol's daily closes, oldest first. A symbol with no close
/// on `as_of` itself gets nothing at all: its newest move belongs to a
/// different day from the benchmark's, and setting the two side by side would
/// compare unlike things — so the tile is left blank rather than quietly
/// misleading.
///
/// 1D is measured from the session before; the others from the last close on or
/// before the same date a week, a month and three months earlier.
pub fn period_returns(closes: &[(NaiveDate, f64)], as_of: NaiveDate) -> PeriodReturns {
    let Some(end_index) = closes.partition_point(|(date, _)| *date <= as_of).checked_sub(1) else {
        return PeriodReturns::default();
    };
    let (end_date, end) = closes[end_index];
    if end_date != as_of || end <= 0.0 {
        return PeriodReturns::default();
    }

    // The session before, wherever the weekend or a holiday put it — but not
    // across a gap in the data, which would pass off several days as one.
    let previous = end_index
        .checked_sub(1)
        .map(|i| closes[i])
        .filter(|(date, _)| as_of.checked_sub_days(Days::new(LOOKBACK_DAYS)).is_some_and(|floor| *date >= floor))
        .map(|(_, close)| close);
    let back = |target: Option<NaiveDate>| target.and_then(|t| close_at(closes, t));

    PeriodReturns {
        day: pct(previous, end),
        week: pct(back(as_of.checked_sub_days(Days::new(7))), end),
        month: pct(back(as_of.checked_sub_months(Months::new(1))), end),
        quarter: pct(back(as_of.checked_sub_months(Months::new(3))), end),
    }
}

/// A holding's move over each period the holdings heat map offers, in percent.
/// `None` where there was no close to measure from: history that does not reach
/// back far enough is no data, not a move of nothing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct TrailingMoves {
    #[serde(rename = "1w")]
    pub week: Option<f64>,
    #[serde(rename = "1m")]
    pub month: Option<f64>,
    #[serde(rename = "3m")]
    pub quarter: Option<f64>,
    #[serde(rename = "1y")]
    pub year: Option<f64>,
    #[serde(rename = "2y")]
    pub two_years: Option<f64>,
}

/// Each period's move to `price`, a quote dated `as_of`, from the last close on
/// or before the same date a week, a month, three months, a year and two years
/// earlier.
///
/// Unlike `period_returns`, the move ends on a price rather than a stored close:
/// a holding's tile has to agree with the quote the rest of the screen shows,
/// and the stored bar for the quote's day can be missing or taken mid-session.
pub fn trailing_moves(closes: &[(NaiveDate, f64)], as_of: NaiveDate, price: f64) -> TrailingMoves {
    if !crate::prices::is_usable_price(Some(price)) {
        return TrailingMoves::default();
    }
    let back = |target: Option<NaiveDate>| pct(target.and_then(|t| close_at(closes, t)), price);
    TrailingMoves {
        week: back(as_of.checked_sub_days(Days::new(7))),
        month: back(as_of.checked_sub_months(Months::new(1))),
        quarter: back(as_of.checked_sub_months(Months::new(3))),
        year: back(as_of.checked_sub_months(Months::new(12))),
        two_years: back(as_of.checked_sub_months(Months::new(24))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    fn closes(rows: &[(&str, f64)]) -> Vec<(NaiveDate, f64)> {
        rows.iter().map(|(d, c)| (day(d), *c)).collect()
    }

    fn assert_pct(found: Option<f64>, expected: f64) {
        let found = found.unwrap_or_else(|| panic!("expected {expected}, found nothing"));
        assert!((found - expected).abs() < 1e-9, "expected {expected}, found {found}");
    }

    #[test]
    fn each_period_is_measured_from_its_own_target_date() {
        // Friday 23 Oct 2026. Every target is a weekday with a close of its own.
        let bars = closes(&[
            ("2026-07-23", 80.0), // three months back
            ("2026-09-23", 90.0), // one month back
            ("2026-10-16", 95.0), // one week back
            ("2026-10-22", 98.0), // the session before
            ("2026-10-23", 100.0),
        ]);
        let r = period_returns(&bars, day("2026-10-23"));
        assert_pct(r.day, (100.0 / 98.0 - 1.0) * 100.0);
        assert_pct(r.week, (100.0 / 95.0 - 1.0) * 100.0);
        assert_pct(r.month, (100.0 / 90.0 - 1.0) * 100.0);
        assert_pct(r.quarter, 25.0);
    }

    /// A target on a weekend answers with the Friday before it, not the Monday
    /// after: that is the last price the market made by that date.
    #[test]
    fn a_weekend_target_reads_the_close_before_it() {
        // Monday 26 Oct 2026; one month back is Saturday 26 Sep.
        let bars = closes(&[
            ("2026-09-24", 88.0),
            ("2026-09-25", 90.0), // Friday — the answer
            ("2026-09-28", 92.0), // Monday after — must not be used
            ("2026-10-23", 99.0), // Friday before the as-of Monday
            ("2026-10-26", 99.0),
        ]);
        let r = period_returns(&bars, day("2026-10-26"));
        assert_pct(r.month, (99.0 / 90.0 - 1.0) * 100.0);
        // The session before Monday is Friday, three calendar days earlier.
        assert_pct(r.day, 0.0);
    }

    /// No close on the as-of date means the symbol's newest move is from a
    /// different day than the benchmark's, so nothing is reported.
    #[test]
    fn a_symbol_behind_the_as_of_date_reports_nothing() {
        let bars = closes(&[("2026-10-21", 98.0), ("2026-10-22", 99.0)]);
        assert_eq!(period_returns(&bars, day("2026-10-23")), PeriodReturns::default());
        assert_eq!(period_returns(&[], day("2026-10-23")), PeriodReturns::default());
    }

    /// A close after the as-of date is ignored rather than measured to — the
    /// benchmark sets the day.
    #[test]
    fn closes_after_the_as_of_date_are_ignored() {
        let bars = closes(&[("2026-10-22", 98.0), ("2026-10-23", 100.0), ("2026-10-26", 150.0)]);
        assert_pct(period_returns(&bars, day("2026-10-23")).day, (100.0 / 98.0 - 1.0) * 100.0);
    }

    /// History that does not reach back far enough leaves the longer periods
    /// empty, and a gap in the data is not measured across.
    #[test]
    fn missing_history_leaves_the_period_empty() {
        let bars = closes(&[
            ("2026-08-01", 70.0), // more than a week before the 1M target (23 Sep)
            ("2026-10-05", 90.0), // more than a week before the as-of date
            ("2026-10-23", 100.0),
        ]);
        let r = period_returns(&bars, day("2026-10-23"));
        assert_eq!(r.day, None, "a 2½-week gap is not one session");
        assert_eq!(r.week, None, "nothing within a week of 16 Oct");
        assert_eq!(r.month, None, "nothing within a week of 23 Sep");
        assert_eq!(r.quarter, None, "history starts after 23 Jul");
    }

    #[test]
    fn relative_figures_are_the_difference_in_points() {
        let sector = PeriodReturns { day: Some(1.5), week: Some(-2.0), month: None, quarter: Some(4.0) };
        let benchmark = PeriodReturns { day: Some(0.5), week: Some(1.0), month: Some(3.0), quarter: None };
        let r = sector.minus(&benchmark);
        assert_pct(r.day, 1.0);
        assert_pct(r.week, -3.0);
        assert_eq!(r.month, None);
        assert_eq!(r.quarter, None);
    }

    #[test]
    fn periods_serialise_under_their_short_names() {
        let json = serde_json::to_value(PeriodReturns { day: Some(1.0), ..Default::default() }).unwrap();
        assert_eq!(json, serde_json::json!({ "1d": 1.0, "1w": null, "1m": null, "3m": null }));
    }

    /// The moves end on the quote, not on the newest stored close: that bar is
    /// often missing, or was taken mid-session and has since been passed.
    #[test]
    fn trailing_moves_run_from_each_start_to_the_quote() {
        // Friday 23 Oct 2026. Every target is a weekday with a close of its own.
        let bars = closes(&[
            ("2024-10-23", 40.0), // two years back
            ("2025-10-23", 50.0), // one year back
            ("2026-07-23", 80.0), // three months back
            ("2026-09-23", 90.0), // one month back
            ("2026-10-16", 95.0), // one week back
            ("2026-10-23", 99.0), // a mid-session bar the quote has moved on from
        ]);
        let m = trailing_moves(&bars, day("2026-10-23"), 100.0);
        assert_pct(m.week, (100.0 / 95.0 - 1.0) * 100.0);
        assert_pct(m.month, (100.0 / 90.0 - 1.0) * 100.0);
        assert_pct(m.quarter, 25.0);
        assert_pct(m.year, 100.0);
        assert_pct(m.two_years, 150.0);
    }

    /// Stored bars have holes. Each period still starts from the last close on
    /// or before its own target date, never from a count of bars back, and
    /// never from a close after the target.
    #[test]
    fn trailing_moves_start_from_the_close_on_or_before_each_target() {
        // Monday 26 Oct 2026: a year back is Sunday 26 Oct 2025, two years back
        // Saturday 26 Oct 2024, and the week's target, Monday 19 Oct, is a hole.
        let bars = closes(&[
            ("2024-10-25", 50.0),  // Friday before the 2Y target — the answer
            ("2024-10-28", 999.0), // Monday after — must not be used
            ("2025-10-24", 80.0),  // Friday before the 1Y target
            ("2025-10-27", 999.0),
            ("2026-10-16", 90.0), // the last close before the missing Monday
            ("2026-10-20", 999.0),
        ]);
        let m = trailing_moves(&bars, day("2026-10-26"), 100.0);
        assert_pct(m.two_years, 100.0);
        assert_pct(m.year, 25.0);
        assert_pct(m.week, (100.0 / 90.0 - 1.0) * 100.0);
    }

    /// A holding listed eighteen months ago has no two-year move, and a gap
    /// longer than the lookback is not measured across. Both are no data,
    /// which the heat map draws differently from a flat 0%.
    #[test]
    fn trailing_moves_leave_periods_without_history_empty() {
        let bars = closes(&[
            ("2025-05-22", 50.0), // first close: after the 2Y target
            ("2025-10-23", 60.0),
            ("2026-08-01", 70.0), // more than a week before the 1M target (23 Sep)
            ("2026-10-16", 90.0),
        ]);
        let m = trailing_moves(&bars, day("2026-10-23"), 100.0);
        assert_eq!(m.two_years, None, "history starts after 23 Oct 2024");
        assert_eq!(m.month, None, "nothing within a week of 23 Sep");
        assert_pct(m.year, (100.0 / 60.0 - 1.0) * 100.0);
        assert_pct(m.week, (100.0 / 90.0 - 1.0) * 100.0);
    }

    #[test]
    fn trailing_moves_need_a_price_to_end_on() {
        let bars = closes(&[("2026-10-16", 90.0)]);
        for price in [0.0, -1.0, f64::NAN] {
            assert_eq!(trailing_moves(&bars, day("2026-10-23"), price), TrailingMoves::default());
        }
    }

    #[test]
    fn trailing_moves_serialise_under_their_short_names() {
        let json = serde_json::to_value(TrailingMoves { year: Some(1.0), ..Default::default() }).unwrap();
        assert_eq!(json, serde_json::json!({ "1w": null, "1m": null, "3m": null, "1y": 1.0, "2y": null }));
    }
}

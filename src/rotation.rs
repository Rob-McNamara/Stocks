//! Sector rotation: how far each sector moved over a few fixed periods, and how
//! that compares with a benchmark over the same days.
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
}

//! Australian income-tax rules the tax report applies to the portfolio.
//!
//! Only the rules live here — the year boundaries, when a parcel qualifies for
//! the CGT discount, the order losses are applied in, franking-credit gross-up.
//! Which shares a sale consumed is `portfolio::walk_parcels`, so the report and
//! the holdings screens can never disagree about the same trade; gathering the
//! rows and rendering the report is the API's job.
//!
//! Nothing here is advice: it is arithmetic over what was recorded, laid out
//! the way the return asks for it, for a person (or their accountant) to check.

use chrono::{Datelike, Days, Months, NaiveDate};

/// Who the return is for. Only the CGT discount differs between them here.
///
/// The report is for an individual today; the others exist so an SMSF or
/// company return later is a parameter, not a rewrite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Entity {
    Individual,
    /// A complying superannuation fund.
    Smsf,
    Company,
}

impl Entity {
    /// The share of a discountable gain that is taken off it.
    pub fn discount_fraction(self) -> f64 {
        match self {
            Entity::Individual => 0.5,
            Entity::Smsf => 1.0 / 3.0,
            Entity::Company => 0.0,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Entity::Individual => "individual",
            Entity::Smsf => "smsf",
            Entity::Company => "company",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Entity::Individual => "Individual",
            Entity::Smsf => "Self-managed super fund",
            Entity::Company => "Company",
        }
    }
}

/// An Australian income year, 1 July to 30 June, named by the year it ends in:
/// `TaxYear(2026)` is 2025–26.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TaxYear(pub i32);

impl TaxYear {
    pub fn containing(date: NaiveDate) -> TaxYear {
        if date.month() >= 7 { TaxYear(date.year() + 1) } else { TaxYear(date.year()) }
    }

    pub fn start(self) -> NaiveDate {
        NaiveDate::from_ymd_opt(self.0 - 1, 7, 1).expect("1 July always exists")
    }

    pub fn end(self) -> NaiveDate {
        NaiveDate::from_ymd_opt(self.0, 6, 30).expect("30 June always exists")
    }

    pub fn contains(self, date: NaiveDate) -> bool {
        date >= self.start() && date <= self.end()
    }

    /// "2025–26", as the ATO writes it.
    pub fn label(self) -> String {
        format!("{}–{:02}", self.0 - 1, self.0.rem_euclid(100))
    }

    /// "FY2025-26": ASCII, for a filename or a spreadsheet cell.
    pub fn slug(self) -> String {
        format!("FY{}-{:02}", self.0 - 1, self.0.rem_euclid(100))
    }
}

/// `YYYY-MM-DD`, as every date in the database is stored.
pub fn parse_date(value: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(value.get(..10).unwrap_or(value), "%Y-%m-%d").ok()
}

/// The first disposal date on which a parcel acquired on `acquired` qualifies
/// for the CGT discount.
///
/// The asset has to be owned for at least 12 months, *not counting* the day it
/// was acquired or the day of the CGT event. So the 12 months run from the day
/// after acquisition, and the earliest qualifying sale is the day after the
/// anniversary: bought 30 June 2024, it qualifies from 1 July 2025, and a sale
/// on 30 June 2025 — the anniversary itself — does not.
///
/// A 29 February purchase has no anniversary; adding 12 months lands on 28
/// February, and the parcel qualifies from 1 March.
pub fn discount_eligible_from(acquired: NaiveDate) -> NaiveDate {
    let anniversary = acquired.checked_add_months(Months::new(12)).unwrap_or(acquired);
    anniversary.checked_add_days(Days::new(1)).unwrap_or(anniversary)
}

pub fn discount_eligible(acquired: NaiveDate, disposed: NaiveDate) -> bool {
    disposed >= discount_eligible_from(acquired)
}

/// The income year a trust distribution is assessed in.
///
/// A beneficiary is taxed on the trust's income for the year it was earned in,
/// not the year the cash arrives, and the ex-date is a poor guide on its own:
/// Vanguard's June-quarter distribution goes ex on the first business day of
/// July yet belongs to the year that ended on 30 June. Stepping back a week
/// from the ex-date lands every common pattern in the period the distribution
/// was for — a 1 July ex-date in June, a late-June A-REIT ex-date in June, a
/// 1 October ex-date in September. The annual tax statement is the authority;
/// this only decides which year's statement the cash is set against.
pub fn distribution_tax_year(ex_date: NaiveDate) -> TaxYear {
    TaxYear::containing(ex_date.checked_sub_days(Days::new(7)).unwrap_or(ex_date))
}

/// The franking credit attached to a franked amount.
///
/// Credits are paid at the company's tax rate: 30% for most listed companies,
/// 25% for base-rate entities. A statement's own figure always beats this.
pub fn franking_credit(franked_amount: f64, company_tax_rate: f64) -> f64 {
    if company_tax_rate <= 0.0 || company_tax_rate >= 1.0 {
        return 0.0;
    }
    franked_amount * company_tax_rate / (1.0 - company_tax_rate)
}

/// The standard company tax rate, used when a statement gives no credit.
pub const COMPANY_TAX_RATE: f64 = 0.30;

/// One year's capital gains and losses, before anything is netted.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CapitalGainsInput {
    /// Gains on assets owned long enough for the discount, before losses and
    /// before the discount. A trust's discounted gain goes in grossed up.
    pub discount_gains: f64,
    /// Gains that don't qualify: held under 12 months, and anything else not
    /// calculated with the discount method.
    pub other_gains: f64,
    /// Capital losses made this year, as a positive number.
    pub losses: f64,
    /// Net capital losses carried forward from earlier years, positive.
    pub carried_in: f64,
}

/// What the return's capital gains question needs.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CapitalGainsResult {
    /// Total current year capital gains (label H): every gain, gross.
    pub total_gains: f64,
    pub current_losses_applied: f64,
    pub carried_losses_applied: f64,
    /// The discount taken off what was left of the discountable gains.
    pub discount: f64,
    /// Net capital gain (label A).
    pub net_capital_gain: f64,
    /// Net capital losses carried forward to later income years (label V).
    pub carried_out: f64,
}

/// Net one year's gains and losses the way the return does.
///
/// Losses come off gains *before* the discount, and this year's losses go
/// before any carried forward. Losses are applied to the gains that don't get a
/// discount first: a dollar of loss there saves a full dollar of net gain,
/// against fifty cents on a discountable one. That is the order the ATO's own
/// worksheet uses, and the most favourable one the law allows.
pub fn net_capital_gain(input: CapitalGainsInput, entity: Entity) -> CapitalGainsResult {
    let discount_gains = input.discount_gains.max(0.0);
    let other_gains = input.other_gains.max(0.0);
    let losses = input.losses.max(0.0);
    let carried_in = input.carried_in.max(0.0);

    let total_gains = discount_gains + other_gains;
    let current_losses_applied = losses.min(total_gains);
    let carried_losses_applied = carried_in.min(total_gains - current_losses_applied);

    let mut to_apply = current_losses_applied + carried_losses_applied;
    let from_other = to_apply.min(other_gains);
    to_apply -= from_other;
    let from_discount = to_apply.min(discount_gains);

    let other_left = other_gains - from_other;
    let discount_left = discount_gains - from_discount;
    let discount = discount_left * entity.discount_fraction();

    CapitalGainsResult {
        total_gains,
        current_losses_applied,
        carried_losses_applied,
        discount,
        net_capital_gain: other_left + discount_left - discount,
        carried_out: (losses - current_losses_applied) + (carried_in - carried_losses_applied),
    }
}

/// Split a trust's reported capital gains into the halves `net_capital_gain`
/// needs, from the two figures every annual tax statement prints: total current
/// year capital gains (18H, discounted gains grossed up) and net capital gain
/// (18A, after the trust applied the 50% discount).
///
/// With a 50% discount, 18H − 18A is half the discountable gain. Returns
/// (discount_gains, other_gains), both grossed up.
pub fn split_trust_capital_gains(total_gains: f64, net_gain: f64) -> (f64, f64) {
    let total = total_gains.max(0.0);
    let discount_gains = (2.0 * (total - net_gain.max(0.0))).clamp(0.0, total);
    (discount_gains, total - discount_gains)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn a_tax_year_runs_july_to_june_and_is_named_for_its_end() {
        let fy = TaxYear(2026);
        assert_eq!(fy.start(), d("2025-07-01"));
        assert_eq!(fy.end(), d("2026-06-30"));
        assert_eq!(fy.label(), "2025–26");
        assert_eq!(fy.slug(), "FY2025-26");
        assert_eq!(TaxYear(2000).label(), "1999–00");
    }

    #[test]
    fn the_year_boundary_falls_between_30_june_and_1_july() {
        assert_eq!(TaxYear::containing(d("2026-06-30")), TaxYear(2026));
        assert_eq!(TaxYear::containing(d("2026-07-01")), TaxYear(2027));
        assert_eq!(TaxYear::containing(d("2026-01-15")), TaxYear(2026));
        assert!(TaxYear(2026).contains(d("2025-07-01")));
        assert!(!TaxYear(2026).contains(d("2025-06-30")));
    }

    /// Neither the day of purchase nor the day of sale counts, so selling on
    /// the anniversary is one day short.
    #[test]
    fn a_sale_on_the_anniversary_is_not_discounted_but_the_next_day_is() {
        assert!(!discount_eligible(d("2024-06-30"), d("2025-06-30")));
        assert!(discount_eligible(d("2024-06-30"), d("2025-07-01")));
        assert!(!discount_eligible(d("2024-06-30"), d("2024-12-31")));
        assert_eq!(discount_eligible_from(d("2024-06-30")), d("2025-07-01"));
    }

    #[test]
    fn a_29_february_purchase_qualifies_from_1_march() {
        assert_eq!(discount_eligible_from(d("2024-02-29")), d("2025-03-01"));
        assert!(!discount_eligible(d("2024-02-29"), d("2025-02-28")));
        assert!(discount_eligible(d("2024-02-29"), d("2025-03-01")));
    }

    #[test]
    fn a_june_quarter_distribution_paid_in_july_belongs_to_the_year_before() {
        // Vanguard: ex-date the first business day of July.
        assert_eq!(distribution_tax_year(d("2025-07-01")), TaxYear(2025));
        assert_eq!(distribution_tax_year(d("2025-07-03")), TaxYear(2025));
        // A-REIT: ex-date in late June, paid in August.
        assert_eq!(distribution_tax_year(d("2025-06-27")), TaxYear(2025));
        // September quarter.
        assert_eq!(distribution_tax_year(d("2025-10-01")), TaxYear(2026));
    }

    #[test]
    fn franking_credit_grosses_up_at_the_company_rate() {
        assert!(close(franking_credit(70.0, 0.30), 30.0));
        assert!(close(franking_credit(75.0, 0.25), 25.0));
        assert_eq!(franking_credit(100.0, 0.0), 0.0);
    }

    #[test]
    fn losses_come_off_undiscounted_gains_first_then_the_discount_applies() {
        let r = net_capital_gain(
            CapitalGainsInput { discount_gains: 1000.0, other_gains: 300.0, losses: 400.0, carried_in: 0.0 },
            Entity::Individual,
        );
        assert!(close(r.total_gains, 1300.0));
        // 300 of loss clears the other gains, 100 comes off the discountable.
        assert!(close(r.discount, 450.0));
        assert!(close(r.net_capital_gain, 450.0));
        assert!(close(r.carried_out, 0.0));
    }

    #[test]
    fn this_years_losses_go_before_those_carried_forward() {
        let r = net_capital_gain(
            CapitalGainsInput { discount_gains: 0.0, other_gains: 500.0, losses: 200.0, carried_in: 1000.0 },
            Entity::Individual,
        );
        assert!(close(r.current_losses_applied, 200.0));
        assert!(close(r.carried_losses_applied, 300.0));
        assert!(close(r.net_capital_gain, 0.0));
        assert!(close(r.carried_out, 700.0));
    }

    #[test]
    fn a_net_loss_is_carried_out_and_nothing_is_discounted() {
        let r = net_capital_gain(
            CapitalGainsInput { discount_gains: 100.0, other_gains: 0.0, losses: 250.0, carried_in: 50.0 },
            Entity::Individual,
        );
        assert!(close(r.net_capital_gain, 0.0));
        assert!(close(r.discount, 0.0));
        assert!(close(r.carried_out, 200.0));
    }

    #[test]
    fn the_discount_depends_on_who_the_return_is_for() {
        let input = CapitalGainsInput { discount_gains: 900.0, ..Default::default() };
        assert!(close(net_capital_gain(input, Entity::Individual).net_capital_gain, 450.0));
        assert!(close(net_capital_gain(input, Entity::Smsf).net_capital_gain, 600.0));
        assert!(close(net_capital_gain(input, Entity::Company).net_capital_gain, 900.0));
    }

    #[test]
    fn a_trusts_capital_gains_split_back_into_discount_and_other() {
        // $800 discountable (grossed up) and $100 other: 18H 900, 18A 500.
        let (discount, other) = split_trust_capital_gains(900.0, 500.0);
        assert!(close(discount, 800.0));
        assert!(close(other, 100.0));
        // All discountable.
        let (discount, other) = split_trust_capital_gains(200.0, 100.0);
        assert!(close(discount, 200.0));
        assert!(close(other, 0.0));
    }
}

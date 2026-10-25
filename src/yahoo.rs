//! The shape of Yahoo's chart endpoint, shared by every binary that reads it.
//!
//! Five copies of these structs existed, each declaring the subset of fields
//! its caller used — and each free to disagree about which were optional. One
//! superset with every field optional parses all of them; a caller that needs
//! a field says so where it reads it.

use chrono::{NaiveDate, TimeZone, Utc};
use serde::Deserialize;
use std::collections::HashMap;

#[derive(Debug, Deserialize)]
pub struct ChartResponse {
    pub chart: Chart,
}

#[derive(Debug, Deserialize)]
pub struct Chart {
    pub result: Option<Vec<ChartResult>>,
    pub error: Option<serde_json::Value>,
}

impl Chart {
    /// The first result, or an error naming the symbol and Yahoo's own error.
    pub fn into_first(self, symbol: &str) -> Result<ChartResult, String> {
        let error = self.error;
        self.result
            .and_then(|items| items.into_iter().next())
            .ok_or_else(|| match error {
                Some(err) => format!("No chart result found for {}: {}", symbol, err),
                None => format!("No chart result found for {}", symbol),
            })
    }
}

#[derive(Debug, Deserialize)]
pub struct ChartResult {
    pub meta: Option<Meta>,
    pub timestamp: Option<Vec<i64>>,
    pub indicators: Option<Indicators>,
    pub events: Option<Events>,
}

impl ChartResult {
    /// The OHLCV arrays, which Yahoo wraps in a one-element list.
    pub fn quote(&self) -> Option<&Quote> {
        self.indicators.as_ref().and_then(|i| i.quote.first())
    }

    pub fn gmtoffset(&self) -> Option<i64> {
        self.meta.as_ref().and_then(|m| m.gmtoffset)
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct Meta {
    #[serde(rename = "regularMarketPrice")]
    pub regular_market_price: Option<f64>,
    #[serde(rename = "regularMarketChange")]
    pub regular_market_change: Option<f64>,
    #[serde(rename = "regularMarketChangePercent")]
    pub regular_market_change_percent: Option<f64>,
    #[serde(rename = "regularMarketVolume")]
    pub regular_market_volume: Option<i64>,
    #[serde(rename = "chartPreviousClose")]
    pub chart_previous_close: Option<f64>,
    #[serde(rename = "regularMarketDayHigh")]
    pub regular_market_day_high: Option<f64>,
    #[serde(rename = "regularMarketDayLow")]
    pub regular_market_day_low: Option<f64>,
    /// Yahoo's chart meta has no open field — callers fill it from the quote
    /// arrays, hence `default` rather than a rename.
    #[serde(default)]
    pub day_open: Option<f64>,
    #[serde(rename = "instrumentType")]
    pub instrument_type: Option<String>,
    #[serde(rename = "longName")]
    pub long_name: Option<String>,
    pub currency: Option<String>,
    #[serde(rename = "regularMarketTime")]
    pub regular_market_time: Option<i64>,
    /// Exchange UTC offset in seconds — needed to date bars correctly.
    pub gmtoffset: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct Indicators {
    pub quote: Vec<Quote>,
}

#[derive(Debug, Deserialize)]
pub struct Quote {
    pub open: Option<Vec<Option<f64>>>,
    pub high: Option<Vec<Option<f64>>>,
    pub low: Option<Vec<Option<f64>>>,
    pub close: Option<Vec<Option<f64>>>,
    pub volume: Option<Vec<Option<i64>>>,
}

impl Quote {
    fn at<T: Copy>(series: &Option<Vec<Option<T>>>, index: usize) -> Option<T> {
        series.as_ref().and_then(|v| v.get(index).copied().flatten())
    }
    pub fn open_at(&self, index: usize) -> Option<f64> { Self::at(&self.open, index) }
    pub fn high_at(&self, index: usize) -> Option<f64> { Self::at(&self.high, index) }
    pub fn low_at(&self, index: usize) -> Option<f64> { Self::at(&self.low, index) }
    pub fn close_at(&self, index: usize) -> Option<f64> { Self::at(&self.close, index) }
    pub fn volume_at(&self, index: usize) -> Option<i64> { Self::at(&self.volume, index) }
}

#[derive(Debug, Deserialize)]
pub struct Events {
    pub dividends: Option<HashMap<String, DividendEntry>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DividendEntry {
    pub amount: Option<f64>,
    pub date: Option<i64>,
    pub ex_date: Option<i64>,
    pub payment_date: Option<i64>,
    pub record_date: Option<i64>,
}

/// Convert a Yahoo bar or event timestamp to the exchange-local trading date.
///
/// Yahoo stamps daily bars at the market open; for exchanges ahead of UTC (ASX
/// opens 10:00 Sydney = 23:00 UTC the *previous* day during daylight saving)
/// the UTC date is one day early, so the date is taken in exchange time:
/// UTC + gmtoffset.
pub fn local_date(ts: i64, gmtoffset: Option<i64>) -> Option<NaiveDate> {
    Utc.timestamp_opt(ts + gmtoffset.unwrap_or(0), 0)
        .single()
        .map(|dt| dt.date_naive())
}

/// The trading date of a daily bar: `local_date` with an hour's grace.
///
/// Yahoo gives one offset for a whole series — the exchange's offset now —
/// though the bars span both sides of every clock change. That is harmless for
/// a bar stamped at the open, hours from midnight, but an FX pair's bars are
/// stamped at midnight London time: read in the UK's winter, a summer bar's
/// 23:00 UTC lands an hour short, on the day before. Read on the day the
/// clocks went back, every USD/AUD rate from late March to late October was
/// filed a day early, each weekday holding the next one's rate. No series this
/// app reads stamps a bar in the last hour of its day, so the grace moves
/// nothing else.
///
/// For bars only. A quote's time can fall in the last hour of a trading day —
/// an FX quote late in the London evening — and has to keep `local_date`.
pub fn bar_date(ts: i64, gmtoffset: Option<i64>) -> Option<NaiveDate> {
    local_date(ts + 3600, gmtoffset)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One payload, every consumer: the quote, history, dividend and OHLC
    /// readers all parse this same shape now.
    #[test]
    fn parses_a_chart_payload_with_bars_and_dividends() {
        let json = r#"{
            "chart": {
                "result": [{
                    "meta": { "currency": "AUD", "gmtoffset": 39600, "regularMarketPrice": 45.1 },
                    "timestamp": [1767564000],
                    "indicators": { "quote": [{ "open": [44.0], "high": [45.5], "low": [43.9], "close": [45.1], "volume": [1000] }] },
                    "events": { "dividends": { "1767564000": { "amount": 0.44, "date": 1767564000 } } }
                }],
                "error": null
            }
        }"#;
        let result = serde_json::from_str::<ChartResponse>(json).unwrap().chart.into_first("BHP.AX").unwrap();
        assert_eq!(result.gmtoffset(), Some(39600));
        assert_eq!(result.meta.as_ref().unwrap().regular_market_price, Some(45.1));
        let quote = result.quote().unwrap();
        assert_eq!((quote.open_at(0), quote.close_at(0), quote.volume_at(0)), (Some(44.0), Some(45.1), Some(1000)));
        assert_eq!(quote.close_at(1), None);
        let ts = result.events.unwrap().dividends.unwrap().values().next().unwrap().date.unwrap();
        assert_eq!(local_date(ts, Some(39600)), NaiveDate::from_ymd_opt(2026, 1, 5));
    }

    /// USD/AUD's bars are stamped at midnight London time: 23:00 UTC the day
    /// before in summer time, 00:00 UTC in winter. Whichever offset Yahoo
    /// reports with the series, every bar keeps its own date.
    #[test]
    fn an_fx_bar_keeps_its_date_across_a_uk_clock_change() {
        const BST: Option<i64> = Some(3600);
        const GMT: Option<i64> = Some(0);
        // Tuesday 15 Sep 2026, London midnight in summer time.
        let summer = Utc.with_ymd_and_hms(2026, 9, 14, 23, 0, 0).unwrap().timestamp();
        // Wednesday 14 Jan 2026, London midnight in winter time.
        let winter = Utc.with_ymd_and_hms(2026, 1, 14, 0, 0, 0).unwrap().timestamp();
        for offset in [BST, GMT] {
            assert_eq!(bar_date(summer, offset), NaiveDate::from_ymd_opt(2026, 9, 15), "summer bar, offset {offset:?}");
            assert_eq!(bar_date(winter, offset), NaiveDate::from_ymd_opt(2026, 1, 14), "winter bar, offset {offset:?}");
        }
        // The bug: read with the winter offset, the summer bar was Monday's.
        assert_eq!(local_date(summer, GMT), NaiveDate::from_ymd_opt(2026, 9, 14));
    }

    /// Bars stamped at the open are hours from midnight, so the grace leaves
    /// them where `local_date` puts them, either side of a clock change.
    #[test]
    fn a_bar_stamped_at_the_open_keeps_its_date() {
        // ASX opens 10:00 Sydney: 23:00 UTC the day before in daylight saving.
        let asx_summer = Utc.with_ymd_and_hms(2026, 1, 4, 23, 0, 0).unwrap().timestamp();
        let asx_winter = Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap().timestamp();
        for offset in [Some(39600), Some(36000)] {
            assert_eq!(bar_date(asx_summer, offset), NaiveDate::from_ymd_opt(2026, 1, 5));
            assert_eq!(bar_date(asx_winter, offset), NaiveDate::from_ymd_opt(2026, 6, 1));
        }
        // NYSE opens 09:30 New York.
        let nyse = Utc.with_ymd_and_hms(2026, 7, 6, 13, 30, 0).unwrap().timestamp();
        for offset in [Some(-14400), Some(-18000)] {
            assert_eq!(bar_date(nyse, offset), NaiveDate::from_ymd_opt(2026, 7, 6));
        }
    }

    #[test]
    fn an_empty_result_names_the_symbol() {
        let chart: Chart = serde_json::from_str(r#"{ "result": null, "error": { "code": "Not Found" } }"#).unwrap();
        let err = chart.into_first("JLG.AX").unwrap_err();
        assert!(err.contains("JLG.AX") && err.contains("Not Found"), "{err}");
    }
}

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

    #[test]
    fn an_empty_result_names_the_symbol() {
        let chart: Chart = serde_json::from_str(r#"{ "result": null, "error": { "code": "Not Found" } }"#).unwrap();
        let err = chart.into_first("JLG.AX").unwrap_err();
        assert!(err.contains("JLG.AX") && err.contains("Not Found"), "{err}");
    }
}

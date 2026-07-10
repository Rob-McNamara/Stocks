//! Technical indicator engine — simple moving averages, SMA trend, crossover
//! statistics and volume-change metrics.
//!
//! Ported from `web/src/utils/sma.ts` so the watchlist enrichment and
//! dashboard rankings are computed once, server-side, for every client.

/// Minimal daily price point for indicator calculations.
#[derive(Debug, Clone)]
pub struct PricePoint {
    pub close: Option<f64>,
    pub volume: Option<i64>,
}

/// Simple moving average series aligned with `data`. A slot is None until the
/// window is full or when any close inside the window is missing.
pub fn calculate_sma(data: &[PricePoint], period: usize) -> Vec<Option<f64>> {
    let mut sma: Vec<Option<f64>> = vec![None; data.len()];
    if period == 0 {
        return sma;
    }
    for i in 0..data.len() {
        if i + 1 < period {
            continue;
        }
        let window = &data[i + 1 - period..=i];
        if window.iter().all(|p| p.close.is_some()) {
            let sum: f64 = window.iter().map(|p| p.close.unwrap()).sum();
            sma[i] = Some(sum / period as f64);
        }
    }
    sma
}

/// The most recent non-None SMA value.
pub fn latest_sma(sma: &[Option<f64>]) -> Option<f64> {
    sma.iter().rev().find_map(|v| *v)
}

/// Direction of the SMA over the last `lookback` non-null values.
pub fn sma_trend(sma: &[Option<f64>], lookback: usize) -> Option<&'static str> {
    let mut latest: Option<f64> = None;
    let mut earlier: Option<f64> = None;
    let mut non_null_count = 0;
    for value in sma.iter().rev() {
        if let Some(v) = value {
            non_null_count += 1;
            if latest.is_none() {
                latest = Some(*v);
            } else if non_null_count > lookback {
                earlier = Some(*v);
                break;
            }
        }
    }
    match (latest, earlier) {
        (Some(l), Some(e)) => Some(if l >= e { "up" } else { "down" }),
        _ => None,
    }
}

pub struct CrossoverStats {
    /// Trading days since the close crossed above the SMA
    pub days: i64,
    /// Volume on the crossover day vs the preceding 20-day average, in percent
    pub volume_pct: Option<f64>,
}

/// Walk back from the latest bar to find the last day the close was below the
/// SMA; the following day is the crossover. `today_volume` is used when the
/// crossover is "today" (live price crossed but no stored bar exists yet).
pub fn crossover_stats(history: &[PricePoint], sma: &[Option<f64>], today_volume: Option<i64>) -> CrossoverStats {
    for i in (0..sma.len()).rev() {
        let (Some(sma_v), Some(close)) = (sma[i], history.get(i).and_then(|p| p.close)) else {
            continue;
        };
        if close < sma_v {
            let crossover_idx = i + 1;
            let days = (sma.len() as i64) - 1 - (i as i64);
            let crossover_vol = if crossover_idx < history.len() {
                history[crossover_idx].volume
            } else {
                today_volume
            };
            let prev20: Vec<i64> = history[crossover_idx.saturating_sub(20)..crossover_idx]
                .iter()
                .filter_map(|p| p.volume)
                .filter(|v| *v > 0)
                .collect();
            let mut volume_pct = None;
            if let Some(cv) = crossover_vol.filter(|v| *v > 0) {
                if !prev20.is_empty() {
                    let avg = prev20.iter().sum::<i64>() as f64 / prev20.len() as f64;
                    if avg > 0.0 {
                        volume_pct = Some(((cv as f64 - avg) / avg) * 100.0);
                    }
                }
            }
            return CrossoverStats { days, volume_pct };
        }
    }
    CrossoverStats { days: sma.len() as i64 - 1, volume_pct: None }
}

/// Average volume of the last 5 volume-bearing days vs the previous 5, in
/// percent. None until ten such days exist.
pub fn volume_change_pct(history: &[PricePoint]) -> Option<f64> {
    let vols: Vec<i64> = history.iter().filter_map(|p| p.volume).filter(|v| *v > 0).collect();
    if vols.len() < 10 {
        return None;
    }
    let last10 = &vols[vols.len() - 10..];
    let prev5_avg = last10[..5].iter().sum::<i64>() as f64 / 5.0;
    let last5_avg = last10[5..].iter().sum::<i64>() as f64 / 5.0;
    if prev5_avg > 0.0 {
        Some(((last5_avg - prev5_avg) / prev5_avg) * 100.0)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pts(closes: &[Option<f64>]) -> Vec<PricePoint> {
        closes.iter().map(|c| PricePoint { close: *c, volume: None }).collect()
    }

    fn pts_v(pairs: &[(f64, i64)]) -> Vec<PricePoint> {
        pairs.iter().map(|(c, v)| PricePoint { close: Some(*c), volume: Some(*v) }).collect()
    }

    #[test]
    fn sma_rolls_once_window_full() {
        let sma = calculate_sma(&pts(&[Some(1.0), Some(2.0), Some(3.0), Some(4.0), Some(5.0)]), 3);
        assert_eq!(sma[0], None);
        assert_eq!(sma[1], None);
        assert_eq!(sma[2], Some(2.0));
        assert_eq!(sma[3], Some(3.0));
        assert_eq!(sma[4], Some(4.0));
    }

    #[test]
    fn sma_null_close_voids_window() {
        let sma = calculate_sma(&pts(&[Some(1.0), None, Some(3.0), Some(4.0), Some(5.0)]), 3);
        assert_eq!(sma[2], None); // window contains the None
        assert_eq!(sma[4], Some(4.0)); // 3,4,5
    }

    #[test]
    fn latest_sma_skips_trailing_none() {
        assert_eq!(latest_sma(&[None, Some(2.0), Some(3.0), None]), Some(3.0));
        assert_eq!(latest_sma(&[None, None]), None);
    }

    #[test]
    fn trend_up_and_down() {
        let up: Vec<Option<f64>> = (1..=10).map(|v| Some(v as f64)).collect();
        assert_eq!(sma_trend(&up, 5), Some("up"));
        let down: Vec<Option<f64>> = (1..=10).rev().map(|v| Some(v as f64)).collect();
        assert_eq!(sma_trend(&down, 5), Some("down"));
        assert_eq!(sma_trend(&[None, Some(1.0), Some(2.0)], 5), None);
    }

    #[test]
    fn crossover_counts_days_above() {
        // closes: below, below, cross above 2 days ago
        let history = pts_v(&[(10.0, 100), (9.0, 100), (11.0, 300), (12.0, 100)]);
        let sma = vec![Some(10.0), Some(10.0), Some(10.0), Some(10.0)];
        let stats = crossover_stats(&history, &sma, None);
        // last below at index 1 → days = 4-1-1 = 2, crossover at index 2
        assert_eq!(stats.days, 2);
        // prev volumes before crossover: [100, 100] avg 100; crossover vol 300 → +200%
        assert_eq!(stats.volume_pct, Some(200.0));
    }

    #[test]
    fn crossover_today_uses_live_volume() {
        // never below within window until the live price crosses today
        let history = pts_v(&[(9.0, 100), (9.5, 100)]);
        let sma = vec![Some(10.0), Some(10.0)];
        let stats = crossover_stats(&history, &sma, Some(400));
        // last close (9.5) below sma → crossover_idx = 2 == len → live volume
        assert_eq!(stats.days, 0);
        assert_eq!(stats.volume_pct, Some(300.0));
    }

    #[test]
    fn crossover_never_below_returns_full_span() {
        let history = pts_v(&[(11.0, 100), (12.0, 100), (13.0, 100)]);
        let sma = vec![Some(10.0), Some(10.0), Some(10.0)];
        let stats = crossover_stats(&history, &sma, None);
        assert_eq!(stats.days, 2);
        assert_eq!(stats.volume_pct, None);
    }

    #[test]
    fn volume_change_needs_ten_points() {
        let short = pts_v(&[(1.0, 100); 9]);
        assert_eq!(volume_change_pct(&short), None);
        let mut pairs = vec![(1.0, 100); 5];
        pairs.extend(vec![(1.0, 200); 5]);
        assert_eq!(volume_change_pct(&pts_v(&pairs)), Some(100.0));
    }

    #[test]
    fn volume_change_ignores_zero_volume_days() {
        let mut pairs = vec![(1.0, 100); 5];
        pairs.push((1.0, 0)); // ignored
        pairs.extend(vec![(1.0, 300); 5]);
        assert_eq!(volume_change_pct(&pts_v(&pairs)), Some(200.0));
    }
}

//! The Dashboard's sector rotation map: sets of sector ETFs or indices, each
//! measured against its own benchmark over 1D, 1W, 1M and 3M.
//!
//! The sets live in one app_config value, edited on the Configuration screen.
//! app_config is audited as a whole, so they need no table and no triggers of
//! their own. Their prices take the same path as everything else's: the startup
//! refresh quotes them, and `prices` keeps their daily closes.

use super::*;
use stocks::rotation::{period_returns, PeriodReturns};

/// app_config key holding the sets, as JSON.
pub(crate) const SECTOR_ROTATION: &str = "sector_rotation";

/// Stamped by the startup refresh when it quotes the sets' symbols. Their own
/// stamp, because sharing the holdings one would make the holdings' prices look
/// fresher than they are.
pub(crate) const SECTOR_PRICES_UPDATED_AT: &str = "sector_prices_updated_at";

/// Kept small because every symbol in every set is quoted on each refresh and
/// read on each Dashboard load.
const MAX_SETS: usize = 10;
const MAX_SECTORS_PER_SET: usize = 30;

/// Stored bars read per symbol: three months back, plus the holidays and the
/// lookback either side of it.
const HISTORY_BARS: i64 = 100;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct RotationSetConfig {
    pub(crate) name: String,
    pub(crate) benchmark: BenchmarkConfig,
    #[serde(default)]
    pub(crate) sectors: Vec<SectorConfig>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct BenchmarkConfig {
    pub(crate) symbol: String,
    #[serde(default)]
    pub(crate) name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SectorConfig {
    pub(crate) symbol: String,
    /// A name from the app's sector list, so a set lines up with the sectors
    /// holdings are tagged with. Saving a set adds any name the list lacks.
    pub(crate) sector: String,
}

/// What a database starts with: the SPDR sector ETFs against SPY, and the
/// S&P/ASX 200 sector indices against the ASX 200.
pub(crate) fn default_rotation_sets() -> Vec<RotationSetConfig> {
    let set = |name: &str, (symbol, benchmark): (&str, &str), sectors: &[(&str, &str)]| RotationSetConfig {
        name: name.to_string(),
        benchmark: BenchmarkConfig { symbol: symbol.to_string(), name: benchmark.to_string() },
        sectors: sectors
            .iter()
            .map(|(symbol, sector)| SectorConfig { symbol: symbol.to_string(), sector: sector.to_string() })
            .collect(),
    };
    vec![
        set(
            "US",
            ("SPY", "S&P 500"),
            &[
                ("XLK", "Technology"),
                ("XLF", "Financials"),
                ("XLB", "Materials"),
                ("XLC", "Communication Services"),
                ("XLV", "Health Care"),
                ("XLU", "Utilities"),
                ("XLY", "Consumer Discretionary"),
                ("XLI", "Industrials"),
                ("XLRE", "Real Estate"),
                ("XLP", "Consumer Staples"),
                ("XLE", "Energy"),
            ],
        ),
        set(
            "ASX",
            ("^AXJO", "S&P/ASX 200"),
            &[
                ("^AXIJ", "Technology"),
                ("^AXFJ", "Financials"),
                ("^AXMJ", "Materials"),
                ("^AXTJ", "Communication Services"),
                ("^AXHJ", "Health Care"),
                ("^AXUJ", "Utilities"),
                ("^AXDJ", "Consumer Discretionary"),
                ("^AXNJ", "Industrials"),
                ("^AXPJ", "Real Estate"),
                ("^AXSJ", "Consumer Staples"),
                ("^AXEJ", "Energy"),
            ],
        ),
    ]
}

/// Reject a set the Dashboard could not show, naming the set and the entry.
pub(crate) fn validate_sector_rotation(value: &str) -> Result<(), String> {
    let parsed: serde_json::Value =
        serde_json::from_str(value).map_err(|err| format!("not valid JSON: {err}"))?;
    let sets = parsed.as_array().ok_or("must be a JSON array of sets")?;
    if sets.len() > MAX_SETS {
        return Err(format!("{} sets is more than the {MAX_SETS} allowed", sets.len()));
    }
    fn text(value: Option<&serde_json::Value>) -> Option<&str> {
        value.and_then(|v| v.as_str()).map(str::trim).filter(|s| !s.is_empty())
    }

    let mut names = std::collections::HashSet::new();
    for (index, set) in sets.iter().enumerate() {
        let at = |message: String| format!("set {}: {}", index + 1, message);
        let obj = set.as_object().ok_or_else(|| at("must be an object".into()))?;

        let name = text(obj.get("name")).ok_or_else(|| at("name is required".into()))?;
        if !names.insert(name.to_lowercase()) {
            return Err(at(format!("duplicate name '{name}' — the Dashboard switches between sets by name")));
        }

        let benchmark = obj
            .get("benchmark")
            .and_then(|b| b.as_object())
            .ok_or_else(|| at("benchmark is required".into()))?;
        let benchmark_symbol = normalize_symbol(
            text(benchmark.get("symbol")).ok_or_else(|| at("benchmark symbol is required".into()))?,
        );
        if benchmark.get("name").is_some_and(|n| !n.is_string()) {
            return Err(at("benchmark name must be text".into()));
        }

        let sectors: &[serde_json::Value] = match obj.get("sectors") {
            None => &[],
            Some(list) => list.as_array().ok_or_else(|| at("sectors must be a list".into()))?.as_slice(),
        };
        if sectors.len() > MAX_SECTORS_PER_SET {
            return Err(at(format!("{} sectors is more than the {MAX_SECTORS_PER_SET} allowed", sectors.len())));
        }
        let mut symbols = std::collections::HashSet::new();
        for (row, sector) in sectors.iter().enumerate() {
            let at = |message: String| format!("set {}, sector {}: {}", index + 1, row + 1, message);
            let obj = sector.as_object().ok_or_else(|| at("must be an object".into()))?;
            let symbol = normalize_symbol(text(obj.get("symbol")).ok_or_else(|| at("symbol is required".into()))?);
            text(obj.get("sector")).ok_or_else(|| at("sector is required".into()))?;
            if symbol == benchmark_symbol {
                return Err(at(format!("{symbol} is the set's benchmark")));
            }
            if !symbols.insert(symbol.clone()) {
                return Err(at(format!("{symbol} is listed twice")));
            }
        }
    }
    Ok(())
}

/// Add to the sector list any of `names` it lacks, keeping "Others" last, and
/// return the names added.
///
/// The list is only rewritten when something is added, so saving sets that
/// name nothing new leaves no audit row behind.
pub(crate) fn ensure_sectors_listed(conn: &Connection, names: &[String]) -> Result<Vec<String>, String> {
    let stored: Option<String> = conn
        .query_row("SELECT value FROM app_config WHERE key = 'sectors'", [], |row| row.get(0))
        .optional()
        .map_err(|err| err.to_string())?;
    let mut list: Vec<String> = serde_json::from_str(stored.as_deref().unwrap_or(DEFAULT_SECTORS_JSON))
        .map_err(|err| format!("the sector list could not be read: {err}"))?;

    let mut added: Vec<String> = Vec::new();
    for name in names.iter().map(|n| n.trim()).filter(|n| !n.is_empty()) {
        if !list.iter().chain(&added).any(|s| s == name) {
            added.push(name.to_string());
        }
    }
    if added.is_empty() {
        return Ok(added);
    }
    let at = list.iter().position(|s| s == "Others").unwrap_or(list.len());
    list.splice(at..at, added.iter().cloned());
    let json = serde_json::to_string(&list).map_err(|err| err.to_string())?;
    conn.execute(
        "INSERT INTO app_config (key, value) VALUES ('sectors', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![json],
    )
    .map_err(|err| err.to_string())?;
    Ok(added)
}

/// Seed the default sets on a database that has none.
///
/// The sector list is only extended when the seed itself lands, so a sector
/// removed from the list later is not put back at every start.
pub(crate) fn seed_sector_rotation(conn: &Connection) -> Result<(), String> {
    let sets = default_rotation_sets();
    let json = serde_json::to_string(&sets).map_err(|err| err.to_string())?;
    let seeded = conn
        .execute("INSERT OR IGNORE INTO app_config (key, value) VALUES (?1, ?2)", params![SECTOR_ROTATION, json])
        .map_err(|err| err.to_string())?;
    if seeded > 0 {
        let names: Vec<String> = sets.iter().flat_map(|s| s.sectors.iter().map(|x| x.sector.clone())).collect();
        ensure_sectors_listed(conn, &names)?;
    }
    Ok(())
}

/// Store the sets, adding to the sector list any sector they name that it
/// lacks. One transaction, so the sets never name a sector the list has not got.
pub(crate) fn save_sector_rotation(db_path: &Path, value: &str) -> Result<Vec<String>, String> {
    let sets: Vec<RotationSetConfig> = serde_json::from_str(value).map_err(|err| err.to_string())?;
    let names: Vec<String> = sets.iter().flat_map(|s| s.sectors.iter().map(|x| x.sector.clone())).collect();
    with_tx(db_path, |tx| {
        tx.execute(
            "INSERT INTO app_config (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![SECTOR_ROTATION, value],
        )
        .map_err(|err| err.to_string())?;
        ensure_sectors_listed(tx, &names)
    })
}

/// The configured sets, symbols normalised. An unreadable value reads as no
/// sets — `config_json` leaves the reason in the event log.
pub(crate) fn rotation_sets(db_path: &Path, config: &HashMap<String, String>) -> Vec<RotationSetConfig> {
    let mut sets: Vec<RotationSetConfig> = config_json(db_path, config, SECTOR_ROTATION);
    for set in &mut sets {
        set.benchmark.symbol = normalize_symbol(&set.benchmark.symbol);
        for sector in &mut set.sectors {
            sector.symbol = normalize_symbol(&sector.symbol);
            sector.sector = sector.sector.trim().to_string();
        }
    }
    sets
}

fn unique_symbols(sets: &[RotationSetConfig]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    sets.iter()
        .flat_map(|set| std::iter::once(&set.benchmark.symbol).chain(set.sectors.iter().map(|s| &s.symbol)))
        .filter(|symbol| seen.insert(symbol.as_str()))
        .cloned()
        .collect()
}

/// Every benchmark and sector symbol across the sets, once each — what the
/// startup refresh quotes.
pub(crate) fn sector_rotation_symbols(db_path: &Path) -> Result<Vec<String>, String> {
    let config: HashMap<String, String> =
        load_config(&db_path.to_path_buf())?.into_iter().map(|c| (c.key, c.value)).collect();
    Ok(unique_symbols(&rotation_sets(db_path, &config)))
}

#[derive(Serialize)]
pub(crate) struct RotationBenchmark {
    symbol: String,
    name: String,
    long_name: Option<String>,
    last_date: Option<String>,
    returns: PeriodReturns,
}

#[derive(Serialize)]
pub(crate) struct RotationEntry {
    symbol: String,
    sector: String,
    long_name: Option<String>,
    /// Date of the newest stored close. When it falls before the set's `as_of`
    /// the returns are empty, and this says how far behind the symbol is.
    last_date: Option<String>,
    returns: PeriodReturns,
    /// `returns` minus the benchmark's, in percentage points.
    relative: PeriodReturns,
}

#[derive(Serialize)]
pub(crate) struct RotationSet {
    name: String,
    /// The day every figure in the set runs to.
    as_of: Option<String>,
    benchmark: RotationBenchmark,
    sectors: Vec<RotationEntry>,
}

/// One set's figures.
///
/// The as-of date is the benchmark's newest close, so every sector is measured
/// over the same days as the figure it is compared with. With no benchmark
/// price at all it falls back to the newest sector close, so a mistyped
/// benchmark still leaves the sectors' own moves on screen.
fn measure_set(
    set: RotationSetConfig,
    closes: &HashMap<String, Vec<(NaiveDate, f64)>>,
    long_names: &HashMap<String, String>,
) -> RotationSet {
    let closes_of = |symbol: &str| closes.get(symbol).map(Vec::as_slice).unwrap_or_default();
    let newest = |symbol: &str| closes_of(symbol).last().map(|(date, _)| *date);
    let as_of = newest(&set.benchmark.symbol).or_else(|| set.sectors.iter().filter_map(|s| newest(&s.symbol)).max());
    let measure = |symbol: &str| as_of.map(|day| period_returns(closes_of(symbol), day)).unwrap_or_default();
    let date_text = |date: NaiveDate| date.format("%Y-%m-%d").to_string();

    let benchmark_returns = measure(&set.benchmark.symbol);
    let sectors = set
        .sectors
        .into_iter()
        .map(|sector| {
            let returns = measure(&sector.symbol);
            RotationEntry {
                long_name: long_names.get(&sector.symbol).cloned(),
                last_date: newest(&sector.symbol).map(date_text),
                relative: returns.minus(&benchmark_returns),
                returns,
                symbol: sector.symbol,
                sector: sector.sector,
            }
        })
        .collect();
    RotationSet {
        name: set.name,
        as_of: as_of.map(date_text),
        benchmark: RotationBenchmark {
            long_name: long_names.get(&set.benchmark.symbol).cloned(),
            last_date: newest(&set.benchmark.symbol).map(date_text),
            returns: benchmark_returns,
            name: set.benchmark.name,
            symbol: set.benchmark.symbol,
        },
        sectors,
    }
}

/// Yahoo's full names, for the tiles' tooltips. Stored by every quote fetch.
fn long_names(conn: &Connection, symbols: &[String]) -> rusqlite::Result<HashMap<String, String>> {
    if symbols.is_empty() {
        return Ok(HashMap::new());
    }
    let placeholders = vec!["?"; symbols.len()].join(",");
    let mut stmt = conn.prepare(&format!(
        "SELECT symbol, long_name FROM symbol_info WHERE long_name IS NOT NULL AND symbol IN ({placeholders})"
    ))?;
    let rows = stmt.query_map(rusqlite::params_from_iter(symbols), |row| Ok((row.get(0)?, row.get(1)?)))?;
    rows.collect()
}

#[utoipa::path(
    get,
    path = "/api/v1/sector-rotation",
    tag = "prices",
    responses((status = 200, description = "Each configured sector set's 1D, 1W, 1M and 3M price moves, alone and against its benchmark"))
)]
#[get("/api/sector-rotation")]
pub(crate) async fn get_sector_rotation(db_path: web::Data<PathBuf>) -> impl Responder {
    let config: HashMap<String, String> = match load_config(&db_path) {
        Ok(items) => items.into_iter().map(|c| (c.key, c.value)).collect(),
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "sector_rotation", "api", None, &err);
            return err_internal(err);
        }
    };
    let sets = rotation_sets(&db_path, &config);
    let symbols = unique_symbols(&sets);

    // Stored bars, topped up from Yahoo at most once per symbol every ten
    // minutes — the same path the watchlist and the overview read through. A
    // symbol whose history cannot be had is logged there and shows as blank.
    let histories = fetch_histories(&db_path, &symbols, HISTORY_BARS).await;
    let closes: HashMap<String, Vec<(NaiveDate, f64)>> = histories
        .into_iter()
        .map(|(symbol, bars)| {
            let series = bars
                .into_iter()
                .filter_map(|bar| Some((NaiveDate::parse_from_str(&bar.date, "%Y-%m-%d").ok()?, bar.close?)))
                .collect();
            (symbol, series)
        })
        .collect();

    let names = match open_db(db_path.as_ref()).and_then(|conn| long_names(&conn, &symbols)) {
        Ok(names) => names,
        Err(err) => {
            let _ = insert_event_log(&db_path, "warn", "sector_rotation", "api", None, &format!("Names not read, tiles will show codes only: {err}"));
            HashMap::new()
        }
    };

    let sets: Vec<RotationSet> = sets.into_iter().map(|set| measure_set(set, &closes, &names)).collect();
    HttpResponse::Ok().json(serde_json::json!({ "sets": sets }))
}

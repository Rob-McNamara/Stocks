//! Price levels and trendlines drawn on a symbol's chart.

use super::*;

#[derive(Serialize)]
pub(crate) struct ChartDrawing {
    pub(crate) id: i64,
    pub(crate) symbol: String,
    pub(crate) kind: String,
    /// In the symbol's own currency — the chart applies the FX rate at render.
    pub(crate) price: f64,
    pub(crate) label: Option<String>,
    pub(crate) colour: Option<String>,
    /// Trendlines only: the two anchors are (start_date, price) and
    /// (end_date, end_price). Null on a horizontal level.
    pub(crate) start_date: Option<String>,
    pub(crate) end_date: Option<String>,
    pub(crate) end_price: Option<f64>,
    pub(crate) created_at: String,
}

#[derive(Deserialize)]
pub(crate) struct NewChartDrawing {
    /// Omitted for a horizontal level, which is the default.
    pub(crate) kind: Option<String>,
    pub(crate) price: f64,
    pub(crate) label: Option<String>,
    pub(crate) colour: Option<String>,
    pub(crate) start_date: Option<String>,
    pub(crate) end_date: Option<String>,
    pub(crate) end_price: Option<f64>,
}

pub(crate) fn load_chart_drawings(conn: &Connection, symbol: &str) -> Result<Vec<ChartDrawing>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT id, symbol, kind, price, label, colour, start_date, end_date, end_price, created_at
               FROM chart_drawings WHERE symbol = ?1 ORDER BY price DESC, id",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![symbol], |r| {
            Ok(ChartDrawing {
                id: r.get(0)?,
                symbol: r.get(1)?,
                kind: r.get(2)?,
                price: r.get(3)?,
                label: r.get(4)?,
                colour: r.get(5)?,
                start_date: r.get(6)?,
                end_date: r.get(7)?,
                end_price: r.get(8)?,
                created_at: r.get(9)?,
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
}

#[utoipa::path(get, path = "/api/v1/chart-drawings/{symbol}", tag = "charts",
    params(("symbol" = String, Path, description = "symbol")),
    responses((status = 200, description = "Price levels drawn on this symbol's chart")))]
#[get("/api/chart-drawings/{symbol}")]
pub(crate) async fn get_chart_drawings(db_path: web::Data<PathBuf>, path: web::Path<String>) -> impl Responder {
    let symbol = path.into_inner().to_uppercase();
    let conn = match open_db(db_path.as_ref()) {
        Ok(c) => c,
        Err(err) => return err_internal(err.to_string()),
    };
    match load_chart_drawings(&conn, &symbol) {
        Ok(rows) => HttpResponse::Ok().json(serde_json::json!({ "drawings": rows })),
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "chart_drawings", "api", Some(&symbol), &err);
            err_internal(err)
        }
    }
}

#[utoipa::path(post, path = "/api/v1/chart-drawings/{symbol}", tag = "charts",
    params(("symbol" = String, Path, description = "symbol")),
    responses((status = 200, description = "Draw a horizontal price level")))]
#[post("/api/chart-drawings/{symbol}")]
pub(crate) async fn add_chart_drawing(
    db_path: web::Data<PathBuf>,
    path: web::Path<String>,
    payload: web::Json<NewChartDrawing>,
) -> impl Responder {
    let symbol = path.into_inner().to_uppercase();
    let payload = payload.into_inner();
    // A level at or below zero is not a price. Rejecting it here keeps a
    // mis-drag from writing a line that can never be seen on the chart.
    if !payload.price.is_finite() || payload.price <= 0.0 {
        return err_bad_request("Price level must be a positive number".to_string());
    }
    let kind = payload.kind.as_deref().unwrap_or("horizontal");
    if kind == "trend" {
        // A trendline is defined by two anchors. Storing one with a missing or
        // non-positive second anchor would leave a row that can be read but
        // never drawn — a line with no slope and no end.
        let ok = payload.start_date.is_some()
            && payload.end_date.is_some()
            && payload.end_price.is_some_and(|p| p.is_finite() && p > 0.0);
        if !ok {
            return err_bad_request(
                "A trendline needs both anchors: start_date, end_date and a positive end_price".to_string(),
            );
        }
        if payload.start_date == payload.end_date {
            return err_bad_request("A trendline's two anchors must be on different dates".to_string());
        }
    } else if kind != "horizontal" {
        return err_bad_request(format!("Unknown drawing kind '{}'", kind));
    }
    let conn = match open_db(db_path.as_ref()) {
        Ok(c) => c,
        Err(err) => return err_internal(err.to_string()),
    };
    let result = conn.execute(
        "INSERT INTO chart_drawings (symbol, kind, price, label, colour, start_date, end_date, end_price, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![symbol, kind, payload.price, payload.label, payload.colour,
                payload.start_date, payload.end_date, payload.end_price, Utc::now().to_rfc3339()],
    );
    match result {
        Ok(_) => {
            let id = conn.last_insert_rowid();
            let _ = insert_event_log(&db_path, "info", "chart_drawings", "api", Some(&symbol),
                &format!("Drew level {} at {}", id, payload.price));
            match load_chart_drawings(&conn, &symbol) {
                Ok(rows) => HttpResponse::Ok().json(serde_json::json!({ "drawings": rows })),
                Err(err) => err_internal(err),
            }
        }
        Err(err) => err_internal(err.to_string()),
    }
}

#[derive(Deserialize)]
pub(crate) struct MovedChartDrawing {
    /// A level's price, or a trendline's first anchor.
    pub(crate) price: f64,
    /// Trendlines only, and then all three are required: the line is moved by
    /// restating both anchors.
    pub(crate) start_date: Option<String>,
    pub(crate) end_date: Option<String>,
    pub(crate) end_price: Option<f64>,
}

#[utoipa::path(patch, path = "/api/v1/chart-drawings/id/{id}", tag = "charts",
    params(("id" = i64, Path, description = "id")),
    responses((status = 200, description = "Move a level to a new price, or a trendline to new anchors")))]
#[patch("/api/chart-drawings/id/{id}")]
pub(crate) async fn move_chart_drawing(
    db_path: web::Data<PathBuf>,
    path: web::Path<i64>,
    payload: web::Json<MovedChartDrawing>,
) -> impl Responder {
    let id = path.into_inner();
    let payload = payload.into_inner();
    let price = payload.price;
    // Same rule as drawing one: a level at or below zero can never be seen.
    if !price.is_finite() || price <= 0.0 {
        return err_bad_request("Price level must be a positive number".to_string());
    }
    let conn = match open_db(db_path.as_ref()) {
        Ok(c) => c,
        Err(err) => return err_internal(err.to_string()),
    };
    let found: Option<(String, String, f64)> = match conn
        .query_row(
            "SELECT symbol, kind, price FROM chart_drawings WHERE id = ?1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
    {
        Ok(found) => found,
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "chart_drawings", "api", None, &format!("Failed to load drawing {}: {}", id, err));
            return err_internal(err.to_string());
        }
    };
    let Some((symbol, kind, old_price)) = found else {
        return err_not_found(format!("Drawing {} not found", id));
    };
    let has_anchors = payload.start_date.is_some() || payload.end_date.is_some() || payload.end_price.is_some();
    let (result, moved) = if kind == "trend" {
        // A trendline's `price` is only its first anchor; moving that alone
        // would silently change the line's slope, so both anchors are restated.
        let (Some(start_date), Some(end_date), Some(end_price)) =
            (payload.start_date.as_deref(), payload.end_date.as_deref(), payload.end_price)
        else {
            return err_bad_request(
                "Moving a trendline needs both anchors: start_date, end_date and end_price".to_string(),
            );
        };
        if !end_price.is_finite() || end_price <= 0.0 {
            return err_bad_request("A trendline's end_price must be a positive number".to_string());
        }
        if start_date == end_date {
            return err_bad_request("A trendline's two anchors must be on different dates".to_string());
        }
        (
            conn.execute(
                "UPDATE chart_drawings SET price = ?1, start_date = ?2, end_date = ?3, end_price = ?4 WHERE id = ?5",
                params![price, start_date, end_date, end_price, id],
            ),
            format!("Moved trendline {} to {} {} → {} {}", id, start_date, price, end_date, end_price),
        )
    } else {
        if has_anchors {
            return err_bad_request("A horizontal level has only a price to move".to_string());
        }
        (
            conn.execute("UPDATE chart_drawings SET price = ?1 WHERE id = ?2", params![price, id]),
            format!("Moved level {} from {} to {}", id, old_price, price),
        )
    };
    if let Err(err) = result {
        let _ = insert_event_log(&db_path, "error", "chart_drawings", "api", Some(&symbol), &format!("Failed to move drawing {}: {}", id, err));
        return err_internal(err.to_string());
    }
    let _ = insert_event_log(&db_path, "info", "chart_drawings", "api", Some(&symbol), &moved);
    match load_chart_drawings(&conn, &symbol) {
        Ok(rows) => HttpResponse::Ok().json(serde_json::json!({ "drawings": rows })),
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "chart_drawings", "api", Some(&symbol), &err);
            err_internal(err)
        }
    }
}

#[utoipa::path(delete, path = "/api/v1/chart-drawings/id/{id}", tag = "charts",
    params(("id" = i64, Path, description = "id")),
    responses((status = 204, description = "Remove a drawn level")))]
#[delete("/api/chart-drawings/id/{id}")]
pub(crate) async fn delete_chart_drawing(db_path: web::Data<PathBuf>, path: web::Path<i64>) -> impl Responder {
    let id = path.into_inner();
    let conn = match open_db(db_path.as_ref()) {
        Ok(c) => c,
        Err(err) => return err_internal(err.to_string()),
    };
    match conn.execute("DELETE FROM chart_drawings WHERE id = ?1", params![id]) {
        Ok(0) => err_not_found(format!("Drawing {} not found", id)),
        Ok(_) => {
            let _ = insert_event_log(&db_path, "info", "chart_drawings", "api", None, &format!("Removed level {}", id));
            HttpResponse::NoContent().finish()
        }
        Err(err) => err_internal(err.to_string()),
    }
}

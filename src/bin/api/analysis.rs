//! AI stock analysis: the conversation history and the calls to the configured
//! provider.

use super::*;

#[derive(Deserialize)]
pub(crate) struct AnalysisMessage {
    pub(crate) role: String,
    pub(crate) content: String,
}

#[derive(Deserialize)]
pub(crate) struct AnalysisRequest {
    pub(crate) symbol: String,
    pub(crate) messages: Vec<AnalysisMessage>,
}

#[derive(Deserialize)]
pub(crate) struct AnalysisHistoryQuery {
    pub(crate) symbol: String,
}

#[derive(Serialize)]
pub(crate) struct AnalysisHistoryEntry {
    pub(crate) id: i64,
    pub(crate) role: String,
    pub(crate) content: String,
    pub(crate) model_used: Option<String>,
    pub(crate) created_at: String,
}

#[utoipa::path(get, path = "/api/v1/stock-analysis/history", tag = "analysis", responses((status = 200, description = "Get analysis history")))]
#[get("/api/stock-analysis/history")]
pub(crate) async fn get_analysis_history(db_path: web::Data<PathBuf>, query: web::Query<AnalysisHistoryQuery>) -> impl Responder {
    // Messages are stored under the normalized symbol — query the same way.
    let symbol = normalize_symbol(&query.symbol);
    let conn = match open_db(db_path.as_ref()) {
        Ok(c) => c,
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "analysis_history_fetch", "api", Some(&symbol), &err.to_string());
            return err_internal(err.to_string());
        }
    };
    let mut stmt = match conn.prepare(
        "SELECT id, role, content, model_used, created_at FROM stock_analysis_messages WHERE symbol = ?1 ORDER BY created_at ASC, id ASC"
    ) {
        Ok(s) => s,
        Err(err) => return err_internal(err.to_string()),
    };
    let rows = stmt.query_map(params![symbol], |row| {
        Ok(AnalysisHistoryEntry {
            id: row.get(0)?,
            role: row.get(1)?,
            content: row.get(2)?,
            model_used: row.get(3)?,
            created_at: row.get(4)?,
        })
    });
    match rows {
        Ok(mapped) => match collect_rows(&db_path, "analysis_history_fetch", mapped) {
            Ok(entries) => HttpResponse::Ok().json(entries),
            Err(response) => response,
        },
        Err(err) => err_internal(err.to_string()),
    }
}

#[utoipa::path(delete, path = "/api/v1/stock-analysis/history", tag = "analysis", responses((status = 200, description = "Delete analysis history")))]
#[delete("/api/stock-analysis/history")]
pub(crate) async fn delete_analysis_history(db_path: web::Data<PathBuf>, query: web::Query<AnalysisHistoryQuery>) -> impl Responder {
    let symbol = normalize_symbol(&query.symbol);
    let conn = match open_db(db_path.as_ref()) {
        Ok(c) => c,
        Err(err) => return err_internal(err.to_string()),
    };
    match conn.execute("DELETE FROM stock_analysis_messages WHERE symbol = ?1", params![symbol]) {
        Ok(_) => HttpResponse::NoContent().finish(),
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "analysis_history_delete", "api", Some(&symbol), &err.to_string());
            err_internal(err.to_string())
        }
    }
}

/// How long an AI analysis request may take.
pub(crate) const ANALYSIS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// Used when no `ai_model` is configured.
pub(crate) const DEFAULT_ANALYSIS_MODEL: &str = "claude-sonnet-5-5";

#[utoipa::path(post, path = "/api/v1/stock-analysis", tag = "analysis", responses((status = 200, description = "Post stock analysis")))]
#[post("/api/stock-analysis")]
pub(crate) async fn post_stock_analysis(
    db_path: web::Data<PathBuf>,
    payload: web::Json<AnalysisRequest>,
) -> impl Responder {
    let symbol = normalize_symbol(&payload.symbol);

    // Load AI config
    let config = match load_config(&db_path) {
        Ok(items) => items.into_iter().map(|c| (c.key, c.value)).collect::<HashMap<String, String>>(),
        Err(err) => return err_internal(format!("Failed to load config: {}", err)),
    };
    let provider = config.get("ai_provider").map(|s| s.as_str()).unwrap_or("anthropic");
    let api_key = match config.get("ai_api_key") {
        Some(k) if !k.is_empty() => k.clone(),
        _ => return err_bad_request("AI API key not configured. Set it in Configuration."),
    };
    let model = config.get("ai_model").map(|s| s.as_str()).unwrap_or(DEFAULT_ANALYSIS_MODEL).to_string();

    // Build local context for the system prompt
    let mut context_parts: Vec<String> = Vec::new();
    if let Ok(conn) = open_db(db_path.as_ref()) {
        // Cached price
        if let Ok(row) = conn.query_row(
            "SELECT price, change, change_percent, volume, price_date FROM cached_current_prices WHERE symbol = ?1",
            params![symbol],
            |row| Ok((
                row.get::<_, Option<f64>>(0)?,
                row.get::<_, Option<f64>>(1)?,
                row.get::<_, Option<f64>>(2)?,
                row.get::<_, Option<i64>>(3)?,
                row.get::<_, Option<String>>(4)?,
            )),
        )
            && let Some(price) = row.0 {
                let mut line = format!("Current price: ${:.2}", price);
                if let Some(chg) = row.1 { line.push_str(&format!(", change: {:.2}", chg)); }
                if let Some(pct) = row.2 { line.push_str(&format!(" ({:.2}%)", pct)); }
                if let Some(vol) = row.3 { line.push_str(&format!(", volume: {}", vol)); }
                if let Some(ref date) = row.4 { line.push_str(&format!(", as of {}", date)); }
                context_parts.push(line);
            }
        // Symbol info
        if let Ok(info) = conn.query_row(
            "SELECT instrument_type, long_name, currency FROM symbol_info WHERE symbol = ?1",
            params![symbol],
            |row| Ok((
                row.get::<_, Option<String>>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
            )),
        ) {
            let mut parts = Vec::new();
            if let Some(ref name) = info.1 { parts.push(format!("Name: {}", name)); }
            if let Some(ref itype) = info.0 { parts.push(format!("Type: {}", itype)); }
            if let Some(ref cur) = info.2 { parts.push(format!("Currency: {}", cur)); }
            if !parts.is_empty() { context_parts.push(parts.join(", ")); }
        }
        // Built-in watchlist price fields
        if let Ok(row) = conn.query_row(
            "SELECT breakthrough_price, stop_loss_price FROM watchlist_symbols WHERE symbol = ?1",
            params![symbol],
            |row| Ok((row.get::<_, Option<f64>>(0)?, row.get::<_, Option<f64>>(1)?)),
        ) {
            let mut parts = Vec::new();
            if let Some(bp) = row.0 { parts.push(format!("Breakthrough Price: {:.2}", bp)); }
            if let Some(sl) = row.1 { parts.push(format!("Stop Loss Price: {:.2}", sl)); }
            if !parts.is_empty() { context_parts.push(parts.join(", ")); }
        }
        // Custom fields (watchlist + holdings)
        let mut fields_stmt = conn.prepare("SELECT field_key, value FROM watchlist_symbol_fields WHERE symbol = ?1").ok();
        if let Some(ref mut stmt) = fields_stmt
            && let Ok(rows) = stmt.query_map(params![symbol], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))) {
                let fields: Vec<String> = rows.filter_map(|r| r.ok()).map(|(k, v)| format!("{}: {}", k, v)).collect();
                if !fields.is_empty() { context_parts.push(format!("Watchlist fields: {}", fields.join(", "))); }
            }
        let mut hf_stmt = conn.prepare("SELECT field_key, value FROM holdings_symbol_fields WHERE symbol = ?1").ok();
        if let Some(ref mut stmt) = hf_stmt
            && let Ok(rows) = stmt.query_map(params![symbol], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))) {
                let fields: Vec<String> = rows.filter_map(|r| r.ok()).map(|(k, v)| format!("{}: {}", k, v)).collect();
                if !fields.is_empty() { context_parts.push(format!("Holdings fields: {}", fields.join(", "))); }
            }
    }

    let system_prompt = format!(
        "You are a stock market analyst. Analyze the stock {} using web search to find the latest news, analyst ratings, financial data, and technical analysis. \
         Provide a comprehensive but concise analysis covering: recent news, fundamental outlook, technical indicators, and a summary recommendation.\n\n\
         Local data from user's portfolio:\n{}",
        symbol,
        if context_parts.is_empty() { "No local data available.".to_string() } else { context_parts.join("\n") }
    );

    let client = http_client();

    // Save user message to history
    let now = Utc::now().to_rfc3339();
    if let Some(last_msg) = payload.messages.last()
        && let Err(err) = open_db(db_path.as_ref()).and_then(|conn| {
            conn.execute(
                "INSERT INTO stock_analysis_messages (symbol, role, content, created_at) VALUES (?1, ?2, ?3, ?4)",
                params![symbol, last_msg.role, last_msg.content, now],
            )
        })
    {
        let _ = insert_event_log(&db_path, "warn", "stock_analysis", "api", Some(&symbol), &format!("Question not saved to history: {}", err));
    }

    let result = if provider == "openai" {
        call_openai_api(client, &api_key, &model, &system_prompt, &payload.messages).await
    } else {
        call_anthropic_api(client, &api_key, &model, &system_prompt, &payload.messages).await
    };

    match result {
        Ok(response_text) => {
            // Save assistant response to history
            let now2 = Utc::now().to_rfc3339();
            if let Err(err) = open_db(db_path.as_ref()).and_then(|conn| {
                conn.execute(
                    "INSERT INTO stock_analysis_messages (symbol, role, content, model_used, created_at) VALUES (?1, 'assistant', ?2, ?3, ?4)",
                    params![symbol, response_text, model, now2],
                )
            }) {
                let _ = insert_event_log(&db_path, "warn", "stock_analysis", "api", Some(&symbol), &format!("Answer not saved to history: {}", err));
            }
            HttpResponse::Ok().json(serde_json::json!({ "role": "assistant", "content": response_text }))
        }
        Err(err) => {
            let _ = insert_event_log(&db_path, "error", "stock_analysis", "api", Some(&symbol), &err);
            err_internal(err)
        }
    }
}

pub(crate) async fn call_anthropic_api(
    client: &Client,
    api_key: &str,
    model: &str,
    system_prompt: &str,
    messages: &[AnalysisMessage],
) -> Result<String, String> {
    let api_messages: Vec<serde_json::Value> = messages.iter().map(|m| {
        serde_json::json!({ "role": m.role, "content": m.content })
    }).collect();

    let body = serde_json::json!({
        "model": model,
        "max_tokens": 4096,
        "system": system_prompt,
        "tools": [{ "type": "web_search_20250305", "name": "web_search", "max_uses": 5 }],
        "messages": api_messages,
    });

    let response = client
        .post("https://api.anthropic.com/v1/messages")
        // An analysis runs several web searches before it answers, so it gets
        // far longer than the shared client's market-data timeout.
        .timeout(ANALYSIS_TIMEOUT)
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("Anthropic API request failed: {}", e))?;

    let status = response.status();
    let response_text = response.text().await.map_err(|e| format!("Failed to read response: {}", e))?;

    if !status.is_success() {
        return Err(format!("Anthropic API error ({}): {}", status, response_text));
    }

    let data: serde_json::Value = serde_json::from_str(&response_text)
        .map_err(|e| format!("Failed to parse Anthropic response: {}", e))?;

    // Extract text from content blocks
    let mut result_text = String::new();
    if let Some(content) = data["content"].as_array() {
        for block in content {
            if block["type"] == "text"
                && let Some(text) = block["text"].as_str() {
                    result_text.push_str(text);
                }
        }
    }

    if result_text.is_empty() {
        Err(format!("No text in Anthropic response: {}", response_text))
    } else {
        Ok(result_text)
    }
}

pub(crate) async fn call_openai_api(
    client: &Client,
    api_key: &str,
    model: &str,
    system_prompt: &str,
    messages: &[AnalysisMessage],
) -> Result<String, String> {
    let mut api_messages = vec![serde_json::json!({ "role": "system", "content": system_prompt })];
    for m in messages {
        api_messages.push(serde_json::json!({ "role": m.role, "content": m.content }));
    }

    let body = serde_json::json!({
        "model": model,
        "messages": api_messages,
        "max_tokens": 4096,
    });

    let response = client
        .post("https://api.openai.com/v1/chat/completions")
        .timeout(ANALYSIS_TIMEOUT)
        .header("Authorization", format!("Bearer {}", api_key))
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("OpenAI API request failed: {}", e))?;

    let status = response.status();
    let response_text = response.text().await.map_err(|e| format!("Failed to read response: {}", e))?;

    if !status.is_success() {
        return Err(format!("OpenAI API error ({}): {}", status, response_text));
    }

    let data: serde_json::Value = serde_json::from_str(&response_text)
        .map_err(|e| format!("Failed to parse OpenAI response: {}", e))?;

    data["choices"][0]["message"]["content"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| format!("No content in OpenAI response: {}", response_text))
}

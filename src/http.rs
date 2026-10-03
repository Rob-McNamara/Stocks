//! The one way every binary builds an HTTP client.
//!
//! reqwest has no timeout by default, so a Yahoo connection that stalls holds
//! its caller forever. The portfolio endpoints await an FX lookup, and the
//! refresh holds a lock while it runs, so a single stuck request used to hang
//! the holdings screens and block every later refresh. Every client starts
//! from these bounds; a caller that legitimately waits longer (the AI analysis,
//! which runs web searches) raises `timeout` on the builder it is handed.

use std::time::Duration;

/// Longest a market-data request may take, end to end.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Longest to wait for a connection to open.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

pub fn builder(user_agent: &str) -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .user_agent(user_agent)
        .timeout(REQUEST_TIMEOUT)
        .connect_timeout(CONNECT_TIMEOUT)
}

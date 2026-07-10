//! Shared library for the Stocks binaries. Holds domain logic that must be
//! identical across every client (web, iOS, Android): the portfolio
//! calculation engine lives here so the API server is the single source of
//! truth for money math.

pub mod indicators;
pub mod portfolio;

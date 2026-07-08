/**
 * GICS-style sector list used for the Sector dropdown on the Holdings screen.
 * The selected value is stored per symbol in holdings_symbol_fields under the
 * built-in key 'sector'. Add or reorder entries here as needed — existing
 * stocks keep whatever value was saved (the dropdowns keep showing a saved
 * value even if it is later removed from this list).
 */
export const SECTORS = [
  'Energy',
  'Materials',
  'Industrials',
  'Consumer Discretionary',
  'Consumer Staples',
  'Health Care',
  'Financials',
  'Information Technology',
  'Communication Services',
  'Utilities',
  'Real Estate',
  'Others',
] as const

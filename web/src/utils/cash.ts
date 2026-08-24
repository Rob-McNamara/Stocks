import type { CashAccount } from '../services/api'

/**
 * Accounts that can settle a trade in `tradeCurrency`.
 *
 * An account in the trade's own currency settles it directly. An AUD account
 * can also settle a foreign trade, because that is what a broker like CommSec
 * International does: the account holds only AUD and each foreign trade is
 * converted at its own rate, so no foreign balance ever exists to draw on.
 *
 * Mirrors `sync_trade_cash_leg` in the API — if this list offers an account the
 * backend refuses, saving fails with a currency error.
 */
export function settlementAccountsFor(accounts: CashAccount[], tradeCurrency: string): CashAccount[] {
  const currency = tradeCurrency || 'AUD'
  return accounts.filter((a) => a.currency === currency || (a.currency === 'AUD' && currency !== 'AUD'))
}

/** Whether picking `account` for a trade in `tradeCurrency` means a conversion. */
export function settlesByConversion(account: CashAccount | undefined, tradeCurrency: string): boolean {
  if (!account) return false
  const currency = tradeCurrency || 'AUD'
  return account.currency === 'AUD' && currency !== 'AUD'
}

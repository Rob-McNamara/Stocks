import { apiClient } from '../services/api'
import { invalidateAppConfig, loadAppConfig } from './appConfig'

/**
 * Single `app_config` key holding the ids of every card the user has folded
 * away, as a JSON array.
 *
 * One key rather than one per card: the set is small, it is read in a single
 * pass at mount alongside every other setting, and a card that is removed from
 * the app leaves one stale id behind instead of its own orphaned row.
 */
export const COLLAPSED_CARDS_KEY = 'collapsed_cards'

/**
 * Anything unparseable reads as "nothing collapsed".
 *
 * A value written by an older version must not leave cards stuck shut with no
 * content and nothing on screen explaining why — an open card is always a
 * recoverable state, a silently empty screen is not.
 */
export function parseCollapsedCards(raw: string | undefined): Set<string> {
  if (!raw) return new Set()
  try {
    const parsed: unknown = JSON.parse(raw)
    if (!Array.isArray(parsed)) return new Set()
    return new Set(parsed.filter((id): id is string => typeof id === 'string'))
  } catch {
    return new Set()
  }
}

export function loadCollapsedCards(): Promise<Set<string>> {
  return loadAppConfig().then((config) => parseCollapsedCards(config[COLLAPSED_CARDS_KEY]))
}

/**
 * Fold or unfold one card, leaving every other card's state alone.
 *
 * Read–modify–write rather than a blind overwrite: several collapsible cards
 * share the one key, so a card that wrote only its own id would shut the rest.
 * The read comes from the cached config, and the cache is dropped before the
 * write so the next reader — this card or another screen — sees the new set.
 */
export async function setCardCollapsed(id: string, collapsed: boolean): Promise<void> {
  const next = new Set(await loadCollapsedCards())
  if (collapsed) next.add(id)
  else next.delete(id)
  invalidateAppConfig()
  await apiClient.updateConfig(COLLAPSED_CARDS_KEY, JSON.stringify([...next].sort()))
}

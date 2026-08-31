import { apiClient } from '../services/api'

/**
 * Shared, cached read of `app_config`.
 *
 * Several components want settings at mount — three charts on the Holdings
 * screen alone — and each would otherwise pull the whole config. The promise is
 * cached rather than the value, so concurrent mounts wait on one request.
 * `invalidateAppConfig` drops it after a save, so the next reader sees the new
 * settings without a page reload.
 */
let configPromise: Promise<Record<string, string>> | null = null

export function loadAppConfig(): Promise<Record<string, string>> {
  configPromise ??= apiClient.getConfig().catch(() => ({}) as Record<string, string>)
  return configPromise
}

export function invalidateAppConfig(): void {
  configPromise = null
}

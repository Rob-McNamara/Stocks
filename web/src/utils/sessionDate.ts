const WEEKDAYS = ['Sun', 'Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat']
const MONTHS = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec']

/**
 * A trading day as "Fri 23 Oct 2026". The weekday is there because a US close
 * lands on an Australian morning, so the newest session on screen is often
 * yesterday's. Read as a calendar date, never through the local time zone.
 */
export function formatSessionDate(date: string): string {
  const [year, month, day] = date.split('-').map(Number)
  const at = new Date(Date.UTC(year, month - 1, day))
  if (Number.isNaN(at.getTime())) return date
  return `${WEEKDAYS[at.getUTCDay()]} ${day} ${MONTHS[month - 1]} ${year}`
}

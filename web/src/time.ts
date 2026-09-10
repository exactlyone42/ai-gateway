export type RangeKey = 'today' | '7d' | '30d' | 'custom'
export interface ReportWindow { from: string; to: string }

export function reportWindow(key: RangeKey, now = new Date(), custom?: { from: string; to: string }): ReportWindow | null {
  if (key === 'custom' && custom) {
    const from = Date.parse(custom.from)
    const to = Date.parse(custom.to)
    if (!Number.isFinite(from) || !Number.isFinite(to) || from >= to) return null
    return { from: new Date(from).toISOString(), to: new Date(to).toISOString() }
  }
  const to = new Date(now)
  const from = new Date(now)
  if (key === 'today') from.setHours(0, 0, 0, 0)
  if (key === '7d') from.setDate(from.getDate() - 7)
  if (key === '30d') from.setDate(from.getDate() - 30)
  return { from: from.toISOString(), to: to.toISOString() }
}

export function formatDate(value?: string | null) {
  if (!value) return 'Unavailable'
  return new Intl.DateTimeFormat(undefined, {
    dateStyle: 'medium',
    timeStyle: 'short',
  }).format(new Date(value))
}

export function compactNumber(value: unknown) {
  return typeof value === 'number'
    ? new Intl.NumberFormat(undefined, { notation: 'compact', maximumFractionDigits: 1 }).format(value)
    : 'Unavailable'
}

export function formatCost(value: unknown) {
  return typeof value === 'number'
    ? new Intl.NumberFormat(undefined, { style: 'currency', currency: 'USD' }).format(value / 1_000_000)
    : 'Unavailable'
}

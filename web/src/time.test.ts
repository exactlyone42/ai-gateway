import { compactNumber, formatCost, reportWindow } from './time'
import { expect, test } from 'vitest'

test('report windows are bounded and explicit', () => {
  const now = new Date('2026-09-11T03:30:00.000Z')
  const today = reportWindow('today', now)
  expect(today?.to).toBe(now.toISOString())
  expect(new Date(today!.from).getHours()).toBe(0)
  const week = reportWindow('7d', now)!
  expect((Date.parse(week.to) - Date.parse(week.from)) / 86_400_000).toBe(7)
  const custom = reportWindow('custom', now, { from: '2026-09-01T00:00', to: '2026-09-02T00:00' })
  expect(Date.parse(custom!.to) - Date.parse(custom!.from)).toBe(86_400_000)
  expect(reportWindow('custom', now, { from: '', to: '' })).toBeNull()
  expect(compactNumber(null)).toBe('Unavailable')
  expect(formatCost(1_000_000)).toContain('1.00')
})

import { afterEach, expect, test, vi } from 'vitest'
import { adminRequest } from './api'

afterEach(() => vi.unstubAllGlobals())

test('a 401 clears the in-memory session before returning the error', async () => {
  const clear = vi.fn()
  vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(
    JSON.stringify({ error: { message: 'expired' } }),
    { status: 401, headers: { 'content-type': 'application/json' } },
  )))

  await expect(adminRequest('/overview', 'session-token', clear)).rejects.toEqual(
    expect.objectContaining({ status: 401 }),
  )
  expect(clear).toHaveBeenCalledOnce()
})

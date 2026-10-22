import type { Session } from './types'

export class ApiError extends Error {
  constructor(public status: number, message: string) {
    super(message)
  }
}

async function decode(response: Response) {
  if (response.status === 204) return null
  const body = await response.json().catch(() => null)
  if (!response.ok) {
    const message = body?.error?.message ?? body?.message ?? `Request failed (${response.status})`
    throw new ApiError(response.status, message)
  }
  return body
}

export async function createSession(loginToken: string): Promise<Session> {
  const response = await fetch('/admin/v1/sessions', {
    method: 'POST',
    headers: { Authorization: `Bearer ${loginToken}`, 'x-request-id': crypto.randomUUID() },
  })
  return decode(response)
}

export async function adminRequest<T>(
  path: string,
  token: string,
  onUnauthorized: () => void,
  init: RequestInit = {},
): Promise<T> {
  const response = await fetch(`/admin/v1${path}`, {
    ...init,
    headers: {
      Authorization: `Bearer ${token}`,
      'x-request-id': crypto.randomUUID(),
      ...(init.body ? { 'content-type': 'application/json' } : {}),
      ...init.headers,
    },
  })
  if (response.status === 401) onUnauthorized()
  return decode(response)
}

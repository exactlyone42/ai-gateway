export type Role = 'admin' | 'operator' | 'viewer'
export type JsonObject = Record<string, any>

export interface Session {
  access_token: string
  expires_at: string
  role: Role
}

export interface CursorPage {
  items: JsonObject[]
  next_cursor: string | null
}

export interface ResourcePage {
  items: JsonObject[]
  next_after: string | null
}

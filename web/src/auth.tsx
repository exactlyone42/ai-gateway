import { useQueryClient } from '@tanstack/react-query'
import { createContext, useCallback, useContext, useEffect, useMemo, useState, type PropsWithChildren } from 'react'
import { createSession } from './api'
import type { Role, Session } from './types'

interface AuthState {
  token: string | null
  role: Role | null
  expiresAt: string | null
  login: (loginToken: string) => Promise<void>
  clear: () => void
}

const AuthContext = createContext<AuthState | null>(null)

export function AuthProvider({ children }: PropsWithChildren) {
  const queryClient = useQueryClient()
  const [session, setSession] = useState<Session | null>(null)
  const clear = useCallback(() => {
    setSession(null)
    queryClient.clear()
  }, [queryClient])
  const login = useCallback(async (loginToken: string) => {
    const next = await createSession(loginToken)
    queryClient.clear()
    setSession(next)
  }, [queryClient])

  useEffect(() => {
    if (!session) return
    const delay = Math.max(0, new Date(session.expires_at).getTime() - Date.now())
    const timer = window.setTimeout(clear, delay)
    return () => window.clearTimeout(timer)
  }, [clear, session])

  const value = useMemo<AuthState>(() => ({
    token: session?.access_token ?? null,
    role: session?.role ?? null,
    expiresAt: session?.expires_at ?? null,
    login,
    clear,
  }), [clear, login, session])

  return <AuthContext.Provider value={value}>{children}</AuthContext.Provider>
}

export function useAuth() {
  const value = useContext(AuthContext)
  if (!value) throw new Error('AuthProvider is missing')
  return value
}

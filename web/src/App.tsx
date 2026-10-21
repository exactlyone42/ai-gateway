import { Activity, Boxes, FileClock, Fingerprint, Gauge, KeyRound, LogOut, Network, ScrollText, ShieldCheck, TimerReset, TriangleAlert } from 'lucide-react'
import { useState, type FormEvent } from 'react'
import { NavLink, Navigate, Route, Routes } from 'react-router-dom'
import { adminRequest } from './api'
import { useAuth } from './auth'
import { AuditPage, CatalogPage, CredentialsPage, GovernancePage, HealthPage, IdentitiesPage, IncidentsPage, OverviewPage, RequestsPage, UsagePage } from './pages'

const navigation = [
  ['/', 'Overview', Gauge], ['/identities', 'Identities', Network], ['/credentials', 'Credentials', KeyRound],
  ['/requests', 'Requests', ScrollText], ['/usage', 'Usage', Activity], ['/catalog', 'Models & Providers', Boxes],
  ['/governance', 'Policies & Scheduler', ShieldCheck], ['/incidents', 'Incidents', TriangleAlert], ['/health', 'System Health', TimerReset], ['/audit', 'Audit', FileClock],
] as const

function Login() {
  const auth = useAuth()
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  const submit = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault(); setBusy(true); setError(null)
    const token = String(new FormData(event.currentTarget).get('token') ?? '')
    try { await auth.login(token) } catch (cause) { setError(cause instanceof Error ? cause.message : 'Login failed') }
    finally { setBusy(false) }
  }
  return <main className="login"><form onSubmit={(event) => void submit(event)}><div className="brand-mark"><Fingerprint /></div><h1>Gateway Control</h1><p>Sign in with an Admin login credential.</p><label>Login credential<input name="token" type="password" autoComplete="off" required autoFocus /></label>{error && <div className="state error">{error}</div>}<button disabled={busy}>{busy ? 'Signing in' : 'Sign in'}</button></form></main>
}

function Shell() {
  const auth = useAuth()
  const logout = async () => {
    try { await adminRequest('/session', auth.token!, auth.clear, { method: 'DELETE' }) } finally { auth.clear() }
  }
  return <div className="shell"><aside className="sidebar"><header><div className="brand-mark"><Fingerprint /></div><div><strong>Gateway Control</strong><small>{auth.role}</small></div></header><nav>{navigation.map(([path, label, Icon]) => <NavLink key={path} to={path} end={path === '/'} title={label}><Icon /> <span>{label}</span></NavLink>)}</nav><footer><span>Session ends<br />{auth.expiresAt && new Date(auth.expiresAt).toLocaleTimeString()}</span><button className="icon-button" title="Sign out" onClick={() => void logout()}><LogOut /></button></footer></aside>
    <div className="content"><Routes><Route path="/" element={<OverviewPage />} /><Route path="/identities" element={<IdentitiesPage />} /><Route path="/credentials" element={<CredentialsPage />} /><Route path="/requests" element={<RequestsPage />} /><Route path="/usage" element={<UsagePage />} /><Route path="/catalog" element={<CatalogPage />} /><Route path="/governance" element={<GovernancePage />} /><Route path="/incidents" element={<IncidentsPage />} /><Route path="/health" element={<HealthPage />} /><Route path="/audit" element={<AuditPage />} /><Route path="*" element={<Navigate to="/" replace />} /></Routes></div>
  </div>
}

export default function App() {
  return useAuth().token ? <Shell /> : <Login />
}

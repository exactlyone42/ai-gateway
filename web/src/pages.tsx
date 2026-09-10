import { useQuery, useQueryClient } from '@tanstack/react-query'
import { Plus, RefreshCw, ShieldOff, X } from 'lucide-react'
import { useMemo, useState, type FormEvent } from 'react'
import { adminRequest } from './api'
import { useAuth } from './auth'
import { DataTable, ErrorState, Loading, Page, RangePicker, SecretDialog, Status, Unavailable } from './components'
import { compactNumber, formatCost, formatDate, reportWindow, type RangeKey } from './time'
import type { CursorPage, JsonObject } from './types'

function useAdminQuery<T>(key: unknown[], path: string, enabled = true) {
  const auth = useAuth()
  return useQuery<T>({
    queryKey: key,
    queryFn: () => adminRequest(path, auth.token!, auth.clear),
    enabled: Boolean(auth.token) && enabled,
  })
}

function useRangePath(endpoint: string, range: RangeKey, extra = '') {
  const window = useMemo(() => reportWindow(range), [range])
  if (!window) return endpoint
  return `${endpoint}?from=${encodeURIComponent(window.from)}&to=${encodeURIComponent(window.to)}${extra}`
}

function QueryBody<T>({ query, children }: { query: ReturnType<typeof useQuery<T>>; children: (data: T) => React.ReactNode }) {
  if (query.isLoading) return <Loading />
  if (query.error) return <ErrorState error={query.error} />
  return <>{children(query.data!)}</>
}

function principalEntities(data: JsonObject | undefined, dimension: string) {
  if (!data) return []
  const associated = (data.paths ?? []).map((path: JsonObject) => path[dimension]).filter(Boolean)
  const unassociated = data.unassociated?.[`${dimension}s`] ?? []
  return Array.from(new Map([...associated, ...unassociated].map((item: JsonObject) => [item.id, item])).values())
}

export function OverviewPage() {
  const [range, setRange] = useState<RangeKey>('today')
  const path = useRangePath('/overview', range)
  const query = useAdminQuery<JsonObject>(['overview', range], path)
  const metrics = [
    ['Requests', 'requests'], ['Success rate', 'success_rate'], ['Unsuccessful', 'unsuccessful_requests'],
    ['Input tokens', 'input_tokens'], ['Output tokens', 'output_tokens'], ['Total tokens', 'total_tokens'],
    ['Cached input', 'cached_input_tokens'], ['P95 latency', 'p95_latency_ms'], ['P95 TTFT', 'p95_ttft_ms'],
    ['Cost', 'cost_microusd'],
  ]
  return <Page title="Overview" actions={<RangePicker value={range} onChange={setRange} />}>
    <QueryBody query={query}>{(data) => <>
      <section className="metric-grid">{metrics.map(([label, key]) => <article className="metric" key={key}>
        <span>{label}</span><strong>{key === 'success_rate' && typeof data[key] === 'number' ? `${(data[key] * 100).toFixed(1)}%` : key === 'cost_microusd' ? formatCost(data[key]) : compactNumber(data[key])}</strong>
        {key === 'cost_microusd' && data[key] == null && <small>{data.cost_unavailable_reason}</small>}
      </article>)}</section>
      {!data.queue?.available && <Unavailable reason={data.queue?.unavailable_reason} />}
    </>}</QueryBody>
  </Page>
}

export function IdentitiesPage() {
  const auth = useAuth()
  const queryClient = useQueryClient()
  const query = useAdminQuery<JsonObject>(['identity-map'], '/identity-map')
  const dimensions = ['user', 'device', 'client', 'agent']
  const [creating, setCreating] = useState(false)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const canWrite = auth.role !== 'viewer'
  const create = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault(); setBusy(true); setError(null)
    const values = new FormData(event.currentTarget)
    try {
      await adminRequest(`/${values.get('kind')}`, auth.token!, auth.clear, { method: 'POST', body: JSON.stringify({ display_name: values.get('name') }) })
      setCreating(false)
      await queryClient.invalidateQueries({ queryKey: ['identity-map'] })
    } catch (cause) { setError(cause instanceof Error ? cause.message : 'Request failed') }
    finally { setBusy(false) }
  }
  return <Page title="Identities" actions={canWrite ? <button onClick={() => setCreating(true)}><Plus size={16} />Create identity</button> : undefined}>{error && <div className="state error">{error}</div>}<QueryBody query={query}>{(data) => <>
    <DataTable rows={data.paths ?? []} columns={[
      { key: 'user', label: 'User', render: (row) => row.user?.display_name ?? 'Unassigned' },
      { key: 'device', label: 'Device', render: (row) => row.device?.display_name ?? 'Unassigned' },
      { key: 'client', label: 'Client', render: (row) => row.client?.display_name ?? 'Unassigned' },
      { key: 'agent', label: 'Agent', render: (row) => row.agent?.display_name ?? 'Unassigned' },
      { key: 'credential', label: 'Credential', render: (row) => <><strong>{row.credential.display_name}</strong><small className="subline">{row.credential.key_prefix} · {row.credential.id}</small></> },
    ]} />
    <section className="section"><h2>Unassociated entities</h2><div className="entity-columns">{dimensions.map((dimension) => <div key={dimension}><h3>{dimension}</h3>{(data.unassociated?.[`${dimension}s`] ?? []).map((item: JsonObject) => <p key={item.id}>{item.display_name}</p>)}</div>)}</div></section>
  </>}</QueryBody>{creating && <div className="dialog-backdrop"><form className="dialog" onSubmit={(event) => void create(event)}><header><h2>Create identity</h2><button type="button" className="icon-button" title="Close" onClick={() => setCreating(false)}><X /></button></header><label>Dimension<select name="kind"><option value="users">User</option><option value="devices">Device</option><option value="clients">Client</option><option value="agents">Agent</option></select></label><label>Name<input name="name" required maxLength={120} /></label><footer><button type="button" className="secondary" onClick={() => setCreating(false)}>Cancel</button><button disabled={busy}>{busy ? 'Creating' : 'Create'}</button></footer></form></div>}</Page>
}

export function CredentialsPage() {
  const auth = useAuth()
  const queryClient = useQueryClient()
  const query = useAdminQuery<JsonObject>(['identity-map'], '/identity-map')
  const [creating, setCreating] = useState(false)
  const [busy, setBusy] = useState(false)
  const [secret, setSecret] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  const canWrite = auth.role !== 'viewer'
  const users = principalEntities(query.data, 'user')

  const mutate = async (path: string, init: RequestInit) => {
    setBusy(true); setError(null)
    try {
      const result = await adminRequest<JsonObject>(path, auth.token!, auth.clear, init)
      if (result.secret) setSecret(result.secret)
      setCreating(false)
      await queryClient.invalidateQueries({ queryKey: ['identity-map'] })
    } catch (cause) { setError(cause instanceof Error ? cause.message : 'Request failed') }
    finally { setBusy(false) }
  }

  const create = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault()
    const data = new FormData(event.currentTarget)
    await mutate('/credentials', { method: 'POST', body: JSON.stringify({ display_name: data.get('name'), user_id: data.get('user_id'), device_id: null, client_id: null, agent_id: null, expires_at: null }) })
  }

  return <Page title="Credentials" actions={canWrite ? <button onClick={() => setCreating(true)}><Plus size={16} />Create</button> : undefined}>
    {error && <div className="state error">{error}</div>}
    <QueryBody query={query}>{(data) => <DataTable rows={(data.paths ?? []).map((path: JsonObject) => path.credential)} columns={[
      { key: 'display_name', label: 'Name' }, { key: 'key_prefix', label: 'Key prefix' }, { key: 'id', label: 'Credential ID' },
      { key: 'last_used_at', label: 'Last used', render: (row) => formatDate(row.last_used_at) },
      { key: 'status', label: 'Status', render: (row) => <Status value={row.status} /> },
      ...(canWrite ? [{ key: 'actions', label: 'Actions', render: (row: JsonObject) => <div className="row-actions">
        <button className="icon-button" title="Rotate credential" disabled={busy} onClick={(event) => { event.stopPropagation(); void mutate(`/credentials/${row.id}/rotate`, { method: 'POST', body: JSON.stringify({ version: row.version, overlap_seconds: 60 }) }) }}><RefreshCw /></button>
        <button className="icon-button danger" title="Revoke credential" disabled={busy} onClick={(event) => { event.stopPropagation(); void mutate(`/credentials/${row.id}?version=${row.version}`, { method: 'DELETE' }) }}><ShieldOff /></button>
      </div> }] : []),
    ]} />}</QueryBody>
    {creating && <div className="dialog-backdrop"><form className="dialog" onSubmit={(event) => void create(event)}><header><h2>Create credential</h2><button type="button" className="icon-button" title="Close" onClick={() => setCreating(false)}><X /></button></header>
      <label>Name<input name="name" required maxLength={120} /></label><label>User<select name="user_id" required><option value="">Select a user</option>{users.map((user: JsonObject) => <option key={user.id} value={user.id}>{user.display_name}</option>)}</select></label>
      <footer><button type="button" className="secondary" onClick={() => setCreating(false)}>Cancel</button><button disabled={busy}>{busy ? 'Creating' : 'Create'}</button></footer>
    </form></div>}
    {secret && <SecretDialog secret={secret} onClose={() => setSecret(null)} />}
  </Page>
}

export function RequestsPage() {
  const [range, setRange] = useState<RangeKey>('today')
  const [cursor, setCursor] = useState<string | null>(null)
  const [selected, setSelected] = useState<string | null>(null)
  const path = useRangePath('/requests', range, `&limit=50${cursor ? `&cursor=${encodeURIComponent(cursor)}` : ''}`)
  const query = useAdminQuery<CursorPage>(['requests', range, cursor], path)
  const detail = useAdminQuery<JsonObject>(['request', selected], `/requests/${selected}`, selected !== null)
  return <Page title="Requests" actions={<RangePicker value={range} onChange={(value) => { setRange(value); setCursor(null) }} />}>
    <QueryBody query={query}>{(data) => <><DataTable rows={data.items} onRow={(row) => setSelected(row.id)} columns={[
      { key: 'created_at', label: 'Time', render: (row) => formatDate(row.created_at) }, { key: 'request_id', label: 'Request' },
      { key: 'principal', label: 'Principal', render: (row) => row.principal?.user ?? row.principal?.credential ?? 'Unassigned' },
      { key: 'selected_model', label: 'Model' }, { key: 'selected_provider', label: 'Provider' },
      { key: 'status', label: 'Status', render: (row) => <Status value={row.status} /> },
      { key: 'total_latency_ms', label: 'Latency', render: (row) => row.total_latency_ms == null ? 'Unavailable' : `${Math.round(row.total_latency_ms)} ms` },
    ]} />{data.next_cursor && <button className="load-more" onClick={() => setCursor(data.next_cursor)}>Next page</button>}</>}</QueryBody>
    {selected && <div className="drawer-backdrop" onClick={() => setSelected(null)}><aside className="drawer" onClick={(event) => event.stopPropagation()}><header><h2>Request detail</h2><button className="icon-button" title="Close" onClick={() => setSelected(null)}><X /></button></header>
      <QueryBody query={detail}>{(data) => <><dl className="detail-list"><dt>Request ID</dt><dd>{data.request.request_id}</dd><dt>Trace ID</dt><dd>{data.request.trace_id ?? 'Unavailable'}</dd><dt>Status</dt><dd><Status value={data.request.status} /></dd><dt>Operation</dt><dd>{data.request.operation}</dd></dl>
        <h3>Provider attempts</h3><DataTable rows={data.attempts} columns={[{ key: 'attempt_number', label: '#' }, { key: 'provider', label: 'Provider' }, { key: 'model', label: 'Model' }, { key: 'status', label: 'Status', render: (row) => <Status value={row.status} /> }]} />
        <Unavailable reason={data.retry_fallback_history?.unavailable_reason} /><Unavailable reason={data.trace_spans?.unavailable_reason} /></>}</QueryBody>
    </aside></div>}
  </Page>
}

export function UsagePage() {
  const [range, setRange] = useState<RangeKey>('7d')
  const [dimension, setDimension] = useState('user')
  const [customFrom, setCustomFrom] = useState(() => new Date(Date.now() - 86_400_000).toISOString().slice(0, 16))
  const [customTo, setCustomTo] = useState(() => new Date().toISOString().slice(0, 16))
  const window = useMemo(() => reportWindow(range, new Date(), { from: customFrom, to: customTo }), [range, customFrom, customTo])
  const path = window ? `/usage?from=${encodeURIComponent(window.from)}&to=${encodeURIComponent(window.to)}&dimension=${dimension}` : '/usage'
  const query = useAdminQuery<JsonObject>(['usage', range, dimension, customFrom, customTo], path, window !== null)
  return <Page title="Usage" actions={<div className="toolbar"><select aria-label="Dimension" value={dimension} onChange={(event) => setDimension(event.target.value)}>{['user', 'device', 'client', 'agent', 'credential', 'model', 'provider', 'workload'].map((value) => <option key={value}>{value}</option>)}</select><RangePicker value={range} onChange={setRange} allowCustom /></div>}>
    {range === 'custom' && <div className="custom-range"><label>From<input type="datetime-local" value={customFrom} onChange={(event) => setCustomFrom(event.target.value)} /></label><label>To<input type="datetime-local" value={customTo} onChange={(event) => setCustomTo(event.target.value)} /></label></div>}
    {!window && <div className="state error">Enter a valid start and end time.</div>}
    <QueryBody query={query}>{(data) => <><DataTable rows={data.groups ?? []} columns={[
      { key: 'dimension_name', label: 'Group' }, { key: 'input_tokens', label: 'Input', render: (row) => compactNumber(row.input_tokens) },
      { key: 'output_tokens', label: 'Output', render: (row) => compactNumber(row.output_tokens) }, { key: 'attempts', label: 'Attempts' },
      { key: 'unreconciled_attempts', label: 'Estimated' }, { key: 'cost_microusd', label: 'Cost', render: (row) => row.cost_microusd == null ? 'Unavailable' : `$${(row.cost_microusd / 1_000_000).toFixed(4)}` },
    ]} />{!data.cost_available && <Unavailable reason={data.cost_unavailable_reason} />}{!data.team?.available && <Unavailable reason={data.team?.unavailable_reason} />}</>}</QueryBody>
  </Page>
}

export function CatalogPage() {
  const query = useAdminQuery<JsonObject>(['catalog'], '/catalog')
  return <Page title="Models & Providers"><QueryBody query={query}>{(data) => <>
    {!data.activation?.available && <Unavailable reason={data.activation?.unavailable_reason} />}
    <section className="section"><h2>Models</h2><DataTable rows={data.models ?? []} columns={[{ key: 'public_model_id', label: 'Public model' }, { key: 'provider_key', label: 'Provider' }, { key: 'quality_tier', label: 'Tier' }, { key: 'context_window', label: 'Context' }, { key: 'lifecycle_state', label: 'State', render: (row) => <Status value={row.lifecycle_state} /> }]} /></section>
    <section className="section"><h2>Providers</h2><DataTable rows={data.providers ?? []} columns={[{ key: 'display_name', label: 'Provider' }, { key: 'provider_type', label: 'Type' }, { key: 'base_url', label: 'Base URL' }, { key: 'credential_configured', label: 'Credential', render: (row) => row.credential_configured ? 'Configured' : 'Missing' }, { key: 'lifecycle_state', label: 'State', render: (row) => <Status value={row.lifecycle_state} /> }]} /></section>
  </>}</QueryBody></Page>
}

export function GovernancePage() {
  const query = useAdminQuery<JsonObject>(['governance'], '/governance')
  return <Page title="Policies & Scheduler"><QueryBody query={query}>{(data) => <>
    <div className="availability-grid"><Unavailable reason={data.policy_runtime?.unavailable_reason} /><Unavailable reason={data.quota_runtime?.unavailable_reason} /><Unavailable reason={data.scheduler?.unavailable_reason} /></div>
    <section className="section"><h2>Policy drafts</h2><DataTable rows={data.policies ?? []} columns={[{ key: 'policy_key', label: 'Policy' }, { key: 'revision', label: 'Revision' }, { key: 'lifecycle_state', label: 'State', render: (row) => <Status value={row.lifecycle_state} /> }, { key: 'updated_at', label: 'Updated', render: (row) => formatDate(row.updated_at) }]} /></section>
    <section className="section"><h2>Quota drafts</h2><DataTable rows={data.quotas ?? []} columns={[{ key: 'quota_key', label: 'Quota' }, { key: 'revision', label: 'Revision' }, { key: 'lifecycle_state', label: 'State', render: (row) => <Status value={row.lifecycle_state} /> }, { key: 'updated_at', label: 'Updated', render: (row) => formatDate(row.updated_at) }]} /></section>
  </>}</QueryBody></Page>
}

export function HealthPage() {
  const query = useAdminQuery<JsonObject>(['system-health'], '/system-health')
  return <Page title="System Health"><QueryBody query={query}>{(data) => <><div className="health-summary"><div><span>Overall</span><Status value={data.status} /></div><small>Checked {formatDate(data.checked_at)}</small></div><div className="health-list">{Object.entries(data.components ?? {}).map(([name, component]: [string, any]) => <div key={name}><strong>{name.replaceAll('_', ' ')}</strong><Status value={component.status} />{component.reason && <small>{component.reason}</small>}</div>)}</div></>}</QueryBody></Page>
}

export function AuditPage() {
  const [range, setRange] = useState<RangeKey>('7d')
  const [cursor, setCursor] = useState<string | null>(null)
  const path = useRangePath('/audit', range, `&limit=50${cursor ? `&cursor=${encodeURIComponent(cursor)}` : ''}`)
  const query = useAdminQuery<CursorPage>(['audit', range, cursor], path)
  return <Page title="Audit" actions={<RangePicker value={range} onChange={(value) => { setRange(value); setCursor(null) }} />}><QueryBody query={query}>{(data) => <><DataTable rows={data.items} columns={[
    { key: 'occurred_at', label: 'Time', render: (row) => formatDate(row.occurred_at) }, { key: 'actor_name', label: 'Actor' },
    { key: 'actor_role', label: 'Role' }, { key: 'action', label: 'Action' }, { key: 'target_type', label: 'Target' },
    { key: 'request_id', label: 'Request ID' }, { key: 'source_ip', label: 'Source' },
  ]} />{data.next_cursor && <button className="load-more" onClick={() => setCursor(data.next_cursor)}>Next page</button>}</>}</QueryBody></Page>
}

import { useQuery, useQueryClient } from '@tanstack/react-query'
import { CirclePower, Pencil, Plus, PowerOff, RefreshCw, ShieldOff, Trash2, X } from 'lucide-react'
import { useMemo, useState, type FormEvent } from 'react'
import { adminRequest } from './api'
import { useAuth } from './auth'
import { DataTable, ErrorState, Loading, Page, RangePicker, SecretDialog, Status, Unavailable } from './components'
import { compactNumber, formatCost, formatDate, reportWindow, type RangeKey } from './time'
import type { CursorPage, JsonObject, ResourcePage } from './types'

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
    ['Cache read', 'cached_input_tokens'], ['Cache write', 'cache_write_input_tokens'], ['P95 Gateway processing', 'p95_gateway_overhead_ms'],
    ['Cost', 'cost_microusd'],
  ]
  return <Page title="Overview" actions={<RangePicker value={range} onChange={setRange} />}>
    <QueryBody query={query}>{(data) => <>
      <section className="metric-grid">{metrics.map(([label, key]) => <article className="metric" key={key}>
        <span>{label}</span><strong>{key === 'success_rate' && typeof data[key] === 'number' ? `${(data[key] * 100).toFixed(1)}%` : key === 'cost_microusd' ? formatCost(data[key]) : compactNumber(data[key])}</strong>
        {key === 'cost_microusd' && data[key] == null && <small>{data.cost_unavailable_reason}</small>}
      </article>)}</section>
      {!data.queue?.available && <Unavailable reason={data.queue?.unavailable_reason} />}
      {data.queue?.available && <section className="section"><h2>Scheduler</h2><div className="metric-grid">
        {[['Active', data.queue.runtime?.active_tenant], ['Queued', data.queue.runtime?.queued], ['Queue P50', data.queue.p50_ms], ['Queue P95', data.queue.p95_ms], ['Rejected', data.queue.rejected], ['Expired', data.queue.expired]].map(([label, value]) => <article className="metric" key={String(label)}><span>{label}</span><strong>{compactNumber(value)}</strong></article>)}
      </div></section>}
    </>}</QueryBody>
  </Page>
}

export function IdentitiesPage() {
  const auth = useAuth()
  const queryClient = useQueryClient()
  const query = useAdminQuery<JsonObject>(['identity-map'], '/identity-map')
  const dimensions = ['user', 'device', 'client', 'agent']
  const [creating, setCreating] = useState(false)
  const [kind, setKind] = useState('users')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const canWrite = auth.role !== 'viewer'
  const create = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault(); setBusy(true); setError(null)
    const values = new FormData(event.currentTarget)
    try {
      const body = { display_name: values.get('name'), ...(kind === 'users' && values.get('email') ? { email: values.get('email') } : {}) }
      await adminRequest(`/${kind}`, auth.token!, auth.clear, { method: 'POST', body: JSON.stringify(body) })
      setCreating(false)
      await queryClient.invalidateQueries({ queryKey: ['identity-map'] })
      await queryClient.invalidateQueries({ queryKey: ['users'] })
    } catch (cause) { setError(cause instanceof Error ? cause.message : 'Request failed') }
    finally { setBusy(false) }
  }
  return <Page title="Identities" actions={canWrite ? <button onClick={() => setCreating(true)}><Plus size={16} />Create identity</button> : undefined}>{error && <div className="state error">{error}</div>}<UserEmails canWrite={canWrite} /><Groups canWrite={canWrite} canManageMembers={auth.role === 'admin'} /><QueryBody query={query}>{(data) => <>
    <DataTable rows={data.paths ?? []} columns={[
      { key: 'user', label: 'User', render: (row) => row.user?.display_name ?? 'Unassigned' },
      { key: 'device', label: 'Device', render: (row) => row.device?.display_name ?? 'Unassigned' },
      { key: 'client', label: 'Client', render: (row) => row.client?.display_name ?? 'Unassigned' },
      { key: 'agent', label: 'Agent', render: (row) => row.agent?.display_name ?? 'Unassigned' },
      { key: 'credential', label: 'Credential', render: (row) => <><strong>{row.credential.display_name}</strong><small className="subline">{row.credential.key_prefix} · {row.credential.id}</small></> },
    ]} />
    <section className="section"><h2>Unassociated entities</h2><div className="entity-columns">{dimensions.map((dimension) => <div key={dimension}><h3>{dimension}</h3>{(data.unassociated?.[`${dimension}s`] ?? []).map((item: JsonObject) => <p key={item.id}>{item.display_name}</p>)}</div>)}</div></section>
  </>}</QueryBody>{creating && <div className="dialog-backdrop"><form className="dialog" onSubmit={(event) => void create(event)}><header><h2>Create identity</h2><button type="button" className="icon-button" title="Close" onClick={() => setCreating(false)}><X /></button></header><label>Dimension<select name="kind" value={kind} onChange={(event) => setKind(event.target.value)}><option value="users">User</option><option value="devices">Device</option><option value="clients">Client</option><option value="agents">Agent</option></select></label><label>Name<input name="name" required maxLength={120} /></label>{kind === 'users' && <label>Email<input name="email" type="email" maxLength={320} /></label>}<footer><button type="button" className="secondary" onClick={() => setCreating(false)}>Cancel</button><button disabled={busy}>{busy ? 'Creating' : 'Create'}</button></footer></form></div>}</Page>
}

function Groups({ canWrite, canManageMembers }: { canWrite: boolean; canManageMembers: boolean }) {
  const auth = useAuth()
  const client = useQueryClient()
  const [group, setGroup] = useState('')
  const [groupAfter, setGroupAfter] = useState('')
  const [userAfter, setUserAfter] = useState('')
  const [memberAfter, setMemberAfter] = useState('')
  const [error, setError] = useState<string | null>(null)
  const groups = useAdminQuery<ResourcePage>(['groups', groupAfter], `/groups?limit=100${groupAfter ? `&after=${groupAfter}` : ''}`)
  const users = useAdminQuery<ResourcePage>(['users', 'group-members', userAfter], `/users?limit=100${userAfter ? `&after=${userAfter}` : ''}`)
  const members = useAdminQuery<ResourcePage>(['group-members', group, memberAfter], `/groups/${group}/members?limit=100${memberAfter ? `&after=${memberAfter}` : ''}`, Boolean(group))
  async function change(path: string, method: string, body?: JsonObject) {
    setError(null)
    try {
      await adminRequest(path, auth.token!, auth.clear, { method, ...(body ? { body: JSON.stringify(body) } : {}) })
      await client.invalidateQueries({ queryKey: ['groups'] })
      await client.invalidateQueries({ queryKey: ['group-members'] })
    } catch (cause) { setError(cause instanceof Error ? cause.message : '操作失敗') }
  }
  return <section className="section"><h2>群組與成員</h2>{error && <div className="state error">{error}</div>}
    {canWrite && <form onSubmit={(event) => { event.preventDefault(); const form = event.currentTarget; const name = String(new FormData(form).get('name') ?? '').trim(); if (name) void change('/groups', 'POST', { display_name: name }) }}><label>群組名稱<input name="name" required maxLength={120} /></label><button>建立群組</button></form>}
    <QueryBody query={groups}>{(data) => <><label>選擇群組<select value={group} onChange={(event) => { setGroup(event.target.value); setMemberAfter('') }}><option value="">請選擇</option>{data.items.map((item) => <option key={item.id} value={item.id}>{item.display_name}</option>)}</select></label>{groupAfter && <button onClick={() => { setGroupAfter(''); setGroup('') }}>回第一頁</button>}{data.next_after && <button onClick={() => { setGroupAfter(data.next_after!); setGroup('') }}>下一頁群組</button>}</>}</QueryBody>
    {group && <QueryBody query={members}>{(data) => <><DataTable rows={data.items} columns={[{ key: 'user_id', label: '使用者', render: (row) => users.data?.items.find((user) => user.id === row.user_id)?.display_name ?? row.user_id }, ...(canManageMembers ? [{ key: 'actions', label: '操作', render: (row: JsonObject) => <button onClick={() => void change(`/groups/${group}/members/${row.user_id}`, 'DELETE')}>移除</button> }] : [])]} />{memberAfter && <button onClick={() => setMemberAfter('')}>回第一頁成員</button>}{data.next_after && <button onClick={() => setMemberAfter(data.next_after!)}>下一頁成員</button>}{canManageMembers && <form onSubmit={(event) => { event.preventDefault(); const user = String(new FormData(event.currentTarget).get('user') ?? ''); if (user) void change(`/groups/${group}/members/${user}`, 'POST') }}><label>加入使用者<select name="user" required><option value="">請選擇</option>{(users.data?.items ?? []).map((user) => <option key={user.id} value={user.id}>{user.display_name}</option>)}</select></label><button>加入</button>{userAfter && <button type="button" onClick={() => setUserAfter('')}>回第一頁使用者</button>}{users.data?.next_after && <button type="button" onClick={() => setUserAfter(users.data!.next_after!)}>下一頁使用者</button>}</form>}</>}</QueryBody>}
  </section>
}

function UserEmails({ canWrite }: { canWrite: boolean }) {
  const auth = useAuth()
  const queryClient = useQueryClient()
  const [search, setSearch] = useState('')
  const [email, setEmail] = useState('')
  const [editing, setEditing] = useState<JsonObject | null>(null)
  const [error, setError] = useState<string | null>(null)
  const query = useAdminQuery<ResourcePage>(['users', search], `/users?limit=100${search ? `&email=${encodeURIComponent(search)}` : ''}`)
  async function save(event: FormEvent<HTMLFormElement>) {
    event.preventDefault()
    if (!editing) return
    setError(null)
    try {
      await adminRequest(`/users/${editing.id}`, auth.token!, auth.clear, { method: 'PATCH', body: JSON.stringify({ version: editing.version, email: email || null }) })
      setEditing(null)
      await queryClient.invalidateQueries({ queryKey: ['users'] })
      await queryClient.invalidateQueries({ queryKey: ['identity-map'] })
    } catch (cause) { setError(cause instanceof Error ? cause.message : 'Request failed') }
  }
  return <section className="section"><h2>使用者 email</h2><form onSubmit={(event) => { event.preventDefault(); setSearch(String(new FormData(event.currentTarget).get('search') ?? '').trim()) }}><label>精確搜尋 email<input name="search" type="email" /></label><button>搜尋</button></form>{error && <div className="state error">{error}</div>}<QueryBody query={query}>{(data) => <><DataTable rows={data.items} columns={[{ key: 'display_name', label: '姓名' }, { key: 'email', label: 'Email', render: (row) => row.email ?? '未填寫' }, ...(canWrite ? [{ key: 'actions', label: '操作', render: (row: JsonObject) => <button onClick={() => { setEditing(row); setEmail(row.email ?? '') }}>編輯 email</button> }] : [])]} />{data.next_after && <p>只顯示前 100 筆，請使用 email 搜尋。</p>}</>}</QueryBody>{editing && <div className="dialog-backdrop"><form className="dialog" onSubmit={(event) => void save(event)}><header><h2>編輯 email</h2><button type="button" className="icon-button" title="關閉" onClick={() => setEditing(null)}><X /></button></header><label>Email<input value={email} onChange={(event) => setEmail(event.target.value)} type="email" maxLength={320} /></label><p>留空會清除 email。</p><footer><button type="button" className="secondary" onClick={() => setEditing(null)}>取消</button><button>儲存</button></footer></form></div>}</section>
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
        {(row.status === 'active' || row.status === 'disabled') && <button disabled={busy} onClick={(event) => { event.stopPropagation(); void mutate(`/credentials/${row.id}`, { method: 'PATCH', body: JSON.stringify({ version: row.version, status: row.status === 'active' ? 'disabled' : 'active' }) }) }}>{row.status === 'active' ? '停用' : '啟用'}</button>}
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
        <h3>Task routing</h3>{data.request.policy_decision?.routing?.task_classification
          ? <dl className="detail-list">{(() => { const routing = data.request.policy_decision.routing.task_classification; return <><dt>Original model</dt><dd>{routing.original_model}</dd><dt>Task</dt><dd>{routing.classification?.task ?? 'No classification'}</dd><dt>Reason</dt><dd>{routing.classification?.reason ?? 'Fallback to original model'}</dd><dt>Jev opinion</dt><dd>{routing.jev_opinion ?? 'No opinion'}</dd><dt>Final model</dt><dd>{routing.final_model}</dd><dt>Selection reason</dt><dd>{routing.selection_reason}</dd></> })()}</dl>
          : <Unavailable reason="No task-routing decision was recorded for this request." />}
        <h3>Request parameters</h3><dl className="detail-list"><dt>Received</dt><dd><code>{JSON.stringify(data.parameters?.requested ?? {})}</code></dd><dt>Sent to provider</dt><dd><code>{JSON.stringify(data.parameters?.provider ?? {})}</code></dd></dl>
        <h3>Registry selection</h3><dl className="detail-list"><dt>Selector</dt><dd>{data.request.selector_kind ? `${data.request.selector_kind}:${data.request.selector_value}` : 'Unavailable'}</dd><dt>Alias</dt><dd>{data.request.selected_alias_id ? `${data.request.selected_alias_id} v${data.request.selected_alias_version}` : 'Direct model'}</dd><dt>Model</dt><dd>{data.request.selected_model_id ? `${data.request.selected_model_id} v${data.request.selected_model_version}` : 'Unavailable'}</dd><dt>Provider</dt><dd>{data.request.selected_provider_id ? `${data.request.selected_provider_id} v${data.request.selected_provider_version}` : 'Unavailable'}</dd><dt>Provider model</dt><dd>{data.request.selected_provider_model_id ?? 'Unavailable'}</dd><dt>Cost metadata</dt><dd><code>{JSON.stringify(data.request.selected_cost_metadata ?? {})}</code></dd></dl>
        <h3>Effective policy</h3>{data.request.policy_decision
          ? <pre className="decision-json">{JSON.stringify(data.request.policy_decision, null, 2)}</pre>
          : <Unavailable reason="No policy snapshot was recorded for this request." />}
        <h3>Quota decision</h3>{data.request.quota_decision
          ? <pre className="decision-json">{JSON.stringify(data.request.quota_decision, null, 2)}</pre>
          : <Unavailable reason="No quota applied to this request." />}
        <h3>Provider attempts</h3><DataTable rows={data.attempts} columns={[{ key: 'attempt_number', label: '#' }, { key: 'provider', label: 'Provider' }, { key: 'model', label: 'Model' }, { key: 'status', label: 'Status', render: (row) => <Status value={row.status} /> }]} />
        <h3>Routing history</h3><DataTable rows={data.retry_fallback_history?.events ?? []} columns={[{ key: 'attempt_number', label: '#' }, { key: 'attempt_kind', label: 'Kind' }, { key: 'routing_reason', label: 'Reason' }, { key: 'provider', label: 'Provider' }, { key: 'status', label: 'Status', render: (row) => <Status value={row.status} /> }]} />
        <Unavailable reason={data.trace_spans?.unavailable_reason} /></>}</QueryBody>
    </aside></div>}
  </Page>
}

export function IncidentsPage() {
  const auth = useAuth()
  const queryClient = useQueryClient()
  const [filters, setFilters] = useState({ status: '', service: '', impact_scope: '', from: '', until: '' })
  const [selected, setSelected] = useState<string | null>(null)
  const [creating, setCreating] = useState(false)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const params = new URLSearchParams(Object.entries(filters).filter(([, value]) => value).map(([key, value]) => [key, ['from', 'until'].includes(key) ? new Date(value).toISOString() : value]))
  const query = useAdminQuery<JsonObject>(['incidents', filters], `/incidents${params.size ? `?${params}` : ''}`)
  const detail = useAdminQuery<JsonObject>(['incident', selected], `/incidents/${selected}`, selected !== null)
  const canWrite = auth.role !== 'viewer'
  const refresh = async () => {
    await queryClient.invalidateQueries({ queryKey: ['incidents'] })
    if (selected) await queryClient.invalidateQueries({ queryKey: ['incident', selected] })
  }
  const mutate = async (path: string, init: RequestInit) => {
    setBusy(true); setError(null)
    try { await adminRequest(path, auth.token!, auth.clear, init); await refresh() }
    catch (cause) { setError(cause instanceof Error ? cause.message : 'Request failed') }
    finally { setBusy(false) }
  }
  const create = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault()
    const data = new FormData(event.currentTarget)
    await mutate('/incidents', { method: 'POST', body: JSON.stringify({
      title: data.get('title'), status: 'investigating', service: data.get('service'), impact_scope: data.get('impact_scope'),
      started_at: new Date(String(data.get('started_at'))).toISOString(), ended_at: null,
      summary: data.get('summary'), impact: data.get('impact'), root_cause: null, temporary_fix: null,
      permanent_fix: null, prevention: null, validation_evidence: null,
    }) })
    setCreating(false)
  }
  const changeStatus = async (incident: JsonObject, status: string) => {
    await mutate(`/incidents/${incident.id}`, { method: 'PATCH', body: JSON.stringify({ version: incident.version, status }) })
  }
  const appendEvent = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault()
    const data = new FormData(event.currentTarget)
    await mutate(`/incidents/${selected}/events`, { method: 'POST', body: JSON.stringify({
      event_type: data.get('event_type'), occurred_at: new Date().toISOString(), message: data.get('message') || null,
      gateway_request_id: data.get('gateway_request_id') || null, deployment_version: data.get('deployment_version') || null,
    }) })
    event.currentTarget.reset()
  }
  const setFilter = (key: string, value: string) => setFilters((current) => ({ ...current, [key]: value }))
  return <Page title="Incidents" actions={canWrite ? <button onClick={() => setCreating(true)}><Plus size={16} />Create incident</button> : undefined}>
    {error && <div className="state error">{error}</div>}
    <div className="toolbar"><select aria-label="Incident status" value={filters.status} onChange={(event) => setFilter('status', event.target.value)}><option value="">All statuses</option>{['investigating', 'mitigated', 'resolved', 'closed'].map((value) => <option key={value}>{value}</option>)}</select><input aria-label="Service" placeholder="Service" value={filters.service} onChange={(event) => setFilter('service', event.target.value)} /><select aria-label="Impact scope" value={filters.impact_scope} onChange={(event) => setFilter('impact_scope', event.target.value)}><option value="">All scopes</option>{['system', 'tenant', 'principal'].map((value) => <option key={value}>{value}</option>)}</select><input aria-label="From" type="datetime-local" value={filters.from} onChange={(event) => setFilter('from', event.target.value)} /><input aria-label="Until" type="datetime-local" value={filters.until} onChange={(event) => setFilter('until', event.target.value)} /></div>
    <QueryBody query={query}>{(data) => <DataTable rows={data.items ?? []} onRow={(row) => setSelected(row.id)} columns={[{ key: 'started_at', label: 'Started', render: (row) => formatDate(row.started_at) }, { key: 'title', label: 'Incident' }, { key: 'service', label: 'Service' }, { key: 'impact_scope', label: 'Impact' }, { key: 'status', label: 'Status', render: (row) => <Status value={row.status} /> }]} />}</QueryBody>
    {creating && <div className="dialog-backdrop"><form className="dialog" onSubmit={(event) => void create(event)}><header><h2>Create incident</h2><button type="button" className="icon-button" title="Close" onClick={() => setCreating(false)}><X /></button></header><label>Title<input name="title" required maxLength={512} /></label><label>Service<input name="service" required maxLength={128} /></label><label>Impact scope<select name="impact_scope"><option value="system">System</option><option value="tenant">Tenant</option><option value="principal">Principal</option></select></label><label>Started<input name="started_at" type="datetime-local" required /></label><label>Summary<textarea name="summary" required maxLength={8192} /></label><label>Impact<textarea name="impact" required maxLength={8192} /></label><footer><button type="button" className="secondary" onClick={() => setCreating(false)}>Cancel</button><button disabled={busy}>{busy ? 'Creating' : 'Create'}</button></footer></form></div>}
    {selected && <div className="drawer-backdrop" onClick={() => setSelected(null)}><aside className="drawer" onClick={(event) => event.stopPropagation()}><header><h2>Incident detail</h2><button className="icon-button" title="Close" onClick={() => setSelected(null)}><X /></button></header><QueryBody query={detail}>{(data) => <><h3>{data.incident.title}</h3><dl className="detail-list"><dt>Status</dt><dd>{canWrite ? <select aria-label="Update status" value={data.incident.status} disabled={busy} onChange={(event) => void changeStatus(data.incident, event.target.value)}>{['investigating', 'mitigated', 'resolved', 'closed'].map((value) => <option key={value}>{value}</option>)}</select> : <Status value={data.incident.status} />}</dd><dt>Service</dt><dd>{data.incident.service}</dd><dt>Impact scope</dt><dd>{data.incident.impact_scope}</dd><dt>Summary</dt><dd>{data.incident.summary}</dd><dt>Impact</dt><dd>{data.incident.impact}</dd><dt>Root cause</dt><dd>{data.incident.root_cause ?? 'Pending'}</dd><dt>Temporary fix</dt><dd>{data.incident.temporary_fix ?? 'Pending'}</dd><dt>Permanent fix</dt><dd>{data.incident.permanent_fix ?? 'Pending'}</dd><dt>Prevention</dt><dd>{data.incident.prevention ?? 'Pending'}</dd><dt>Validation evidence</dt><dd>{data.incident.validation_evidence ?? 'Pending'}</dd></dl><h3>Timeline</h3><DataTable rows={data.events ?? []} columns={[{ key: 'occurred_at', label: 'Time', render: (row) => formatDate(row.occurred_at) }, { key: 'event_type', label: 'Type' }, { key: 'message', label: 'Details', render: (row) => row.message ?? row.gateway_request_id ?? row.deployment_version }, { key: 'actor_admin_id', label: 'Changed by' }]} />{canWrite && <form className="section" onSubmit={(event) => void appendEvent(event)}><h3>Add event</h3><label>Type<select name="event_type">{['timeline', 'analysis', 'remediation', 'verification', 'link'].map((value) => <option key={value}>{value}</option>)}</select></label><label>Details<textarea name="message" maxLength={8192} /></label><label>Request ID<input name="gateway_request_id" type="text" /></label><label>Deployment version<input name="deployment_version" maxLength={255} /></label><button disabled={busy}>{busy ? 'Saving' : 'Add event'}</button></form>}</>}</QueryBody></aside></div>}
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
  const auth = useAuth()
  const queryClient = useQueryClient()
  const query = useAdminQuery<JsonObject>(['catalog'], '/catalog')
  const taskProfiles = useAdminQuery<CursorPage>(['model-task-profiles'], '/model-task-profiles?limit=100')
  const classifierConfigs = useAdminQuery<CursorPage>(['task-classifier-configs'], '/task-classifier-configs?limit=100')
  const [creating, setCreating] = useState<'provider' | 'model' | 'alias' | 'task-profile' | 'classifier-config' | null>(null)
  const [editing, setEditing] = useState<{ kind: 'provider' | 'model' | 'alias' | 'task-profile' | 'classifier-config'; row: JsonObject } | null>(null)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const canWrite = auth.role !== 'viewer'
  const canActivate = auth.role === 'admin'
  const mutate = async (path: string, init: RequestInit) => {
    setBusy(true); setError(null)
    try {
      await adminRequest(path, auth.token!, auth.clear, init)
      setCreating(null)
      setEditing(null)
      await queryClient.invalidateQueries({ queryKey: ['catalog'] })
      await queryClient.invalidateQueries({ queryKey: ['model-task-profiles'] })
      await queryClient.invalidateQueries({ queryKey: ['task-classifier-configs'] })
    } catch (cause) { setError(cause instanceof Error ? cause.message : 'Request failed') }
    finally { setBusy(false) }
  }
  const lifecycle = (resource: string, row: JsonObject, impact = '') => {
    const state = row.lifecycle_state === 'active' ? 'disabled' : 'active'
    if (state === 'disabled' && impact && !window.confirm(`Disable ${row.display_name ?? row.public_model_id ?? row.provider_key}? ${impact}`)) return
    return mutate(`/${resource}/${row.id}/lifecycle`, { method: 'POST', body: JSON.stringify({ version: row.version, state }) })
  }
  const actionColumn = (kind: 'provider' | 'model' | 'alias' | 'task-profile', resource: string, impact: (row: JsonObject) => string = () => '') => (canWrite || canActivate) ? [{ key: 'actions', label: 'Actions', render: (row: JsonObject) => <div className="row-actions">
    {canWrite && (kind === 'model' || ['draft', 'disabled'].includes(row.lifecycle_state)) && <button className="icon-button" title="Edit" disabled={busy} onClick={() => setEditing({ kind, row })}><Pencil /></button>}
    {canWrite && ['draft', 'disabled'].includes(row.lifecycle_state) && <button className="icon-button danger" title="Archive" disabled={busy} onClick={() => void mutate(`/${resource}/${row.id}?version=${row.version}`, { method: 'DELETE' })}><Trash2 /></button>}
    {canActivate && row.lifecycle_state !== 'archived' && <button className={`icon-button ${row.lifecycle_state === 'active' ? 'danger' : ''}`} title={row.lifecycle_state === 'active' ? 'Disable' : 'Activate'} disabled={busy} onClick={() => void lifecycle(resource, row, impact(row))}>{row.lifecycle_state === 'active' ? <PowerOff /> : <CirclePower />}</button>}
  </div> }] : []
  return <Page title="Models & Providers" actions={canWrite ? <div className="toolbar"><button onClick={() => setCreating('provider')}><Plus size={16} />Provider</button><button onClick={() => setCreating('model')}><Plus size={16} />Model</button><button onClick={() => setCreating('alias')}><Plus size={16} />Alias</button><button onClick={() => setCreating('task-profile')}><Plus size={16} />Task profile</button><button onClick={() => setCreating('classifier-config')}><Plus size={16} />Classifier</button></div> : undefined}>
    {error && <div className="state error">{error}</div>}
    <QueryBody query={query}>{(data) => <>
      <section className="section catalog-table"><h2>Quality aliases</h2><DataTable rows={data.aliases ?? []} columns={[{ key: 'quality_tier', label: 'Tier' }, { key: 'operation', label: 'Operation' }, { key: 'public_model_id', label: 'Gateway model' }, { key: 'provider_model_id', label: 'Actual model' }, { key: 'provider_key', label: 'Provider' }, { key: 'lifecycle_state', label: 'State', render: (row) => <Status value={row.lifecycle_state} /> }, ...actionColumn('alias', 'model-aliases')]} /></section>
      <section className="section catalog-table"><h2>Models</h2><DataTable rows={data.models ?? []} columns={[{ key: 'display_name', label: 'Model' }, { key: 'provider_key', label: 'Provider' }, { key: 'description', label: 'Purpose' }, { key: 'capabilities', label: 'Capabilities', render: (row) => ['streaming', 'tools', 'reasoning', 'images', 'structured_output'].filter((value) => row[`supports_${value}`]).join(', ') || 'text' }, { key: 'latency_class', label: 'Latency' }, { key: 'observed_success_rate', label: '30d success', render: (row) => typeof row.observed_success_rate === 'number' ? `${(row.observed_success_rate * 100).toFixed(1)}%` : 'No data' }, { key: 'observed_avg_latency_ms', label: '30d avg', render: (row) => typeof row.observed_avg_latency_ms === 'number' ? `${Math.round(row.observed_avg_latency_ms)} ms` : 'No data' }, { key: 'lifecycle_state', label: 'State', render: (row) => <Status value={row.lifecycle_state} /> }, ...actionColumn('model', 'models', (row) => `${(data.aliases ?? []).filter((alias: JsonObject) => alias.model_id === row.id && alias.lifecycle_state === 'active').length} active aliases will also be disabled.`)]} /></section>
      <section className="section catalog-table"><h2>Providers</h2><DataTable rows={data.providers ?? []} columns={[{ key: 'display_name', label: 'Provider' }, { key: 'provider_type', label: 'Type' }, { key: 'base_url', label: 'Base URL' }, { key: 'credential_configured', label: 'Credential', render: (row) => row.credential_configured ? 'Configured' : 'Missing' }, { key: 'lifecycle_state', label: 'State', render: (row) => <Status value={row.lifecycle_state} /> }, ...actionColumn('provider', 'providers', (row) => `${(data.models ?? []).filter((model: JsonObject) => model.provider_id === row.id && model.lifecycle_state === 'active').length} active models and their aliases will also be disabled.`)]} /></section>
      <section className="section catalog-table"><h2>Task profiles</h2><QueryBody query={taskProfiles}>{(profiles) => <DataTable rows={profiles.items} columns={[{ key: 'model_id', label: 'Model', render: (row) => (data.models ?? []).find((model: JsonObject) => model.id === row.model_id)?.display_name ?? row.model_id }, { key: 'task_kind', label: 'Task' }, { key: 'suitability_score', label: 'Suitability' }, { key: 'confidence', label: 'Confidence', render: (row) => `${Math.round(Number(row.confidence) * 100)}%` }, { key: 'source', label: 'Source' }, { key: 'lifecycle_state', label: 'State', render: (row) => <Status value={row.lifecycle_state} /> }, ...actionColumn('task-profile', 'model-task-profiles')]} />}</QueryBody></section>
      <section className="section catalog-table"><h2>Classifier</h2><QueryBody query={classifierConfigs}>{(configs) => <DataTable rows={configs.items} columns={[{ key: 'taxonomy_version', label: 'Taxonomy' }, { key: 'classifier_model_id', label: 'Model', render: (row) => (data.models ?? []).find((model: JsonObject) => model.id === row.classifier_model_id)?.display_name ?? 'None' }, { key: 'mode', label: 'Mode' }, { key: 'minimum_confidence', label: 'Minimum confidence', render: (row) => `${Math.round(Number(row.minimum_confidence) * 100)}%` }, { key: 'timeout_ms', label: 'Timeout' }, ...(canWrite ? [{ key: 'actions', label: 'Actions', render: (row: JsonObject) => <button className="icon-button" title="Edit classifier" disabled={busy} onClick={() => setEditing({ kind: 'classifier-config', row })}><Pencil /></button> }] : [])]} />}</QueryBody></section>
      {(creating || editing) && <CatalogDialog kind={creating ?? editing!.kind} initial={editing?.row} providers={data.providers ?? []} models={data.models ?? []} canActivate={canActivate} busy={busy} onClose={() => { setCreating(null); setEditing(null) }} onSubmit={(path, body, method) => void mutate(path, { method, body: JSON.stringify(body) })} />}
    </>}</QueryBody>
  </Page>
}

function CatalogDialog({ kind, initial, providers, models, canActivate, busy, onClose, onSubmit }: { kind: 'provider' | 'model' | 'alias' | 'task-profile' | 'classifier-config'; initial?: JsonObject; providers: JsonObject[]; models: JsonObject[]; canActivate: boolean; busy: boolean; onClose: () => void; onSubmit: (path: string, body: JsonObject, method: string) => void }) {
  const [validationError, setValidationError] = useState<string | null>(null)
  const submit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault()
    setValidationError(null)
    const data = new FormData(event.currentTarget)
    const capabilities = data.getAll('capabilities').map(String)
    const resource = kind === 'alias' ? 'model-aliases' : kind === 'task-profile' ? 'model-task-profiles' : kind === 'classifier-config' ? 'task-classifier-configs' : `${kind}s`
    const path = initial ? `/${resource}/${initial.id}` : `/${resource}`
    const version = initial ? { version: initial.version } : {}
    if (kind === 'provider') {
      const credentialRef = data.get('credential_ref')
      onSubmit(path, { ...version, ...(!initial ? { provider_key: data.get('provider_key'), provider_type: data.get('provider_type') } : {}), display_name: data.get('display_name'), base_url: data.get('base_url'), ...(!initial || credentialRef ? { credential_ref: credentialRef } : {}), supported_operations: data.getAll('operations') }, initial ? 'PATCH' : 'POST')
    }
    if (kind === 'model') {
      let costMetadata: JsonObject
      try { costMetadata = JSON.parse(String(data.get('cost_metadata') || '{}')) }
      catch { setValidationError('Cost metadata must be valid JSON.'); return }
      if (!costMetadata || Array.isArray(costMetadata) || typeof costMetadata !== 'object') {
        setValidationError('Cost metadata must be a JSON object.')
        return
      }
      onSubmit(path, { ...version, ...(!initial ? { provider_id: data.get('provider_id'), public_model_id: data.get('public_model_id') } : {}), provider_model_id: data.get('provider_model_id'), display_name: data.get('display_name'), description: data.get('description') || null, quality_tier: data.get('quality_tier') || null, context_window: Number(data.get('context_window')) || null, max_output_tokens: Number(data.get('max_output_tokens')) || null, latency_class: data.get('latency_class') || null, cost_metadata: costMetadata, supported_operations: data.getAll('operations'), capabilities, supports_streaming: capabilities.includes('streaming'), supports_tools: capabilities.includes('tools'), supports_reasoning: capabilities.includes('reasoning'), supports_images: capabilities.includes('images'), supports_structured_output: capabilities.includes('structured_output') }, initial ? 'PATCH' : 'POST')
    }
    if (kind === 'alias') onSubmit(path, { ...version, operation: data.get('operation'), quality_tier: data.get('quality_tier'), model_id: data.get('model_id') }, initial ? 'PATCH' : 'POST')
    if (kind === 'task-profile') onSubmit(path, { ...version, ...(!initial ? { model_id: data.get('model_id'), taxonomy_version: 'task-taxonomy-v1', task_kind: data.get('task_kind') } : {}), suitability_score: Number(data.get('suitability_score')), confidence: Number(data.get('confidence')), source: data.get('source'), source_ref: data.get('source_ref') || null, rationale: data.get('rationale') || null }, initial ? 'PATCH' : 'POST')
    if (kind === 'classifier-config') onSubmit(path, { ...version, ...(!initial ? { taxonomy_version: 'task-taxonomy-v1' } : {}), classifier_model_id: data.get('classifier_model_id') || null, mode: data.get('mode'), minimum_confidence: Number(data.get('minimum_confidence')), timeout_ms: Number(data.get('timeout_ms')) }, initial ? 'PATCH' : 'POST')
  }
  const operations = ['anthropic_messages', 'anthropic_count_tokens', 'openai_responses', 'openai_chat_completions']
  return <div className="dialog-backdrop"><form className="dialog" onSubmit={submit}><header><h2>{initial ? 'Edit' : 'Create'} {kind}</h2><button type="button" className="icon-button" title="Close" onClick={onClose}><X /></button></header>
    {validationError && <div className="state error">{validationError}</div>}
    {kind === 'provider' && <><label>Name<input name="display_name" required maxLength={120} defaultValue={initial?.display_name} /></label>{!initial && <><label>Provider key<input name="provider_key" required pattern="[a-z][a-z0-9\-]{0,62}" /></label><label>Type<select name="provider_type"><option value="openai">OpenAI</option><option value="anthropic">Anthropic</option><option value="gemini">Gemini</option><option value="xai">xAI</option><option value="local">Local</option></select></label></>}<label>Base URL<input name="base_url" required type="url" defaultValue={initial?.base_url} /></label><label>Credential reference<input name="credential_ref" required={!initial} placeholder={initial ? 'Leave blank to keep current reference' : 'env://OPENAI_API_KEY'} pattern="env://[A-Z][A-Z0-9_]{2,127}" /></label><fieldset><legend>Operations</legend>{operations.map((value) => <label className="check-label" key={value}><input type="checkbox" name="operations" value={value} defaultChecked={(initial?.supported_operations ?? []).includes(value)} />{value.replaceAll('_', ' ')}</label>)}</fieldset></>}
    {kind === 'model' && <>{!initial && <><label>Provider<select name="provider_id" required><option value="">Select provider</option>{providers.map((row) => <option key={row.id} value={row.id}>{row.display_name}</option>)}</select></label><label>Public model ID<input name="public_model_id" required maxLength={255} /></label></>}<label>Name<input name="display_name" required defaultValue={initial?.display_name} /></label><label>Provider model ID<input name="provider_model_id" required maxLength={255} defaultValue={initial?.provider_model_id} /></label><label>Purpose<input name="description" maxLength={1024} defaultValue={initial?.description} /></label><label>Legacy quality tier<select name="quality_tier" defaultValue={initial?.quality_tier ?? ''}><option value="">Unclassified</option><option>economy</option><option>standard</option><option>advanced</option><option>premium</option></select></label><label>Context window<input name="context_window" type="number" min="1" defaultValue={initial?.context_window} /></label><label>Max output tokens<input name="max_output_tokens" type="number" min="1" defaultValue={initial?.max_output_tokens} /></label><label>Latency class<input name="latency_class" maxLength={64} defaultValue={initial?.latency_class} /></label><label>Cost metadata<textarea name="cost_metadata" defaultValue={JSON.stringify(initial?.cost_metadata ?? {})} /></label><fieldset><legend>Operations</legend>{operations.map((value) => <label className="check-label" key={value}><input type="checkbox" name="operations" value={value} defaultChecked={(initial?.supported_operations ?? []).includes(value)} />{value.replaceAll('_', ' ')}</label>)}</fieldset><fieldset><legend>Capabilities</legend>{['streaming', 'tools', 'reasoning', 'images', 'structured_output'].map((value) => <label className="check-label" key={value}><input type="checkbox" name="capabilities" value={value} defaultChecked={Boolean(initial?.[`supports_${value}`])} />{value.replaceAll('_', ' ')}</label>)}</fieldset></>}
    {kind === 'alias' && <><label>Quality<select name="quality_tier" defaultValue={initial?.quality_tier}><option>economy</option><option>standard</option><option>advanced</option><option>premium</option></select></label><label>Operation<select name="operation" defaultValue={initial?.operation}>{operations.map((value) => <option key={value}>{value}</option>)}</select></label><label>Model<select name="model_id" required defaultValue={initial?.model_id}><option value="">Select model</option>{models.map((row) => <option key={row.id} value={row.id}>{row.display_name ?? row.public_model_id}</option>)}</select></label></>}
    {kind === 'task-profile' && <>{!initial && <><label>Model<select name="model_id" required><option value="">Select model</option>{models.map((row) => <option key={row.id} value={row.id}>{row.display_name ?? row.public_model_id}</option>)}</select></label><label>Task<select name="task_kind">{['code_generation', 'code_review', 'debugging', 'research', 'quantitative_reasoning', 'extraction', 'transformation', 'general_assistance'].map((value) => <option key={value}>{value}</option>)}</select></label></>}<label>Suitability<input name="suitability_score" type="number" min="0" max="100" required defaultValue={initial?.suitability_score ?? 50} /></label><label>Confidence<input name="confidence" type="number" min="0" max="1" step="0.0001" required defaultValue={initial?.confidence ?? 0.8} /></label><label>Source<select name="source" defaultValue={initial?.source === 'migration' ? 'manual' : initial?.source ?? 'manual'}><option value="manual">Manual override</option><option value="provider">Provider</option><option value="benchmark">Benchmark</option></select></label><label>Source reference<input name="source_ref" maxLength={1024} defaultValue={initial?.source_ref} /></label><label>Rationale<textarea name="rationale" maxLength={1024} defaultValue={initial?.rationale} /></label></>}
    {kind === 'classifier-config' && <><label>Classifier model<select name="classifier_model_id" required defaultValue={initial?.classifier_model_id}><option value="">Select model</option>{models.map((row) => <option key={row.id} value={row.id}>{row.display_name ?? row.public_model_id}</option>)}</select></label><label>Mode<select name="mode" defaultValue={initial?.mode ?? 'shadow'}><option value="disabled">Disabled</option><option value="shadow">Shadow</option>{canActivate && <option value="enforced">Enforced</option>}</select></label><label>Minimum confidence<input name="minimum_confidence" type="number" min="0" max="1" step="0.0001" required defaultValue={initial?.minimum_confidence ?? 0.8} /></label><label>Timeout (ms)<input name="timeout_ms" type="number" min="1" max="5000" required defaultValue={initial?.timeout_ms ?? 500} /></label></>}
    <footer><button type="button" className="secondary" onClick={onClose}>Cancel</button><button disabled={busy}>{busy ? 'Saving' : initial ? 'Save' : 'Create'}</button></footer>
  </form></div>
}

export function GovernancePage() {
  const auth = useAuth()
  const queryClient = useQueryClient()
  const query = useAdminQuery<JsonObject>(['governance'], '/governance')
  const identities = useAdminQuery<JsonObject>(['identity-map'], '/identity-map')
  const catalog = useAdminQuery<JsonObject>(['catalog'], '/catalog')
  const [creating, setCreating] = useState<'policy' | 'quota' | null>(null)
  const [editing, setEditing] = useState<{ kind: 'policy' | 'quota'; row: JsonObject } | null>(null)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const canWrite = auth.role !== 'viewer'
  const canActivate = auth.role === 'admin'
  const mutate = async (path: string, init: RequestInit) => {
    setBusy(true); setError(null)
    try {
      await adminRequest(path, auth.token!, auth.clear, init)
      setCreating(null); setEditing(null)
      await queryClient.invalidateQueries({ queryKey: ['governance'] })
    } catch (cause) { setError(cause instanceof Error ? cause.message : 'Request failed') }
    finally { setBusy(false) }
  }
  const lifecycle = (kind: 'policy' | 'quota', row: JsonObject) => mutate(`/${kind === 'policy' ? 'policies' : 'quotas'}/${row.id}/lifecycle`, { method: 'POST', body: JSON.stringify({ version: row.version, state: row.lifecycle_state === 'active' ? 'disabled' : 'active' }) })
  return <Page title="Governance" actions={canWrite ? <div className="toolbar"><button onClick={() => setCreating('policy')}><Plus size={16} />Policy</button><button onClick={() => setCreating('quota')}><Plus size={16} />Quota</button></div> : undefined}>
    {error && <div className="state error">{error}</div>}
    <QueryBody query={query}>{(data) => <>
    <div className="health-summary governance-health"><div className="runtime-summary"><div><span>Policy runtime</span><Status value={data.policy_runtime?.available ? 'active' : 'unavailable'} /></div><small>{data.policy_runtime?.active_count ?? 0} active</small></div><div className="runtime-summary"><div><span>Quota runtime</span><Status value={data.quota_runtime?.available ? 'active' : 'unavailable'} /></div><small>{data.quota_runtime?.active_count ?? 0} active · {data.quota_runtime?.window_count ?? 0} windows · {data.quota_runtime?.timezone ?? 'UTC'}</small></div></div>
    <div className="availability-grid">{data.scheduler?.available ? <div className="runtime-summary"><div><span>Scheduler runtime</span><Status value="active" /></div><small>{data.scheduler.active_tenant ?? 0} active · {data.scheduler.queued ?? 0} queued · {data.scheduler.active_capacity ?? 0} configured capacity</small></div> : <Unavailable reason={data.scheduler?.unavailable_reason} />}</div>
    <section className="section catalog-table"><h2>Policies</h2><DataTable rows={data.policies ?? []} columns={[{ key: 'policy_key', label: 'Policy' }, { key: 'revision', label: 'Revision' }, { key: 'scope', label: 'Scope', render: policyTarget }, { key: 'lifecycle_state', label: 'State', render: (row) => <Status value={row.lifecycle_state} /> }, { key: 'updated_at', label: 'Updated', render: (row) => formatDate(row.updated_at) }, ...((canWrite || canActivate) ? [{ key: 'actions', label: 'Actions', render: (row: JsonObject) => <div className="row-actions">
      {canWrite && row.lifecycle_state === 'draft' && <button className="icon-button" title="Edit policy" disabled={busy} onClick={() => setEditing({ kind: 'policy', row })}><Pencil /></button>}
      {canWrite && ['draft', 'disabled'].includes(row.lifecycle_state) && <button className="icon-button danger" title="Archive policy" disabled={busy} onClick={() => void mutate(`/policies/${row.id}?version=${row.version}`, { method: 'DELETE' })}><Trash2 /></button>}
      {canActivate && row.lifecycle_state !== 'archived' && <button className={`icon-button ${row.lifecycle_state === 'active' ? 'danger' : ''}`} title={row.lifecycle_state === 'active' ? 'Disable policy' : 'Activate policy'} disabled={busy} onClick={() => void lifecycle('policy', row)}>{row.lifecycle_state === 'active' ? <PowerOff /> : <CirclePower />}</button>}
    </div> }] : [])]} /></section>
    <section className="section catalog-table"><h2>Quotas</h2><DataTable rows={data.quotas ?? []} columns={[{ key: 'quota_key', label: 'Quota' }, { key: 'revision', label: 'Revision' }, { key: 'scope', label: 'Scope', render: quotaTarget }, { key: 'mode', label: 'Mode', render: (row) => row.configuration?.mode ?? 'Unavailable' }, { key: 'limits', label: 'Limits', render: (row) => <div className="quota-limit-list">{quotaLimits.filter(([key]) => row.configuration?.limits?.[key] != null).map(([key, label]) => <span key={key}>{label}: {compactNumber(row.configuration.limits[key])}</span>)}</div> }, { key: 'lifecycle_state', label: 'State', render: (row) => <Status value={row.lifecycle_state} /> }, { key: 'updated_at', label: 'Updated', render: (row) => formatDate(row.updated_at) }, ...((canWrite || canActivate) ? [{ key: 'actions', label: 'Actions', render: (row: JsonObject) => <div className="row-actions">
      {canWrite && row.lifecycle_state === 'draft' && <button className="icon-button" title="Edit quota" disabled={busy} onClick={() => setEditing({ kind: 'quota', row })}><Pencil /></button>}
      {canWrite && ['draft', 'disabled'].includes(row.lifecycle_state) && <button className="icon-button danger" title="Archive quota" disabled={busy} onClick={() => void mutate(`/quotas/${row.id}?version=${row.version}`, { method: 'DELETE' })}><Trash2 /></button>}
      {canActivate && row.lifecycle_state !== 'archived' && <button className={`icon-button ${row.lifecycle_state === 'active' ? 'danger' : ''}`} title={row.lifecycle_state === 'active' ? 'Disable quota' : 'Activate quota'} disabled={busy} onClick={() => void lifecycle('quota', row)}>{row.lifecycle_state === 'active' ? <PowerOff /> : <CirclePower />}</button>}
    </div> }] : [])]} />{(data.quota_runtime?.windows ?? []).length > 0 && <><h3>Active windows</h3><DataTable rows={data.quota_runtime.windows} columns={[{ key: 'quota_key', label: 'Quota' }, { key: 'limit_kind', label: 'Limit' }, { key: 'current', label: 'Current' }, { key: 'limit', label: 'Maximum' }, { key: 'reset_at', label: 'Resets', render: (row) => formatDate(row.reset_at) }]} /></>}</section>
    {(creating === 'policy' || editing?.kind === 'policy') && <PolicyDialog initial={editing?.kind === 'policy' ? editing.row : null} identities={identities.data} busy={busy} onClose={() => { setCreating(null); setEditing(null) }} onSubmit={(path, body, method) => void mutate(path, { method, body: JSON.stringify(body) })} />}
    {(creating === 'quota' || editing?.kind === 'quota') && <QuotaDialog initial={editing?.kind === 'quota' ? editing.row : null} identities={identities.data} catalog={catalog.data} busy={busy} onClose={() => { setCreating(null); setEditing(null) }} onSubmit={(path, body, method) => void mutate(path, { method, body: JSON.stringify(body) })} />}
  </>}</QueryBody></Page>
}

function policyScope(row: JsonObject) {
  return ['user', 'group', 'device', 'client', 'agent', 'credential'].find((scope) => row[`${scope}_id`]) ?? 'global'
}

function policyTarget(row: JsonObject) {
  const scope = policyScope(row)
  if (scope === 'global') return scope
  return `${scope} · ${String(row[`${scope}_id`]).slice(0, 8)}`
}

function quotaScope(row: JsonObject) {
  return ['user', 'group', 'device', 'client', 'agent', 'credential', 'model', 'provider'].find((scope) => row[`${scope}_id`]) ?? 'global'
}

function quotaTarget(row: JsonObject) {
  const scope = quotaScope(row)
  if (scope === 'global') return scope
  return `${scope} · ${String(row[`${scope}_id`]).slice(0, 8)}`
}

export function GroupTarget() {
  const [after, setAfter] = useState('')
  const groups = useAdminQuery<ResourcePage>(['groups', after], `/groups?limit=100${after ? `&after=${after}` : ''}`)
  return <QueryBody query={groups}>{(data) => <><label>群組<select name="scope_id" required key={after}><option value="">請選擇</option>{data.items.map((group) => <option key={group.id} value={group.id}>{group.display_name} · {String(group.id).slice(0, 8)}</option>)}</select></label>{after && <button type="button" onClick={() => setAfter('')}>回第一頁</button>}{data.next_after && <button type="button" onClick={() => setAfter(data.next_after!)}>下一頁群組</button>}</>}</QueryBody>
}

function PolicyDialog({ initial, identities, busy, onClose, onSubmit }: { initial: JsonObject | null; identities?: JsonObject; busy: boolean; onClose: () => void; onSubmit: (path: string, body: JsonObject, method: string) => void }) {
  const [scope, setScope] = useState(initial ? policyScope(initial) : 'global')
  const [validationError, setValidationError] = useState<string | null>(null)
  const entities = scope === 'global' ? [] : scope === 'credential'
    ? (identities?.paths ?? []).map((path: JsonObject) => path.credential)
    : principalEntities(identities, scope)
  const submit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault(); setValidationError(null)
    const data = new FormData(event.currentTarget)
    let configuration: JsonObject
    try { configuration = JSON.parse(String(data.get('configuration'))) }
    catch { setValidationError('Configuration must be valid JSON.'); return }
    if (!configuration || Array.isArray(configuration) || typeof configuration !== 'object') {
      setValidationError('Configuration must be a JSON object.'); return
    }
    if (initial) { onSubmit(`/policies/${initial.id}`, { version: initial.version, configuration }, 'PATCH'); return }
    const scopedId = scope === 'global' ? null : String(data.get('scope_id') || '')
    if (scope !== 'global' && !scopedId) { setValidationError('Select a scope target.'); return }
    onSubmit('/policies', {
      policy_key: data.get('policy_key'), revision: Number(data.get('revision')),
      user_id: scope === 'user' ? scopedId : null, group_id: scope === 'group' ? scopedId : null, device_id: scope === 'device' ? scopedId : null,
      client_id: scope === 'client' ? scopedId : null, agent_id: scope === 'agent' ? scopedId : null,
      credential_id: scope === 'credential' ? scopedId : null, configuration,
    }, 'POST')
  }
  return <div className="dialog-backdrop"><form className="dialog policy-dialog" onSubmit={submit}><header><h2>{initial ? 'Edit policy' : 'Create policy'}</h2><button type="button" className="icon-button" title="Close" onClick={onClose}><X /></button></header>
    {validationError && <div className="state error">{validationError}</div>}
    {!initial && <><label>Policy key<input name="policy_key" required maxLength={200} /></label><label>Revision<input name="revision" type="number" min="1" required defaultValue="1" /></label><label>Scope<select name="scope" value={scope} onChange={(event) => setScope(event.target.value)}>{['global', 'user', 'group', 'device', 'client', 'agent', 'credential'].map((value) => <option key={value}>{value}</option>)}</select></label>{scope === 'group' ? <GroupTarget /> : scope !== 'global' && <label>Scope target<select name="scope_id" required><option value="">Select target</option>{entities.map((entity: JsonObject) => <option key={entity.id} value={entity.id}>{entity.display_name} · {String(entity.id).slice(0, 8)}</option>)}</select></label>}</>}
    <label>Configuration<textarea name="configuration" required spellCheck={false} defaultValue={JSON.stringify(initial?.configuration ?? { defaults: { workload_class: 'interactive', urgency: 'normal', quality: 'standard', budget_class: 'normal' } }, null, 2)} /></label>
    <footer><button type="button" className="secondary" onClick={onClose}>Cancel</button><button disabled={busy}>{busy ? 'Saving' : initial ? 'Save' : 'Create'}</button></footer>
  </form></div>
}

const quotaLimits = [
  ['requests_per_minute', 'Requests per minute'],
  ['estimated_tokens_per_minute', 'Estimated tokens per minute'],
  ['tokens_per_hour', 'Tokens per hour'],
  ['tokens_per_day', 'Tokens per day'],
  ['concurrent_requests', 'Concurrent requests'],
  ['cost_per_day_microusd', 'Daily cost (micro-USD)'],
  ['cost_per_month_microusd', 'Monthly cost (micro-USD)'],
] as const

function QuotaDialog({ initial, identities, catalog, busy, onClose, onSubmit }: { initial: JsonObject | null; identities?: JsonObject; catalog?: JsonObject; busy: boolean; onClose: () => void; onSubmit: (path: string, body: JsonObject, method: string) => void }) {
  const [scope, setScope] = useState(initial ? quotaScope(initial) : 'global')
  const [validationError, setValidationError] = useState<string | null>(null)
  const entities = scope === 'global' ? [] : scope === 'credential'
    ? (identities?.paths ?? []).map((path: JsonObject) => path.credential)
    : scope === 'model' ? (catalog?.models ?? [])
    : scope === 'provider' ? (catalog?.providers ?? [])
    : principalEntities(identities, scope)
  const submit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault(); setValidationError(null)
    const data = new FormData(event.currentTarget)
    const limits: JsonObject = {}
    for (const [key] of quotaLimits) {
      const raw = String(data.get(key) ?? '')
      if (!raw) continue
      const value = Number(raw)
      if (!Number.isSafeInteger(value) || value < 0) { setValidationError('Limits must be non-negative whole numbers.'); return }
      limits[key] = value
    }
    if (!Object.keys(limits).length) { setValidationError('Set at least one limit.'); return }
    const configuration = { mode: data.get('mode'), limits }
    if (initial) { onSubmit(`/quotas/${initial.id}`, { version: initial.version, configuration }, 'PATCH'); return }
    const scopedId = scope === 'global' ? null : String(data.get('scope_id') || '')
    if (scope !== 'global' && !scopedId) { setValidationError('Select a scope target.'); return }
    onSubmit('/quotas', {
      quota_key: data.get('quota_key'), revision: Number(data.get('revision')),
      user_id: scope === 'user' ? scopedId : null, group_id: scope === 'group' ? scopedId : null, device_id: scope === 'device' ? scopedId : null,
      client_id: scope === 'client' ? scopedId : null, agent_id: scope === 'agent' ? scopedId : null,
      credential_id: scope === 'credential' ? scopedId : null, model_id: scope === 'model' ? scopedId : null,
      provider_id: scope === 'provider' ? scopedId : null, configuration,
    }, 'POST')
  }
  const configured = initial?.configuration?.limits ?? {}
  return <div className="dialog-backdrop"><form className="dialog policy-dialog" onSubmit={submit}><header><h2>{initial ? 'Edit quota' : 'Create quota'}</h2><button type="button" className="icon-button" title="Close" onClick={onClose}><X /></button></header>
    {validationError && <div className="state error">{validationError}</div>}
    {!initial && <><label>Quota key<input name="quota_key" required maxLength={200} /></label><label>Revision<input name="revision" type="number" min="1" required defaultValue="1" /></label><label>Scope<select name="scope" value={scope} onChange={(event) => setScope(event.target.value)}>{['global', 'user', 'group', 'device', 'client', 'agent', 'credential', 'model', 'provider'].map((value) => <option key={value}>{value}</option>)}</select></label>{scope === 'group' ? <GroupTarget /> : scope !== 'global' && <label>Scope target<select name="scope_id" required><option value="">Select target</option>{entities.map((entity: JsonObject) => <option key={entity.id} value={entity.id}>{entity.display_name ?? entity.public_model_id ?? entity.provider_key} · {String(entity.id).slice(0, 8)}</option>)}</select></label>}</>}
    <label>Mode<select name="mode" defaultValue={initial?.configuration?.mode ?? 'hard'}><option value="hard">Hard</option><option value="soft">Soft</option></select></label>
    <fieldset><legend>Limits</legend>{quotaLimits.map(([key, label]) => <label key={key}>{label}<input name={key} type="number" min="0" step="1" defaultValue={configured[key] ?? (key === 'requests_per_minute' && !initial ? 100 : '')} /></label>)}</fieldset>
    <footer><button type="button" className="secondary" onClick={onClose}>Cancel</button><button disabled={busy}>{busy ? 'Saving' : initial ? 'Save' : 'Create'}</button></footer>
  </form></div>
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

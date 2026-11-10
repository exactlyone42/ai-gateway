import { useQuery } from '@tanstack/react-query'
import { useState, type FormEvent } from 'react'
import { adminRequest } from './api'
import { useAuth } from './auth'
import type { ResourcePage } from './types'

export function GroupCredentials({ group }: { group: string }) {
  const auth = useAuth()
  const [after, setAfter] = useState('')
  const [provider, setProvider] = useState('')
  const [kind, setKind] = useState('api')
  const providers = useQuery<ResourcePage>({
    queryKey: ['group-credential-providers', after],
    queryFn: () => adminRequest(`/providers?limit=100${after ? `&after=${encodeURIComponent(after)}` : ''}`, auth.token!, auth.clear),
    enabled: auth.role === 'admin',
    retry: false,
  })
  if (auth.role !== 'admin') return null
  const selected = providers.data?.items.find((item) => item.provider_key === provider)
  return <section className="section"><h3>群組供應商憑證</h3><p>僅供同一人的帳號使用；成員有個人憑證時優先使用個人憑證。</p>
    {providers.error && <div role="alert">無法讀取供應商，請重新整理。</div>}
    <label>憑證供應商<select value={provider} onChange={(event) => { setProvider(event.target.value); setKind('api') }}><option value="">請選擇</option>{providers.data?.items.filter((item) => item.lifecycle_state === 'active').map((item) => <option key={item.id} value={item.provider_key}>{item.display_name}</option>)}</select></label>
    {after && <button onClick={() => { setAfter(''); setProvider(''); setKind('api') }}>回第一頁供應商</button>}
    {providers.data?.next_after && <button onClick={() => { setAfter(providers.data!.next_after!); setProvider(''); setKind('api') }}>下一頁供應商</button>}
    {selected && <><label>憑證種類<select value={kind} onChange={(event) => setKind(event.target.value)}><option value="api">API 金鑰</option>{selected.provider_type === 'anthropic' && <option value="subscription">訂閱權杖</option>}</select></label><CredentialForm key={`${group}/${provider}/${kind}`} path={`/groups/${group}/provider-credentials/${encodeURIComponent(provider)}/${kind}`} /></>}
  </section>
}

function CredentialForm({ path }: { path: string }) {
  const auth = useAuth()
  const [secret, setSecret] = useState('')
  const [busy, setBusy] = useState(false)
  const [message, setMessage] = useState('')
  const status = useQuery<{ configured: boolean; enabled?: boolean; consistent?: boolean }>({
    queryKey: ['group-credential-status', path],
    queryFn: () => adminRequest(path, auth.token!, auth.clear),
    retry: false,
    staleTime: 0,
  })
  async function change(method: 'PUT' | 'DELETE') {
    setBusy(true)
    setMessage('')
    const token = secret
    setSecret('')
    try {
      await adminRequest(path, auth.token!, auth.clear, { method, ...(method === 'PUT' ? { body: JSON.stringify({ token }) } : {}) })
      setMessage(method === 'PUT' ? '憑證已更新。' : '憑證已撤銷。')
    } catch {
      setMessage('操作結果未確認，請先核對下方狀態；狀態不一致時請重新設定憑證。')
    } finally {
      await status.refetch()
      setBusy(false)
    }
  }
  function submit(event: FormEvent) {
    event.preventDefault()
    if (!busy && secret.trim()) void change('PUT')
  }
  return <div>
    <p role="status">{status.isFetching ? '正在核對憑證狀態…' : status.error ? '無法確認憑證狀態。' : status.data?.configured ? '憑證已設定並啟用。' : status.data?.enabled && status.data?.consistent === false ? '憑證與綁定不一致，請重新設定。' : '尚未設定或已撤銷。'}</p>
    <button disabled={busy || status.isFetching} onClick={() => void status.refetch()}>重新核對狀態</button>
    {message && <p role="alert">{message}</p>}
    <form onSubmit={submit}><label>本人供應商憑證<input type="password" autoComplete="new-password" value={secret} disabled={busy} onChange={(event) => setSecret(event.target.value)} required /></label><button disabled={busy || status.isFetching || Boolean(status.error) || !secret.trim()}>更新憑證</button></form>
    <button disabled={busy || status.isFetching || Boolean(status.error) || !status.data?.enabled} onClick={() => { if (!busy) void change('DELETE') }}>撤銷憑證</button>
  </div>
}

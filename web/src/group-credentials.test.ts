import { createElement } from 'react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, beforeEach, expect, test, vi } from 'vitest'
import { GroupCredentials } from './group-credentials'

const mocks = vi.hoisted(() => ({ role: 'admin', request: vi.fn() }))
vi.mock('./auth', () => ({ useAuth: () => ({ role: mocks.role, token: 'session', clear: vi.fn() }) }))
vi.mock('./api', () => ({ adminRequest: mocks.request }))

beforeEach(() => {
  mocks.role = 'admin'
  mocks.request.mockReset()
  mocks.request.mockImplementation(async (path: string) => path.startsWith('/providers') ? {
    items: [{ id: 'p1', provider_key: path.includes('after=') ? 'second' : 'first', display_name: path.includes('after=') ? '第二個供應商' : '第一個供應商', lifecycle_state: 'active', provider_type: 'anthropic' }],
    next_after: path.includes('after=') ? null : 'page2',
  } : { configured: true, enabled: true, consistent: true })
})
afterEach(cleanup)

function mount() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false }, mutations: { retry: false } } })
  const view = render(createElement(QueryClientProvider, { client }, createElement(GroupCredentials, { key: 'group1', group: 'group1' })))
  return { client, ...view }
}

async function selectProvider() {
  await screen.findByText('第一個供應商')
  fireEvent.change(screen.getByLabelText('憑證供應商'), { target: { value: 'first' } })
  await screen.findByText('憑證已設定並啟用。')
}

test('更新只送到所選群組與種類，清空秘密且不進查詢快取', async () => {
  const { client } = mount()
  await selectProvider()
  fireEvent.change(screen.getByLabelText('憑證種類'), { target: { value: 'subscription' } })
  await screen.findByText('憑證已設定並啟用。')
  fireEvent.change(screen.getByLabelText('本人供應商憑證'), { target: { value: 'private-token-value' } })
  fireEvent.click(screen.getByRole('button', { name: '更新憑證' }))
  await screen.findByText('憑證已更新。')
  expect(mocks.request).toHaveBeenCalledWith('/groups/group1/provider-credentials/first/subscription', 'session', expect.any(Function), { method: 'PUT', body: JSON.stringify({ token: 'private-token-value' }) })
  expect((screen.getByLabelText('本人供應商憑證') as HTMLInputElement).value).toBe('')
  expect(JSON.stringify(client.getQueryCache().getAll())).not.toContain('private-token-value')
})

test('供應商分頁、種類與群組切換均清空舊輸入', async () => {
  const { client, rerender } = mount()
  await selectProvider()
  const fill = () => fireEvent.change(screen.getByLabelText('本人供應商憑證'), { target: { value: 'old-secret' } })
  fill()
  fireEvent.change(screen.getByLabelText('憑證種類'), { target: { value: 'subscription' } })
  expect((screen.getByLabelText('本人供應商憑證') as HTMLInputElement).value).toBe('')
  fill()
  fireEvent.click(screen.getByRole('button', { name: '下一頁供應商' }))
  await screen.findByText('第二個供應商')
  expect(screen.queryByLabelText('本人供應商憑證')).toBeNull()
  fireEvent.change(screen.getByLabelText('憑證供應商'), { target: { value: 'second' } })
  fill()
  rerender(createElement(QueryClientProvider, { client }, createElement(GroupCredentials, { key: 'group2', group: 'group2' })))
  expect(screen.queryByLabelText('本人供應商憑證')).toBeNull()
})

test('結果不明只讀回狀態，不自動重送或顯示秘密', async () => {
  mount()
  await selectProvider()
  const original = mocks.request.getMockImplementation()!
  mocks.request.mockImplementation(async (...args: unknown[]) => {
    if ((args[3] as RequestInit | undefined)?.method === 'DELETE') throw new Error('secret-error-detail')
    return original(...args)
  })
  fireEvent.click(screen.getByRole('button', { name: '撤銷憑證' }))
  await screen.findByText(/操作結果未確認/)
  await waitFor(() => expect(screen.getByRole('button', { name: '撤銷憑證' })).not.toBeDisabled())
  expect(mocks.request.mock.calls.filter((call) => call[3]?.method === 'DELETE')).toHaveLength(1)
  expect(mocks.request.mock.calls.filter((call) => call[0].includes('provider-credentials') && !call[3])).toHaveLength(2)
  expect(screen.queryByText('secret-error-detail')).toBeNull()
})

test.each(['operator', 'viewer'])('%s 不顯示或讀取群組憑證', (role) => {
  mocks.role = role
  mount()
  expect(screen.queryByText('群組供應商憑證')).toBeNull()
  expect(mocks.request).not.toHaveBeenCalled()
})

test('狀態讀取失敗不重試，重新核對成功前禁止修改', async () => {
  const original = mocks.request.getMockImplementation()!
  let unavailable = true
  mocks.request.mockImplementation(async (...args: unknown[]) => {
    if (String(args[0]).includes('provider-credentials') && unavailable) throw new Error('offline')
    return original(...args)
  })
  mount()
  await screen.findByText('第一個供應商')
  fireEvent.change(screen.getByLabelText('憑證供應商'), { target: { value: 'first' } })
  await screen.findByText('無法確認憑證狀態。')
  fireEvent.change(screen.getByLabelText('本人供應商憑證'), { target: { value: 'new-token' } })
  expect(screen.getByRole('button', { name: '更新憑證' })).toBeDisabled()
  expect(screen.getByRole('button', { name: '撤銷憑證' })).toBeDisabled()
  expect(mocks.request.mock.calls.filter((call) => call[0].includes('provider-credentials'))).toHaveLength(1)
  unavailable = false
  fireEvent.click(screen.getByRole('button', { name: '重新核對狀態' }))
  await screen.findByText('憑證已設定並啟用。')
  expect(screen.getByRole('button', { name: '更新憑證' })).not.toBeDisabled()
})

test('更新後斷線且狀態未知，只送一次並清空秘密、禁止修改', async () => {
  mount()
  await selectProvider()
  mocks.request.mockRejectedValue(new Error('offline'))
  fireEvent.change(screen.getByLabelText('本人供應商憑證'), { target: { value: 'private-token' } })
  fireEvent.click(screen.getByRole('button', { name: '更新憑證' }))
  await screen.findByText('無法確認憑證狀態。')
  expect(mocks.request.mock.calls.filter((call) => call[3]?.method === 'PUT')).toHaveLength(1)
  expect((screen.getByLabelText('本人供應商憑證') as HTMLInputElement).value).toBe('')
  expect(screen.getByRole('button', { name: '更新憑證' })).toBeDisabled()
  expect(screen.getByRole('button', { name: '撤銷憑證' })).toBeDisabled()
})

test.each(['PUT', 'DELETE'])('%s 未完成時切換種類，舊結果不影響新對象', async (method) => {
  mount()
  await selectProvider()
  let complete!: () => void
  const pending = new Promise<void>((resolve) => { complete = resolve })
  const original = mocks.request.getMockImplementation()!
  mocks.request.mockImplementation(async (...args: unknown[]) => (args[3] as RequestInit | undefined)?.method === method ? pending : original(...args))
  fireEvent.change(screen.getByLabelText('本人供應商憑證'), { target: { value: 'private-token' } })
  fireEvent.click(screen.getByRole('button', { name: method === 'PUT' ? '更新憑證' : '撤銷憑證' }))
  fireEvent.change(screen.getByLabelText('憑證種類'), { target: { value: 'subscription' } })
  await screen.findByText('憑證已設定並啟用。')
  complete()
  await waitFor(() => expect(mocks.request.mock.calls.filter((call) => call[0].endsWith('/api') && !call[3])).toHaveLength(2))
  expect(screen.queryByText('憑證已更新。')).toBeNull()
  expect(screen.queryByText('憑證已撤銷。')).toBeNull()
  expect((screen.getByLabelText('本人供應商憑證') as HTMLInputElement).value).toBe('')
  expect(mocks.request.mock.calls.filter((call) => call[3]?.method === method)).toHaveLength(1)
})

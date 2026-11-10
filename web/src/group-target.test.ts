import { createElement } from 'react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, expect, test, vi } from 'vitest'
import { GroupTarget } from './pages'

vi.mock('./auth', () => ({ useAuth: () => ({ token: 'test', clear: vi.fn() }) }))
vi.mock('./api', () => ({ adminRequest: async (path: string) => path.includes('after=')
  ? { items: [{ id: 'second-id', display_name: '第二頁群組' }], next_after: null }
  : { items: [{ id: 'first-id', display_name: '第一頁群組' }], next_after: 'cursor' } }))

afterEach(cleanup)

test('選擇後續頁面的群組，換頁後不保留舊選擇', async () => {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } })
  render(createElement(QueryClientProvider, { client }, createElement('form', null, createElement(GroupTarget))))
  await screen.findByText(/第一頁群組/)
  const first = screen.getByLabelText('群組') as HTMLSelectElement
  fireEvent.change(first, { target: { value: 'first-id' } })
  expect(first.value).toBe('first-id')
  fireEvent.click(screen.getByRole('button', { name: '下一頁群組' }))
  await screen.findByText(/第二頁群組/)
  const second = screen.getByLabelText('群組') as HTMLSelectElement
  expect(second.value).toBe('')
  expect(second.checkValidity()).toBe(false)
  fireEvent.change(second, { target: { value: 'second-id' } })
  await waitFor(() => expect(second.value).toBe('second-id'))
})

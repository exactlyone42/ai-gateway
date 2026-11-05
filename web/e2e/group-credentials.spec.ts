import { expect, test } from '@playwright/test'

test('管理者從群組畫面設定與撤銷憑證，輸入不留在畫面', async ({ page }) => {
  let configured = false
  const writes: { method: string; path: string }[] = []
  await page.route('**/admin/v1/**', async (route) => {
    const request = route.request()
    const path = new URL(request.url()).pathname.replace('/admin/v1', '')
    if (path.includes('/provider-credentials/')) {
      if (request.method() !== 'GET') {
        writes.push({ method: request.method(), path })
        configured = request.method() === 'PUT'
        await route.fulfill({ status: 204 })
      } else await route.fulfill({ json: { configured, enabled: configured, consistent: true } })
      return
    }
    const body = path === '/sessions' ? { access_token: 'fake-session', role: 'admin', expires_at: new Date(Date.now() + 600_000).toISOString() }
      : path === '/groups' ? { items: [{ id: 'group1', display_name: '本人群組' }], next_after: null }
      : path === '/providers' ? { items: [{ id: 'provider1', provider_key: 'anthropic', provider_type: 'anthropic', display_name: '測試供應商', lifecycle_state: 'active' }], next_after: null }
      : path === '/identity-map' ? { paths: [], unassociated: {} }
      : { items: [], next_after: null }
    await route.fulfill({ json: body })
  })
  await page.goto('/admin/')
  await page.getByLabel('Login credential').fill('fake-login')
  await page.getByRole('button', { name: 'Sign in' }).click()
  await page.getByRole('link', { name: 'Identities', exact: true }).click()
  await page.getByLabel('選擇群組').selectOption('group1')
  await page.getByLabel('憑證供應商').selectOption('anthropic')
  await page.getByLabel('憑證種類').selectOption('subscription')
  await expect(page.getByText('尚未設定或已撤銷。', { exact: true })).toBeVisible()
  await page.getByLabel('本人供應商憑證').fill('fake-secret-browser')
  await page.getByRole('button', { name: '更新憑證', exact: true }).click()
  await expect(page.getByText('憑證已設定並啟用。', { exact: true })).toBeVisible()
  await expect(page.getByLabel('本人供應商憑證')).toHaveValue('')
  await page.getByRole('button', { name: '撤銷憑證', exact: true }).click()
  await expect(page.getByText('尚未設定或已撤銷。', { exact: true })).toBeVisible()
  expect(writes).toEqual([
    { method: 'PUT', path: '/groups/group1/provider-credentials/anthropic/subscription' },
    { method: 'DELETE', path: '/groups/group1/provider-credentials/anthropic/subscription' },
  ])
})

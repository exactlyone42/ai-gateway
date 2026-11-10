import { expect, test } from '@playwright/test'

test('真正API保存與撤銷群組憑證，其他管理角色被拒絕', async ({ page, request }) => {
  const bootstrapToken = process.env.ADMIN_BOOTSTRAP_TOKEN
  if (!bootstrapToken) throw new Error('ADMIN_BOOTSTRAP_TOKEN is required')
  const bootstrap = await request.post('/admin/v1/bootstrap', {
    headers: { authorization: `Bearer ${bootstrapToken}`, 'x-request-id': crypto.randomUUID() },
    data: { display_name: '隔離管理者', credential_name: '隔離登入' },
  })
  expect(bootstrap.status()).toBe(201)
  const { login_token: login } = await bootstrap.json()
  const session = await request.post('/admin/v1/sessions', { headers: { authorization: `Bearer ${login}`, 'x-request-id': crypto.randomUUID() } })
  expect(session.status()).toBe(201)
  const { access_token: token } = await session.json()
  const headers = { authorization: `Bearer ${token}`, 'x-request-id': crypto.randomUUID() }
  const groupResponse = await request.post('/admin/v1/groups', { headers, data: { display_name: '本人隔離群組' } })
  expect(groupResponse.status()).toBe(201)
  const group = await groupResponse.json()
  const providerResponse = await request.post('/admin/v1/providers', { headers, data: {
    provider_key: 'anthropic', display_name: '隔離供應商', provider_type: 'anthropic', base_url: 'http://127.0.0.1:1', credential_ref: 'env://ANTHROPIC_API_KEY', supported_operations: ['anthropic_messages'],
  } })
  expect(providerResponse.status()).toBe(201)
  const { resource: provider } = await providerResponse.json()
  const activation = await request.post(`/admin/v1/providers/${provider.id}/lifecycle`, { headers, data: { version: provider.version, state: 'active' } })
  expect(activation.status()).toBe(200)
  const path = `/admin/v1/groups/${group.id}/provider-credentials/anthropic/subscription`
  await page.goto('/admin/')
  await page.getByLabel('Login credential').fill(login)
  await page.getByRole('button', { name: 'Sign in' }).click()
  await page.getByRole('link', { name: 'Identities', exact: true }).click()
  await page.getByLabel('選擇群組').selectOption(group.id)
  await page.getByLabel('憑證供應商').selectOption('anthropic')
  await page.getByLabel('憑證種類').selectOption('subscription')
  await expect(page.getByText('尚未設定或已撤銷。', { exact: true })).toBeVisible()
  await page.getByLabel('本人供應商憑證').fill('sk-ant-oat01-isolated-browser-fixture')
  await page.getByRole('button', { name: '更新憑證', exact: true }).click()
  await expect(page.getByText('憑證已設定並啟用。', { exact: true })).toBeVisible()
  await expect(page.getByLabel('本人供應商憑證')).toHaveValue('')
  const status = await request.get(path, { headers })
  expect(await status.json()).toEqual({ configured: true, enabled: true, consistent: true })
  const otherRoles: { authorization: string; 'x-request-id': string }[] = []
  for (const role of ['operator', 'viewer']) {
    const account = await request.post('/admin/v1/admin-accounts', { headers, data: { display_name: role, credential_name: `${role}-login`, role } })
    expect(account.status()).toBe(201)
    const { login_token: roleLogin } = await account.json()
    const roleSession = await request.post('/admin/v1/sessions', { headers: { authorization: `Bearer ${roleLogin}`, 'x-request-id': crypto.randomUUID() } })
    expect(roleSession.status()).toBe(201)
    const { access_token: roleToken } = await roleSession.json()
    const roleHeaders = { authorization: `Bearer ${roleToken}`, 'x-request-id': crypto.randomUUID() }
    otherRoles.push(roleHeaders)
    expect((await request.get(path, { headers: roleHeaders })).status()).toBe(403)
    expect((await request.put(path, { headers: roleHeaders, data: { token: 'sk-ant-oat01-forbidden-fixture' } })).status()).toBe(403)
    expect((await request.delete(path, { headers: roleHeaders })).status()).toBe(403)
    expect(await (await request.get(path, { headers })).json()).toEqual({ configured: true, enabled: true, consistent: true })
  }
  await page.getByRole('button', { name: '撤銷憑證', exact: true }).click()
  await expect(page.getByText('尚未設定或已撤銷。', { exact: true })).toBeVisible()
  const revoked = await request.get(path, { headers })
  expect(await revoked.json()).toEqual({ configured: false, enabled: false, consistent: false })
  await page.getByLabel('憑證種類').selectOption('api')
  await expect(page.getByText('尚未設定或已撤銷。', { exact: true })).toBeVisible()
  await page.getByLabel('本人供應商憑證').fill('isolated-browser-api-fixture')
  await page.getByRole('button', { name: '更新憑證', exact: true }).click()
  await expect(page.getByText('憑證已設定並啟用。', { exact: true })).toBeVisible()
  const apiPath = path.replace('/subscription', '/api')
  for (const roleHeaders of otherRoles) {
    expect((await request.get(apiPath, { headers: roleHeaders })).status()).toBe(403)
    expect((await request.put(apiPath, { headers: roleHeaders, data: { token: 'forbidden-api-fixture' } })).status()).toBe(403)
    expect((await request.delete(apiPath, { headers: roleHeaders })).status()).toBe(403)
    expect(await (await request.get(apiPath, { headers })).json()).toEqual({ configured: true, enabled: true, consistent: true })
  }
  expect(await (await request.get(path, { headers })).json()).toEqual({ configured: false, enabled: false, consistent: false })
  await page.getByRole('button', { name: '撤銷憑證', exact: true }).click()
  await expect(page.getByText('尚未設定或已撤銷。', { exact: true })).toBeVisible()
  expect(await (await request.get(apiPath, { headers })).json()).toEqual({ configured: false, enabled: false, consistent: false })
})

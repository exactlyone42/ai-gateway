import { expect, test } from '@playwright/test'

test('real gateway serves all admin views on desktop and mobile', async ({ browser, request }) => {
  const bootstrapToken = process.env.ADMIN_BOOTSTRAP_TOKEN
  if (!bootstrapToken) throw new Error('ADMIN_BOOTSTRAP_TOKEN is required')

  const bootstrap = await request.post('/admin/v1/bootstrap', {
    headers: { authorization: `Bearer ${bootstrapToken}`, 'x-request-id': crypto.randomUUID() },
    data: { display_name: 'Acceptance Admin', credential_name: 'Browser login' },
  })
  expect(bootstrap.status()).toBe(201)
  const { login_token: loginToken } = await bootstrap.json()
  const adminSessionResponse = await request.post('/admin/v1/sessions', {
    headers: { authorization: `Bearer ${loginToken}`, 'x-request-id': crypto.randomUUID() },
  })
  expect(adminSessionResponse.status()).toBe(201)
  const { access_token: adminToken } = await adminSessionResponse.json()
  const viewerAccountResponse = await request.post('/admin/v1/admin-accounts', {
    headers: { authorization: `Bearer ${adminToken}`, 'x-request-id': crypto.randomUUID() },
    data: { display_name: 'Acceptance Viewer', credential_name: 'Viewer login', role: 'viewer' },
  })
  expect(viewerAccountResponse.status()).toBe(201)
  const { login_token: viewerLoginToken } = await viewerAccountResponse.json()
  const viewerSessionResponse = await request.post('/admin/v1/sessions', {
    headers: { authorization: `Bearer ${viewerLoginToken}`, 'x-request-id': crypto.randomUUID() },
  })
  expect(viewerSessionResponse.status()).toBe(201)
  const { access_token: viewerToken } = await viewerSessionResponse.json()

  const desktop = await browser.newContext({ viewport: { width: 1440, height: 1000 } })
  const page = await desktop.newPage()
  const consoleErrors: string[] = []
  const failedResponses: string[] = []
  page.on('console', (message) => { if (message.type() === 'error') consoleErrors.push(message.text()) })
  page.on('response', (response) => {
    if (response.status() >= 400) failedResponses.push(`${response.status()} ${new URL(response.url()).pathname}${new URL(response.url()).search}`)
  })
  await page.goto('/admin/')
  await page.getByLabel('Login credential').fill(loginToken)
  await page.getByRole('button', { name: 'Sign in' }).click()
  await expect(page.getByRole('heading', { name: 'Overview' })).toBeVisible()
  await page.screenshot({ path: '../.artifacts/w09-desktop.png', fullPage: true })

  await page.getByRole('link', { name: 'Identities' }).click()
  await expect(page.getByRole('heading', { name: 'Identities' })).toBeVisible()
  await page.getByRole('button', { name: 'Create identity' }).click()
  await page.getByLabel('Name').fill('Acceptance User')
  await page.locator('form.dialog').getByRole('button', { name: 'Create', exact: true }).click()
  await expect(page.getByText('Acceptance User')).toBeVisible()
  await page.getByRole('link', { name: 'Credentials' }).click()
  await expect(page.getByRole('heading', { name: 'Credentials' })).toBeVisible()
  await page.getByRole('button', { name: 'Create', exact: true }).click()
  await page.getByLabel('Name').fill('Acceptance key')
  await page.getByLabel('User').selectOption({ label: 'Acceptance User' })
  await page.locator('form.dialog').getByRole('button', { name: 'Create', exact: true }).click()
  await expect(page.getByRole('heading', { name: 'One-time credential' })).toBeVisible()
  await page.getByRole('button', { name: 'Done' }).click()
  await expect(page.getByText('Acceptance key')).toBeVisible()
  await page.getByRole('link', { name: 'Requests' }).click()
  await expect(page.getByRole('heading', { name: 'Requests' })).toBeVisible()
  await page.getByRole('link', { name: 'Usage' }).click()
  await expect(page.getByRole('heading', { name: 'Usage' })).toBeVisible()
  await page.getByRole('button', { name: 'Custom' }).click()
  await expect(page.getByLabel('From')).toBeVisible()
  await page.getByRole('link', { name: 'Models & Providers' }).click()
  await expect(page.getByRole('heading', { name: 'Models & Providers' })).toBeVisible()
  await page.getByRole('link', { name: 'Policies & Scheduler' }).click()
  await expect(page.getByRole('heading', { name: 'Policies & Scheduler' })).toBeVisible()
  await page.getByRole('link', { name: 'System Health' }).click()
  await expect(page.getByRole('heading', { name: 'System Health' })).toBeVisible()
  await page.getByRole('link', { name: 'Audit' }).click()
  await expect(page.getByRole('heading', { name: 'Audit' })).toBeVisible()
  expect(consoleErrors).toEqual([])
  expect(failedResponses).toEqual([])

  const viewer = await browser.newContext({ viewport: { width: 1024, height: 768 } })
  const viewerPage = await viewer.newPage()
  await viewerPage.goto('/admin/')
  await viewerPage.getByLabel('Login credential').fill(viewerLoginToken)
  await viewerPage.getByRole('button', { name: 'Sign in' }).click()
  await viewerPage.getByRole('link', { name: 'Credentials' }).click()
  await expect(viewerPage.getByRole('heading', { name: 'Credentials' })).toBeVisible()
  await expect(viewerPage.getByRole('button', { name: 'Create', exact: true })).toHaveCount(0)
  const denied = await request.post('/admin/v1/users', {
    headers: { authorization: `Bearer ${viewerToken}`, 'x-request-id': crypto.randomUUID() },
    data: { display_name: 'Denied' },
  })
  expect(denied.status()).toBe(403)

  const mobile = await browser.newContext({ viewport: { width: 375, height: 812 } })
  const mobilePage = await mobile.newPage()
  await mobilePage.goto('/admin/')
  await mobilePage.getByLabel('Login credential').fill(loginToken)
  await mobilePage.getByRole('button', { name: 'Sign in' }).click()
  await expect(mobilePage.getByRole('heading', { name: 'Overview' })).toBeVisible()
  expect(await mobilePage.evaluate(() => document.documentElement.scrollWidth <= document.documentElement.clientWidth)).toBe(true)
  await mobilePage.screenshot({ path: '../.artifacts/w09-mobile.png', fullPage: true })
  await desktop.close()
  await viewer.close()
  await mobile.close()
})

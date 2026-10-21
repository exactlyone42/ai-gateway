import { expect, test } from '@playwright/test'

test('incident history is visible, mutable, audited, and tenant-admin protected', async ({ browser, request }) => {
  const bootstrapToken = process.env.ADMIN_BOOTSTRAP_TOKEN
  if (!bootstrapToken) throw new Error('ADMIN_BOOTSTRAP_TOKEN is required')
  const bootstrap = await request.post('/admin/v1/bootstrap', {
    headers: { authorization: `Bearer ${bootstrapToken}`, 'x-request-id': crypto.randomUUID() },
    data: { display_name: 'Incident Owner', credential_name: 'Incident login' },
  })
  expect(bootstrap.status()).toBe(201)
  const { login_token: loginToken } = await bootstrap.json()
  const session = await request.post('/admin/v1/sessions', {
    headers: { authorization: `Bearer ${loginToken}`, 'x-request-id': crypto.randomUUID() },
  })
  const { access_token: adminToken } = await session.json()
  const viewerAccount = await request.post('/admin/v1/admin-accounts', {
    headers: { authorization: `Bearer ${adminToken}`, 'x-request-id': crypto.randomUUID() },
    data: { display_name: 'Incident Viewer', credential_name: 'Viewer login', role: 'viewer' },
  })
  const { login_token: viewerLogin } = await viewerAccount.json()
  const viewerSession = await request.post('/admin/v1/sessions', {
    headers: { authorization: `Bearer ${viewerLogin}`, 'x-request-id': crypto.randomUUID() },
  })
  const { access_token: viewerToken } = await viewerSession.json()

  const created = await request.post('/admin/v1/incidents', {
    headers: { authorization: `Bearer ${adminToken}`, 'x-request-id': crypto.randomUUID() },
    data: {
      title: 'Claude Code work produced no files', status: 'investigating', service: 'gateway',
      impact_scope: 'system', started_at: '2026-10-21T05:05:00Z', ended_at: null,
      summary: 'Gateway limits and a deployment interrupted long-running work.',
      impact: 'Ten requests timed out in queue and eleven client connections ended near 60 seconds.',
      root_cause: 'Gateway concurrency was one and deployment started with an active request.',
      temporary_fix: 'Stopped gateway-side concurrency rejection.', permanent_fix: 'Declared a 600 second idle wait.',
      prevention: 'Busy deployment deferral remains open.',
      validation_evidence: 'Four parallel requests completed; deployed version 90cb105a84a9.',
    },
  })
  expect(created.status()).toBe(201)
  expect((await request.post('/admin/v1/incidents', {
    headers: { authorization: `Bearer ${adminToken}`, 'x-request-id': crypto.randomUUID() },
    data: { title: 'Unsafe', status: 'investigating', service: 'gateway', impact_scope: 'system', started_at: new Date().toISOString(), ended_at: null, summary: 'Authorization: Bearer hidden', impact: 'none', root_cause: null, temporary_fix: null, permanent_fix: null, prevention: null, validation_evidence: null },
  })).status()).toBe(400)
  expect((await request.get('/admin/v1/incidents', { headers: { 'x-api-key': 'not-an-admin-session' } })).status()).toBe(401)
  expect((await request.post('/admin/v1/incidents', { headers: { authorization: `Bearer ${viewerToken}`, 'x-request-id': crypto.randomUUID() }, data: {} })).status()).toBe(403)

  const context = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  const page = await context.newPage()
  await page.goto('/admin/')
  await page.getByLabel('Login credential').fill(loginToken)
  await page.getByRole('button', { name: 'Sign in' }).click()
  await page.getByRole('link', { name: 'Incidents' }).click()
  await expect(page.getByRole('heading', { name: 'Incidents' })).toBeVisible()
  await page.getByLabel('Incident status').selectOption('investigating')
  await page.getByLabel('Service').fill('gateway')
  const row = page.getByRole('row').filter({ hasText: 'Claude Code work produced no files' })
  await expect(row).toBeVisible()
  await row.click()
  await expect(page.getByText('Ten requests timed out in queue')).toBeVisible()
  await expect(page.getByText('90cb105a84a9')).toBeVisible()
  await page.getByLabel('Update status').selectOption('mitigated')
  await expect(page.getByLabel('Update status')).toHaveValue('mitigated')
  await page.getByLabel('Details').fill('Parallel request verification completed.')
  await page.getByLabel('Deployment version').fill('90cb105a84a9')
  await page.getByRole('button', { name: 'Add event' }).click()
  await expect(page.getByText('Parallel request verification completed.')).toBeVisible()
  await page.screenshot({ path: '../.artifacts/incident-admin-history.png', fullPage: true })

  await context.close()
})

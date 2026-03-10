import { expect, test } from '@playwright/test';

test.beforeEach(async ({ page }) => {
  await page.addInitScript(() => {
    window.localStorage.setItem('zeroclaw_token', 'test-token');
    window.localStorage.setItem('zeroclaw:locale', 'en');
  });
});

test('dashboard end-to-end flow stays healthy', async ({ page }) => {
  await page.goto('/_app/');
  await expect(page.getByText('Electric Runtime Dashboard')).toBeVisible();

  await page.goto('/_app/agent');
  await expect(page.getByPlaceholder('Type a message...')).toBeVisible();
  await page.getByTestId('chat-input').fill('hello from e2e');
  await page.keyboard.press('Enter');
  await expect(page.getByText('Echo: hello from e2e')).toBeVisible();

  await page.goto('/_app/tools');
  await page.getByRole('button', { name: /shell/i }).first().click();
  await expect(page.getByText('Parameter Schema')).toBeVisible();

  await page.goto('/_app/cron');
  await page.getByRole('button', { name: 'Add Job' }).click();
  await page.getByPlaceholder('e.g. Daily cleanup').fill('Morning job');
  await page.getByPlaceholder('e.g. 0 0 * * * (cron expression)').fill('0 8 * * *');
  await page.getByPlaceholder('e.g. cleanup --older-than 7d').fill('sync --morning');
  await page.getByRole('button', { name: 'Add Job' }).last().click();
  await expect(page.getByText('Morning job')).toBeVisible();

  await page.goto('/_app/integrations');
  await expect(page.getByText('Discord')).toBeVisible();

  await page.goto('/_app/memory');
  await page.getByRole('button', { name: 'Add Memory' }).click();
  await page.getByPlaceholder('e.g. user_preferences').fill('favorite_editor');
  await page.getByPlaceholder('Memory content...').fill('neovim');
  await page.getByPlaceholder('e.g. preferences, context, facts').fill('preferences');
  await page.getByRole('button', { name: 'Save' }).last().click();
  await expect(page.getByText('favorite_editor')).toBeVisible();

  await page.goto('/_app/config');
  await page.getByRole('button', { name: 'Save' }).click();
  await expect(page.getByText('Configuration saved successfully.')).toBeVisible();

  await page.goto('/_app/cost');
  await expect(page.getByText('Token Statistics')).toBeVisible();
  await expect(page.getByText('anthropic/claude-sonnet-4.6')).toBeVisible();

  await page.goto('/_app/logs');
  await expect(page.getByText('Scheduler heartbeat ok.', { exact: false })).toBeVisible();
  await page.getByRole('button', { name: 'Pause' }).click();
  await expect(page.getByRole('button', { name: 'Resume' })).toBeVisible();

  await page.goto('/_app/doctor');
  await page.getByRole('button', { name: 'Run Diagnostics' }).click();
  await expect(page.getByText('Configuration looks healthy.')).toBeVisible();
  await expect(page.getByText('Webhook endpoint is not configured.')).toBeVisible();
});

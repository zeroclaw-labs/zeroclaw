import { expect, test } from '@playwright/test';

test.describe('Dashboard mobile smoke', () => {
  test.use({
    viewport: { width: 390, height: 844 },
  });

  test('renders mock dashboard and supports mobile navigation and collapsible cards', async ({ page }) => {
    await page.route('**/health', async (route) => {
      await route.fulfill({
        status: 200,
        contentType: 'application/json',
        body: JSON.stringify({
          ok: true,
          paired: true,
          require_pairing: false,
        }),
      });
    });

    await page.route('**/api/status', async (route) => {
      await route.fulfill({
        status: 200,
        contentType: 'application/json',
        body: JSON.stringify({
          provider: 'openai',
          model: 'gpt-5.2',
          temperature: 0.4,
          uptime_seconds: 68420,
          gateway_port: 42617,
          locale: 'en-US',
          memory_backend: 'sqlite',
          paired: true,
          channels: {
            telegram: true,
            discord: false,
            whatsapp: true,
            github: true,
          },
          health: {
            uptime_seconds: 68420,
            updated_at: '2026-03-02T19:34:29.678544+00:00',
            pid: 4242,
            components: {
              gateway: {
                status: 'ok',
                updated_at: '2026-03-02T19:34:29.678544+00:00',
                last_ok: '2026-03-02T19:34:29.678544+00:00',
                last_error: null,
                restart_count: 0,
              },
            },
          },
        }),
      });
    });

    await page.route('**/api/cost', async (route) => {
      await route.fulfill({
        status: 200,
        contentType: 'application/json',
        body: JSON.stringify({
          cost: {
            session_cost_usd: 0.0842,
            daily_cost_usd: 1.3026,
            monthly_cost_usd: 14.9875,
            total_tokens: 182342,
            request_count: 426,
            by_model: {
              'gpt-5.2': {
                model: 'gpt-5.2',
                cost_usd: 11.4635,
                total_tokens: 141332,
                request_count: 292,
              },
            },
          },
        }),
      });
    });

    await page.goto('/');

    await expect(page.getByText('Electric Runtime Dashboard')).toBeVisible();

    await page.getByRole('button', { name: 'Open navigation' }).click();
    await expect(page.getByRole('link', { name: 'Dashboard' })).toBeVisible();

    const closeButtons = page.getByRole('button', { name: 'Close navigation' });
    await closeButtons.first().click();

    const costPulseButton = page.getByRole('button', { name: /Cost Pulse/i });
    await expect(costPulseButton).toHaveAttribute('aria-expanded', 'true');
    await costPulseButton.click();
    await expect(costPulseButton).toHaveAttribute('aria-expanded', 'false');
  });
});

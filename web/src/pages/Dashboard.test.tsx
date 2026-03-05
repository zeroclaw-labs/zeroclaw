import { afterEach, describe, expect, it, vi } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import Dashboard from './Dashboard';
import { getCost, getStatus } from '@/lib/api';
import type { CostSummary, StatusResponse } from '@/types/api';

vi.mock('@/lib/api', () => ({
  getStatus: vi.fn(),
  getCost: vi.fn(),
}));

const mockedGetStatus = vi.mocked(getStatus);
const mockedGetCost = vi.mocked(getCost);

const statusFixture: StatusResponse = {
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
};

const costFixture: CostSummary = {
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
};

afterEach(() => {
  vi.clearAllMocks();
});

describe('Dashboard', () => {
  it('renders with API data and supports collapsing every dashboard section', async () => {
    mockedGetStatus.mockResolvedValue(statusFixture);
    mockedGetCost.mockResolvedValue(costFixture);

    render(<Dashboard />);

    expect(await screen.findByText('Electric Runtime Dashboard')).toBeInTheDocument();
    expect(await screen.findByText('openai')).toBeInTheDocument();

    const sectionButtons = [
      screen.getByRole('button', { name: /Cost Pulse/i }),
      screen.getByRole('button', { name: /Channel Activity/i }),
      screen.getByRole('button', { name: /Component Health/i }),
    ];

    for (const sectionButton of sectionButtons) {
      expect(sectionButton).toHaveAttribute('aria-expanded', 'true');
      await userEvent.click(sectionButton);
      await waitFor(() => {
        expect(sectionButton).toHaveAttribute('aria-expanded', 'false');
      });

      await userEvent.click(sectionButton);
      await waitFor(() => {
        expect(sectionButton).toHaveAttribute('aria-expanded', 'true');
      });
    }
  });
});

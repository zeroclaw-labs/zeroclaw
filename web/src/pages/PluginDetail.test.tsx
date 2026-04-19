import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import { MemoryRouter, Routes, Route } from 'react-router-dom';
import PluginDetail from './PluginDetail';
import * as api from '@/lib/api';

// Mock auth module to bypass authentication
vi.mock('@/lib/auth', () => ({
  getToken: () => 'mock-token',
  setToken: vi.fn(),
  clearToken: vi.fn(),
  isAuthenticated: () => true,
}));

// Mock API calls
vi.mock('@/lib/api', () => ({
  getPlugin: vi.fn(),
  patchPluginConfig: vi.fn(),
  removePlugin: vi.fn().mockResolvedValue({ ok: true }),
  getPublicHealth: vi.fn().mockResolvedValue({ require_pairing: false }),
  getAdminPairCode: vi.fn().mockResolvedValue({ pairing_code: null }),
}));

const mockGetPlugin = api.getPlugin as ReturnType<typeof vi.fn>;

// Mock i18n - return keys for testing
vi.mock('@/lib/i18n', () => ({
  t: (key: string) => key,
  setLocale: vi.fn(),
}));

const createMockPlugin = (overrides = {}) => ({
  name: 'test-plugin',
  version: '1.0.0',
  status: 'loaded',
  description: 'A test plugin for verification',
  tools: [
    {
      name: 'test_tool',
      description: 'A test tool',
      risk_level: 'low',
      parameters_schema: null,
    },
  ],
  capabilities: ['tool'],
  allowed_hosts: [],
  allowed_paths: {},
  config: {},
  ...overrides,
});

describe('Remove Plugin Button on PluginDetail Page - Acceptance: Remove button available on the PluginDetail page', () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it('PluginDetail page has a Remove button', async () => {
    const mockPlugin = createMockPlugin();
    mockGetPlugin.mockResolvedValue(mockPlugin);

    render(
      <MemoryRouter initialEntries={['/plugins/test-plugin']}>
        <Routes>
          <Route path="/plugins/:name" element={<PluginDetail />} />
        </Routes>
      </MemoryRouter>
    );

    // Wait for plugin to load
    await waitFor(() => {
      expect(screen.getByText('test-plugin')).toBeInTheDocument();
    });

    // Verify Remove button exists
    const removeButton = screen.getByRole('button', { name: /plugin\.remove/i });
    expect(removeButton).toBeInTheDocument();
  });

  it('Remove button has Trash icon', async () => {
    const mockPlugin = createMockPlugin();
    mockGetPlugin.mockResolvedValue(mockPlugin);

    render(
      <MemoryRouter initialEntries={['/plugins/test-plugin']}>
        <Routes>
          <Route path="/plugins/:name" element={<PluginDetail />} />
        </Routes>
      </MemoryRouter>
    );

    await waitFor(() => {
      expect(screen.getByText('test-plugin')).toBeInTheDocument();
    });

    const removeButton = screen.getByRole('button', { name: /plugin\.remove/i });
    const icon = removeButton.querySelector('svg');
    expect(icon).toBeInTheDocument();
    expect(icon).toHaveClass('lucide-trash-2');
  });

  it('Remove button is visible in the header section', async () => {
    const mockPlugin = createMockPlugin();
    mockGetPlugin.mockResolvedValue(mockPlugin);

    render(
      <MemoryRouter initialEntries={['/plugins/test-plugin']}>
        <Routes>
          <Route path="/plugins/:name" element={<PluginDetail />} />
        </Routes>
      </MemoryRouter>
    );

    await waitFor(() => {
      expect(screen.getByText('test-plugin')).toBeInTheDocument();
    });

    const removeButton = screen.getByRole('button', { name: /plugin\.remove/i });
    expect(removeButton).toBeVisible();
  });

  it('Remove button is present for both loaded and discovered plugins', async () => {
    // Test loaded plugin
    const loadedPlugin = createMockPlugin({ status: 'loaded' });
    mockGetPlugin.mockResolvedValue(loadedPlugin);

    const { unmount } = render(
      <MemoryRouter initialEntries={['/plugins/test-plugin']}>
        <Routes>
          <Route path="/plugins/:name" element={<PluginDetail />} />
        </Routes>
      </MemoryRouter>
    );

    await waitFor(() => {
      expect(screen.getByText('test-plugin')).toBeInTheDocument();
    });

    expect(screen.getByRole('button', { name: /plugin\.remove/i })).toBeInTheDocument();

    unmount();

    // Test discovered plugin
    const discoveredPlugin = createMockPlugin({ status: 'discovered' });
    mockGetPlugin.mockResolvedValue(discoveredPlugin);

    render(
      <MemoryRouter initialEntries={['/plugins/test-plugin']}>
        <Routes>
          <Route path="/plugins/:name" element={<PluginDetail />} />
        </Routes>
      </MemoryRouter>
    );

    await waitFor(() => {
      expect(screen.getByText('test-plugin')).toBeInTheDocument();
    });

    expect(screen.getByRole('button', { name: /plugin\.remove/i })).toBeInTheDocument();
  });
});

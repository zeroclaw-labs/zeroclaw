import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { MemoryRouter } from 'react-router-dom';
import { Routes, Route, Navigate } from 'react-router-dom';
import Plugins from './Plugins';
import Dashboard from './Dashboard';
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
  getPlugins: vi.fn().mockResolvedValue({ plugins: [] }),
  enablePlugin: vi.fn(),
  disablePlugin: vi.fn(),
  reloadPlugins: vi.fn().mockResolvedValue({ ok: true, total: 0 }),
  installPlugin: vi.fn().mockResolvedValue({ ok: true, plugin_name: 'test-plugin' }),
  removePlugin: vi.fn().mockResolvedValue({ ok: true }),
  getPublicHealth: vi.fn().mockResolvedValue({ require_pairing: false }),
  getAdminPairCode: vi.fn().mockResolvedValue({ pairing_code: null }),
}));

const mockGetPlugins = api.getPlugins as ReturnType<typeof vi.fn>;
const mockReloadPlugins = api.reloadPlugins as ReturnType<typeof vi.fn>;

// Mock i18n - return keys with placeholders intact for keys that use .replace()
vi.mock('@/lib/i18n', () => ({
  t: (key: string) => {
    if (key === 'plugin.install_success') return 'Plugin {name} installed successfully';
    if (key === 'plugin.remove_message') return 'Are you sure you want to remove {name}? This action cannot be undone.';
    return key;
  },
  setLocale: vi.fn(),
}));

describe('Plugins Route', () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it('renders Plugins page at /plugins route without redirecting to dashboard', async () => {
    render(
      <MemoryRouter initialEntries={['/plugins']}>
        <Routes>
          <Route path="/" element={<Dashboard />} />
          <Route path="/plugins" element={<Plugins />} />
          <Route path="*" element={<Navigate to="/" replace />} />
        </Routes>
      </MemoryRouter>
    );

    // Wait for loading to complete and verify Plugins page renders
    await waitFor(() => {
      // The Plugins page shows "plugin.title" (the i18n key) when rendered
      expect(screen.getByText(/plugin\.title/)).toBeInTheDocument();
    });

    // Verify we're NOT on the dashboard (would have "dashboard" related text)
    expect(screen.queryByText(/dashboard\.title/)).not.toBeInTheDocument();
  });

  it('/plugins route is defined and accessible', async () => {
    const { container } = render(
      <MemoryRouter initialEntries={['/plugins']}>
        <Routes>
          <Route path="/" element={<div data-testid="dashboard">Dashboard</div>} />
          <Route path="/plugins" element={<Plugins />} />
          <Route path="*" element={<Navigate to="/" replace />} />
        </Routes>
      </MemoryRouter>
    );

    await waitFor(() => {
      // Should NOT have redirected to dashboard
      expect(screen.queryByTestId('dashboard')).not.toBeInTheDocument();
    });

    // Should render Plugins content
    await waitFor(() => {
      expect(container.querySelector('.animate-fade-in, .animate-spin')).toBeInTheDocument();
    });
  });
});

describe('Plugins Header Reload Button', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mockGetPlugins.mockResolvedValue({ plugins: [] });
  });

  it('reload button is visible in the Plugins page header', async () => {
    render(
      <MemoryRouter initialEntries={['/plugins']}>
        <Routes>
          <Route path="/plugins" element={<Plugins />} />
        </Routes>
      </MemoryRouter>
    );

    // Wait for loading to complete
    await waitFor(() => {
      expect(screen.getByText(/plugin\.title/)).toBeInTheDocument();
    });

    // Verify reload button is visible in header
    const reloadButton = screen.getByRole('button', { name: /plugin\.reload/ });
    expect(reloadButton).toBeInTheDocument();
    expect(reloadButton).toBeVisible();
  });

  it('button shows loading state during reload', async () => {
    const user = userEvent.setup();

    // Create a deferred promise so we can control when reloadPlugins resolves
    let resolveReload: (value: { ok: boolean; total: number }) => void;
    const reloadPromise = new Promise<{ ok: boolean; total: number }>((resolve) => {
      resolveReload = resolve;
    });
    mockReloadPlugins.mockReturnValueOnce(reloadPromise);

    render(
      <MemoryRouter initialEntries={['/plugins']}>
        <Routes>
          <Route path="/plugins" element={<Plugins />} />
        </Routes>
      </MemoryRouter>
    );

    // Wait for loading to complete
    await waitFor(() => {
      expect(screen.getByText(/plugin\.title/)).toBeInTheDocument();
    });

    const reloadButton = screen.getByRole('button', { name: /plugin\.reload/ });

    // Verify button is initially enabled and icon is NOT spinning
    expect(reloadButton).not.toBeDisabled();
    const iconBefore = reloadButton.querySelector('svg');
    expect(iconBefore).not.toHaveClass('animate-spin');

    // Click the reload button
    await user.click(reloadButton);

    // Button should now be disabled with spinning icon
    expect(reloadButton).toBeDisabled();
    const iconDuring = reloadButton.querySelector('svg');
    expect(iconDuring).toHaveClass('animate-spin');

    // Resolve the reload promise
    resolveReload!({ ok: true, total: 0 });

    // Wait for loading state to clear
    await waitFor(() => {
      expect(reloadButton).not.toBeDisabled();
    });

    // Icon should stop spinning after reload completes
    const iconAfter = reloadButton.querySelector('svg');
    expect(iconAfter).not.toHaveClass('animate-spin');
  });
});

describe('Install Plugin Button Visibility', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mockGetPlugins.mockResolvedValue({ plugins: [] });
  });

  it('Install Plugin button is visible on Plugins page', async () => {
    render(
      <MemoryRouter initialEntries={['/plugins']}>
        <Routes>
          <Route path="/plugins" element={<Plugins />} />
        </Routes>
      </MemoryRouter>
    );

    // Wait for loading to complete
    await waitFor(() => {
      expect(screen.getByText(/plugin\.title/)).toBeInTheDocument();
    });

    // Verify Install Plugin button is visible in header
    const installButton = screen.getByRole('button', { name: /plugin\.install/ });
    expect(installButton).toBeInTheDocument();
    expect(installButton).toBeVisible();
  });

  it('Install Plugin button has Plus icon', async () => {
    render(
      <MemoryRouter initialEntries={['/plugins']}>
        <Routes>
          <Route path="/plugins" element={<Plugins />} />
        </Routes>
      </MemoryRouter>
    );

    await waitFor(() => {
      expect(screen.getByText(/plugin\.title/)).toBeInTheDocument();
    });

    const installButton = screen.getByRole('button', { name: /plugin\.install/ });
    const icon = installButton.querySelector('svg');
    expect(icon).toBeInTheDocument();
    expect(icon).toHaveClass('lucide-plus');
  });

  it('Install Plugin button has accent styling', async () => {
    render(
      <MemoryRouter initialEntries={['/plugins']}>
        <Routes>
          <Route path="/plugins" element={<Plugins />} />
        </Routes>
      </MemoryRouter>
    );

    await waitFor(() => {
      expect(screen.getByText(/plugin\.title/)).toBeInTheDocument();
    });

    const installButton = screen.getByRole('button', { name: /plugin\.install/ });
    expect(installButton).toHaveStyle({ background: 'var(--pc-accent)' });
  });
});

describe('Install Modal Source Input - Acceptance: Modal/dialog accepts source path or URL', () => {
  const mockInstallPlugin = vi.fn();

  beforeEach(() => {
    vi.clearAllMocks();
    mockGetPlugins.mockResolvedValue({ plugins: [] });
    vi.mocked(api).installPlugin = mockInstallPlugin;
    mockInstallPlugin.mockResolvedValue({ ok: true, plugin_name: 'test-plugin' });
  });

  it('modal has a source input field that accepts user input', async () => {
    const user = userEvent.setup();

    render(
      <MemoryRouter initialEntries={['/plugins']}>
        <Routes>
          <Route path="/plugins" element={<Plugins />} />
        </Routes>
      </MemoryRouter>
    );

    await waitFor(() => {
      expect(screen.getByText(/plugin\.title/)).toBeInTheDocument();
    });

    // Open the install modal
    const installButton = screen.getByRole('button', { name: /plugin\.install/i });
    await user.click(installButton);

    // Verify modal has source input field
    await waitFor(() => {
      expect(screen.getByPlaceholderText(/plugin\.install_source_placeholder/i)).toBeInTheDocument();
    });

    // Verify input is an editable text field
    const sourceInput = screen.getByPlaceholderText(/plugin\.install_source_placeholder/i);
    expect(sourceInput).toHaveAttribute('type', 'text');
    expect(sourceInput).toBeEnabled();
  });

  it('accepts a local file path as source input', async () => {
    const user = userEvent.setup();
    const localPath = '/home/user/plugins/my-awesome-plugin';

    render(
      <MemoryRouter initialEntries={['/plugins']}>
        <Routes>
          <Route path="/plugins" element={<Plugins />} />
        </Routes>
      </MemoryRouter>
    );

    await waitFor(() => {
      expect(screen.getByText(/plugin\.title/)).toBeInTheDocument();
    });

    // Open install modal
    const installButton = screen.getByRole('button', { name: /plugin\.install/i });
    await user.click(installButton);

    await waitFor(() => {
      expect(screen.getByPlaceholderText(/plugin\.install_source_placeholder/i)).toBeInTheDocument();
    });

    // Enter a local file path
    const sourceInput = screen.getByPlaceholderText(/plugin\.install_source_placeholder/i);
    await user.type(sourceInput, localPath);

    // Verify the input holds the path value
    expect(sourceInput).toHaveValue(localPath);

    // Submit and verify the API receives the path
    const confirmButton = screen.getByRole('button', { name: /plugin\.install_confirm/i });
    await user.click(confirmButton);

    await waitFor(() => {
      expect(mockInstallPlugin).toHaveBeenCalledWith(localPath);
    });
  });

  it('accepts a URL as source input', async () => {
    const user = userEvent.setup();
    const pluginUrl = 'https://github.com/user/my-plugin/releases/download/v1.0/plugin.wasm';

    render(
      <MemoryRouter initialEntries={['/plugins']}>
        <Routes>
          <Route path="/plugins" element={<Plugins />} />
        </Routes>
      </MemoryRouter>
    );

    await waitFor(() => {
      expect(screen.getByText(/plugin\.title/)).toBeInTheDocument();
    });

    // Open install modal
    const installButton = screen.getByRole('button', { name: /plugin\.install/i });
    await user.click(installButton);

    await waitFor(() => {
      expect(screen.getByPlaceholderText(/plugin\.install_source_placeholder/i)).toBeInTheDocument();
    });

    // Enter a URL
    const sourceInput = screen.getByPlaceholderText(/plugin\.install_source_placeholder/i);
    await user.type(sourceInput, pluginUrl);

    // Verify the input holds the URL value
    expect(sourceInput).toHaveValue(pluginUrl);

    // Submit and verify the API receives the URL
    const confirmButton = screen.getByRole('button', { name: /plugin\.install_confirm/i });
    await user.click(confirmButton);

    await waitFor(() => {
      expect(mockInstallPlugin).toHaveBeenCalledWith(pluginUrl);
    });
  });

  it('source input shows label and hint text for path or URL', async () => {
    const user = userEvent.setup();

    render(
      <MemoryRouter initialEntries={['/plugins']}>
        <Routes>
          <Route path="/plugins" element={<Plugins />} />
        </Routes>
      </MemoryRouter>
    );

    await waitFor(() => {
      expect(screen.getByText(/plugin\.title/)).toBeInTheDocument();
    });

    // Open install modal
    const installButton = screen.getByRole('button', { name: /plugin\.install/i });
    await user.click(installButton);

    // Verify label and hint text are displayed
    await waitFor(() => {
      expect(screen.getByText(/plugin\.install_source_label/i)).toBeInTheDocument();
    });

    expect(screen.getByText(/plugin\.install_source_hint/i)).toBeInTheDocument();
  });
});

describe('Plugins Reload Auto-Refresh', () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it('plugin list refreshes automatically after reload completes', async () => {
    const user = userEvent.setup();

    // Initial plugins before reload
    const initialPlugins = [
      {
        name: 'old-plugin',
        version: '1.0.0',
        status: 'loaded' as const,
        tools: [],
        capabilities: [],
        description: 'Old plugin before reload',
      },
    ];

    // Updated plugins after reload (simulates newly discovered plugin)
    const updatedPlugins = [
      {
        name: 'old-plugin',
        version: '1.0.0',
        status: 'loaded' as const,
        tools: [],
        capabilities: [],
        description: 'Old plugin before reload',
      },
      {
        name: 'new-plugin',
        version: '2.0.0',
        status: 'discovered' as const,
        tools: [{ name: 'new-tool' }],
        capabilities: ['cli'],
        description: 'New plugin discovered after reload',
      },
    ];

    // First call returns initial plugins, second call (after reload) returns updated list
    mockGetPlugins
      .mockResolvedValueOnce({ plugins: initialPlugins })
      .mockResolvedValueOnce({ plugins: updatedPlugins });

    mockReloadPlugins.mockResolvedValueOnce({ ok: true, total: 2 });

    render(
      <MemoryRouter initialEntries={['/plugins']}>
        <Routes>
          <Route path="/plugins" element={<Plugins />} />
        </Routes>
      </MemoryRouter>
    );

    // Wait for initial plugins to load
    await waitFor(() => {
      expect(screen.getByText('old-plugin')).toBeInTheDocument();
    });

    // Verify new plugin is NOT yet visible
    expect(screen.queryByText('new-plugin')).not.toBeInTheDocument();

    // Verify initial plugin count
    expect(screen.getByText(/plugin\.title.*\(1\)/)).toBeInTheDocument();

    // Click the reload button
    const reloadButton = screen.getByRole('button', { name: /plugin\.reload/ });
    await user.click(reloadButton);

    // Wait for reload to complete and verify plugin list is refreshed
    await waitFor(() => {
      // New plugin should now be visible
      expect(screen.getByText('new-plugin')).toBeInTheDocument();
    });

    // Verify both plugins are now displayed
    expect(screen.getByText('old-plugin')).toBeInTheDocument();
    expect(screen.getByText('new-plugin')).toBeInTheDocument();

    // Verify plugin count updated
    expect(screen.getByText(/plugin\.title.*\(2\)/)).toBeInTheDocument();

    // Verify getPlugins was called twice: initial load + refresh after reload
    expect(mockGetPlugins).toHaveBeenCalledTimes(2);
  });

  it('plugin list does not refresh when reload fails', async () => {
    const user = userEvent.setup();

    const initialPlugins = [
      {
        name: 'existing-plugin',
        version: '1.0.0',
        status: 'loaded' as const,
        tools: [],
        capabilities: [],
        description: 'Existing plugin',
      },
    ];

    mockGetPlugins.mockResolvedValue({ plugins: initialPlugins });
    mockReloadPlugins.mockResolvedValueOnce({ ok: false, error: 'Reload failed' });

    render(
      <MemoryRouter initialEntries={['/plugins']}>
        <Routes>
          <Route path="/plugins" element={<Plugins />} />
        </Routes>
      </MemoryRouter>
    );

    // Wait for initial plugins to load
    await waitFor(() => {
      expect(screen.getByText('existing-plugin')).toBeInTheDocument();
    });

    // Click reload button
    const reloadButton = screen.getByRole('button', { name: /plugin\.reload/ });
    await user.click(reloadButton);

    // Wait for reload to finish (error notification appears)
    await waitFor(() => {
      expect(screen.getByText('Reload failed')).toBeInTheDocument();
    });

    // getPlugins should only have been called once (initial load)
    // NOT called again because reload failed
    expect(mockGetPlugins).toHaveBeenCalledTimes(1);
  });
});

describe('Plugins Toast Notifications', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mockGetPlugins.mockResolvedValue({ plugins: [] });
  });

  it('shows success toast notification after successful reload', async () => {
    const user = userEvent.setup();
    mockReloadPlugins.mockResolvedValueOnce({ ok: true, total: 5 });

    render(
      <MemoryRouter initialEntries={['/plugins']}>
        <Routes>
          <Route path="/plugins" element={<Plugins />} />
        </Routes>
      </MemoryRouter>
    );

    // Wait for initial load
    await waitFor(() => {
      expect(screen.getByText(/plugin\.title/)).toBeInTheDocument();
    });

    // Verify no notification initially
    expect(screen.queryByText(/plugin\.reload_success/)).not.toBeInTheDocument();

    // Click reload button
    const reloadButton = screen.getByRole('button', { name: /plugin\.reload/ });
    await user.click(reloadButton);

    // Wait for success notification to appear
    await waitFor(() => {
      expect(screen.getByText(/plugin\.reload_success/)).toBeInTheDocument();
    });

    // Verify success notification has correct styling (green background)
    const notification = screen.getByText(/plugin\.reload_success/).closest('div');
    expect(notification).toHaveStyle({ color: '#00e68a' });
  });

  it('shows error toast notification when reload returns ok: false', async () => {
    const user = userEvent.setup();
    mockReloadPlugins.mockResolvedValueOnce({ ok: false, error: 'Plugin registry unavailable' });

    render(
      <MemoryRouter initialEntries={['/plugins']}>
        <Routes>
          <Route path="/plugins" element={<Plugins />} />
        </Routes>
      </MemoryRouter>
    );

    // Wait for initial load
    await waitFor(() => {
      expect(screen.getByText(/plugin\.title/)).toBeInTheDocument();
    });

    // Click reload button
    const reloadButton = screen.getByRole('button', { name: /plugin\.reload/ });
    await user.click(reloadButton);

    // Wait for error notification to appear
    await waitFor(() => {
      expect(screen.getByText('Plugin registry unavailable')).toBeInTheDocument();
    });

    // Verify error notification has correct styling (red background)
    const notification = screen.getByText('Plugin registry unavailable').closest('div');
    expect(notification).toHaveStyle({ color: '#f87171' });
  });

  it('shows error toast notification when reload throws exception', async () => {
    const user = userEvent.setup();
    mockReloadPlugins.mockRejectedValueOnce(new Error('Network connection failed'));

    render(
      <MemoryRouter initialEntries={['/plugins']}>
        <Routes>
          <Route path="/plugins" element={<Plugins />} />
        </Routes>
      </MemoryRouter>
    );

    // Wait for initial load
    await waitFor(() => {
      expect(screen.getByText(/plugin\.title/)).toBeInTheDocument();
    });

    // Click reload button
    const reloadButton = screen.getByRole('button', { name: /plugin\.reload/ });
    await user.click(reloadButton);

    // Wait for error notification to appear with exception message
    await waitFor(() => {
      expect(screen.getByText('Network connection failed')).toBeInTheDocument();
    });

    // Verify error notification has correct styling (red background)
    const notification = screen.getByText('Network connection failed').closest('div');
    expect(notification).toHaveStyle({ color: '#f87171' });
  });

  it('success notification includes check icon', async () => {
    const user = userEvent.setup();
    mockReloadPlugins.mockResolvedValueOnce({ ok: true, total: 3 });

    render(
      <MemoryRouter initialEntries={['/plugins']}>
        <Routes>
          <Route path="/plugins" element={<Plugins />} />
        </Routes>
      </MemoryRouter>
    );

    // Wait for initial load
    await waitFor(() => {
      expect(screen.getByText(/plugin\.title/)).toBeInTheDocument();
    });

    // Click reload button
    const reloadButton = screen.getByRole('button', { name: /plugin\.reload/ });
    await user.click(reloadButton);

    // Wait for success notification
    await waitFor(() => {
      expect(screen.getByText(/plugin\.reload_success/)).toBeInTheDocument();
    });

    // Success notification should contain a check icon (svg element)
    const notification = screen.getByText(/plugin\.reload_success/).closest('div');
    expect(notification?.querySelector('svg')).toBeInTheDocument();
  });
});

describe('Plugin Card Display', () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it('each plugin shows name version status tool-count capabilities and enable/disable toggle', async () => {
    const mockPlugins = [
      {
        name: 'weather-plugin',
        version: '3.2.1',
        status: 'loaded' as const,
        tools: [{ name: 'get_weather' }, { name: 'get_forecast' }, { name: 'set_location' }],
        capabilities: ['cli', 'memory'],
        description: 'Weather information plugin',
      },
      {
        name: 'calendar-plugin',
        version: '1.0.0',
        status: 'discovered' as const,
        tools: [{ name: 'add_event' }],
        capabilities: ['tools'],
        description: 'Calendar management plugin',
      },
    ];

    mockGetPlugins.mockResolvedValueOnce({ plugins: mockPlugins });

    render(
      <MemoryRouter initialEntries={['/plugins']}>
        <Routes>
          <Route path="/plugins" element={<Plugins />} />
        </Routes>
      </MemoryRouter>
    );

    // Wait for plugins to load
    await waitFor(() => {
      expect(screen.getByText('weather-plugin')).toBeInTheDocument();
    });

    // --- Verify all plugins display their names ---
    expect(screen.getByText('weather-plugin')).toBeInTheDocument();
    expect(screen.getByText('calendar-plugin')).toBeInTheDocument();

    // --- Verify versions are displayed ---
    expect(screen.getByText('v3.2.1')).toBeInTheDocument();
    expect(screen.getByText('v1.0.0')).toBeInTheDocument();

    // --- Verify status badges are displayed ---
    // One plugin is loaded, one is discovered
    expect(screen.getByText('plugin.status_loaded')).toBeInTheDocument();
    expect(screen.getByText('plugin.status_discovered')).toBeInTheDocument();

    // --- Verify tool counts are displayed (one per plugin card) ---
    const toolCountElements = screen.getAllByText('plugin.tool_count');
    expect(toolCountElements).toHaveLength(2);

    // --- Verify capabilities are displayed ---
    expect(screen.getByText('cli')).toBeInTheDocument();
    expect(screen.getByText('memory')).toBeInTheDocument();
    expect(screen.getByText('tools')).toBeInTheDocument();

    // --- Verify enable/disable toggles exist for each plugin ---
    // Toggle buttons have titles based on status
    const disableToggle = screen.getByTitle('plugin.disable');
    const enableToggle = screen.getByTitle('plugin.enable');
    expect(disableToggle).toBeInTheDocument();
    expect(enableToggle).toBeInTheDocument();

    // Verify toggles are actual buttons
    expect(disableToggle.tagName).toBe('BUTTON');
    expect(enableToggle.tagName).toBe('BUTTON');
  });
});

describe('Plugin Name Links', () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it('plugin name links to /plugins/:name detail page', async () => {
    const mockPlugins = [
      {
        name: 'weather-plugin',
        version: '1.0.0',
        status: 'loaded' as const,
        tools: [{ name: 'get_weather' }],
        capabilities: ['cli'],
        description: 'Weather info plugin',
      },
      {
        name: 'my-special-plugin',
        version: '2.0.0',
        status: 'discovered' as const,
        tools: [],
        capabilities: [],
        description: 'Another plugin',
      },
    ];

    mockGetPlugins.mockResolvedValueOnce({ plugins: mockPlugins });

    render(
      <MemoryRouter initialEntries={['/plugins']}>
        <Routes>
          <Route path="/plugins" element={<Plugins />} />
          <Route path="/plugins/:name" element={<div data-testid="plugin-detail">Plugin Detail</div>} />
        </Routes>
      </MemoryRouter>
    );

    // Wait for plugins to load
    await waitFor(() => {
      expect(screen.getByText('weather-plugin')).toBeInTheDocument();
    });

    // Verify each plugin name is a link
    const weatherLink = screen.getByText('weather-plugin').closest('a');
    const specialLink = screen.getByText('my-special-plugin').closest('a');

    expect(weatherLink).toBeInTheDocument();
    expect(specialLink).toBeInTheDocument();

    // Verify links point to correct detail page paths
    expect(weatherLink).toHaveAttribute('href', '/plugins/weather-plugin');
    expect(specialLink).toHaveAttribute('href', '/plugins/my-special-plugin');
  });

  it('plugin name link with special characters is properly encoded', async () => {
    const mockPlugins = [
      {
        name: 'plugin/with/slashes',
        version: '1.0.0',
        status: 'loaded' as const,
        tools: [],
        capabilities: [],
        description: 'Plugin with special chars in name',
      },
    ];

    mockGetPlugins.mockResolvedValueOnce({ plugins: mockPlugins });

    render(
      <MemoryRouter initialEntries={['/plugins']}>
        <Routes>
          <Route path="/plugins" element={<Plugins />} />
        </Routes>
      </MemoryRouter>
    );

    // Wait for plugin to load
    await waitFor(() => {
      expect(screen.getByText('plugin/with/slashes')).toBeInTheDocument();
    });

    // Verify link is properly URL-encoded
    const link = screen.getByText('plugin/with/slashes').closest('a');
    expect(link).toHaveAttribute('href', '/plugins/plugin%2Fwith%2Fslashes');
  });

  it('clicking plugin name navigates to detail page', async () => {
    const user = userEvent.setup();

    const mockPlugins = [
      {
        name: 'nav-test-plugin',
        version: '1.0.0',
        status: 'loaded' as const,
        tools: [],
        capabilities: [],
        description: 'Plugin for testing navigation',
      },
    ];

    mockGetPlugins.mockResolvedValueOnce({ plugins: mockPlugins });

    render(
      <MemoryRouter initialEntries={['/plugins']}>
        <Routes>
          <Route path="/plugins" element={<Plugins />} />
          <Route path="/plugins/:name" element={<div data-testid="plugin-detail">Plugin Detail Page</div>} />
        </Routes>
      </MemoryRouter>
    );

    // Wait for plugin to load
    await waitFor(() => {
      expect(screen.getByText('nav-test-plugin')).toBeInTheDocument();
    });

    // Click the plugin name link
    const pluginLink = screen.getByText('nav-test-plugin').closest('a');
    await user.click(pluginLink!);

    // Verify navigation to detail page
    await waitFor(() => {
      expect(screen.getByTestId('plugin-detail')).toBeInTheDocument();
      expect(screen.getByText('Plugin Detail Page')).toBeInTheDocument();
    });

    // Verify we're no longer on the plugins list page
    expect(screen.queryByText(/plugin\.title/)).not.toBeInTheDocument();
  });
});

describe('Search/Filter Bar', () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it('filters plugins by name', async () => {
    const user = userEvent.setup();

    const mockPlugins = [
      {
        name: 'weather-plugin',
        version: '1.0.0',
        status: 'loaded' as const,
        tools: [],
        capabilities: [],
        description: 'Weather info',
      },
      {
        name: 'calendar-plugin',
        version: '1.0.0',
        status: 'loaded' as const,
        tools: [],
        capabilities: [],
        description: 'Calendar management',
      },
      {
        name: 'notes-plugin',
        version: '1.0.0',
        status: 'loaded' as const,
        tools: [],
        capabilities: [],
        description: 'Note taking',
      },
    ];

    mockGetPlugins.mockResolvedValueOnce({ plugins: mockPlugins });

    render(
      <MemoryRouter initialEntries={['/plugins']}>
        <Routes>
          <Route path="/plugins" element={<Plugins />} />
        </Routes>
      </MemoryRouter>
    );

    // Wait for plugins to load
    await waitFor(() => {
      expect(screen.getByText('weather-plugin')).toBeInTheDocument();
    });

    // All plugins should be visible initially
    expect(screen.getByText('weather-plugin')).toBeInTheDocument();
    expect(screen.getByText('calendar-plugin')).toBeInTheDocument();
    expect(screen.getByText('notes-plugin')).toBeInTheDocument();

    // Type in the search bar
    const searchInput = screen.getByPlaceholderText('plugin.search_placeholder');
    await user.type(searchInput, 'weather');

    // Only weather-plugin should be visible
    expect(screen.getByText('weather-plugin')).toBeInTheDocument();
    expect(screen.queryByText('calendar-plugin')).not.toBeInTheDocument();
    expect(screen.queryByText('notes-plugin')).not.toBeInTheDocument();
  });

  it('filters plugins by capability', async () => {
    const user = userEvent.setup();

    const mockPlugins = [
      {
        name: 'cli-tool',
        version: '1.0.0',
        status: 'loaded' as const,
        tools: [],
        capabilities: ['cli', 'tools'],
        description: 'CLI plugin',
      },
      {
        name: 'memory-tool',
        version: '1.0.0',
        status: 'loaded' as const,
        tools: [],
        capabilities: ['memory'],
        description: 'Memory plugin',
      },
      {
        name: 'full-featured',
        version: '1.0.0',
        status: 'loaded' as const,
        tools: [],
        capabilities: ['cli', 'memory', 'tools'],
        description: 'Full featured plugin',
      },
    ];

    mockGetPlugins.mockResolvedValueOnce({ plugins: mockPlugins });

    render(
      <MemoryRouter initialEntries={['/plugins']}>
        <Routes>
          <Route path="/plugins" element={<Plugins />} />
        </Routes>
      </MemoryRouter>
    );

    // Wait for plugins to load
    await waitFor(() => {
      expect(screen.getByText('cli-tool')).toBeInTheDocument();
    });

    // All plugins should be visible initially
    expect(screen.getByText('cli-tool')).toBeInTheDocument();
    expect(screen.getByText('memory-tool')).toBeInTheDocument();
    expect(screen.getByText('full-featured')).toBeInTheDocument();

    // Search by capability "memory"
    const searchInput = screen.getByPlaceholderText('plugin.search_placeholder');
    await user.type(searchInput, 'memory');

    // Only plugins with "memory" capability should be visible
    expect(screen.queryByText('cli-tool')).not.toBeInTheDocument();
    expect(screen.getByText('memory-tool')).toBeInTheDocument();
    expect(screen.getByText('full-featured')).toBeInTheDocument();
  });

  it('search is case-insensitive', async () => {
    const user = userEvent.setup();

    const mockPlugins = [
      {
        name: 'WeatherPlugin',
        version: '1.0.0',
        status: 'loaded' as const,
        tools: [],
        capabilities: ['CLI'],
        description: 'Weather plugin',
      },
    ];

    mockGetPlugins.mockResolvedValueOnce({ plugins: mockPlugins });

    render(
      <MemoryRouter initialEntries={['/plugins']}>
        <Routes>
          <Route path="/plugins" element={<Plugins />} />
        </Routes>
      </MemoryRouter>
    );

    await waitFor(() => {
      expect(screen.getByText('WeatherPlugin')).toBeInTheDocument();
    });

    const searchInput = screen.getByPlaceholderText('plugin.search_placeholder');

    // Search with different cases should all find the plugin
    await user.type(searchInput, 'WEATHER');
    expect(screen.getByText('WeatherPlugin')).toBeInTheDocument();

    await user.clear(searchInput);
    await user.type(searchInput, 'cli');
    expect(screen.getByText('WeatherPlugin')).toBeInTheDocument();
  });

  it('shows no results state when search does not match any plugins', async () => {
    const user = userEvent.setup();

    const mockPlugins = [
      {
        name: 'weather-plugin',
        version: '1.0.0',
        status: 'loaded' as const,
        tools: [],
        capabilities: ['cli'],
        description: 'Weather info',
      },
    ];

    mockGetPlugins.mockResolvedValueOnce({ plugins: mockPlugins });

    render(
      <MemoryRouter initialEntries={['/plugins']}>
        <Routes>
          <Route path="/plugins" element={<Plugins />} />
        </Routes>
      </MemoryRouter>
    );

    await waitFor(() => {
      expect(screen.getByText('weather-plugin')).toBeInTheDocument();
    });

    // Type a query that won't match anything
    const searchInput = screen.getByPlaceholderText('plugin.search_placeholder');
    await user.type(searchInput, 'nonexistent');

    // Plugin should not be visible
    expect(screen.queryByText('weather-plugin')).not.toBeInTheDocument();

    // No results state should be shown
    expect(screen.getByText('plugin.no_results')).toBeInTheDocument();
  });

  it('clearing search shows all plugins again', async () => {
    const user = userEvent.setup();

    const mockPlugins = [
      {
        name: 'plugin-a',
        version: '1.0.0',
        status: 'loaded' as const,
        tools: [],
        capabilities: [],
        description: 'Plugin A',
      },
      {
        name: 'plugin-b',
        version: '1.0.0',
        status: 'loaded' as const,
        tools: [],
        capabilities: [],
        description: 'Plugin B',
      },
    ];

    mockGetPlugins.mockResolvedValueOnce({ plugins: mockPlugins });

    render(
      <MemoryRouter initialEntries={['/plugins']}>
        <Routes>
          <Route path="/plugins" element={<Plugins />} />
        </Routes>
      </MemoryRouter>
    );

    await waitFor(() => {
      expect(screen.getByText('plugin-a')).toBeInTheDocument();
    });

    const searchInput = screen.getByPlaceholderText('plugin.search_placeholder');

    // Filter to show only plugin-a
    await user.type(searchInput, 'plugin-a');
    expect(screen.getByText('plugin-a')).toBeInTheDocument();
    expect(screen.queryByText('plugin-b')).not.toBeInTheDocument();

    // Clear the search
    await user.clear(searchInput);

    // Both plugins should be visible again
    expect(screen.getByText('plugin-a')).toBeInTheDocument();
    expect(screen.getByText('plugin-b')).toBeInTheDocument();
  });
});

describe('Empty State', () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it('shows empty state when no plugins are installed', async () => {
    mockGetPlugins.mockResolvedValueOnce({ plugins: [] });

    render(
      <MemoryRouter initialEntries={['/plugins']}>
        <Routes>
          <Route path="/plugins" element={<Plugins />} />
        </Routes>
      </MemoryRouter>
    );

    // Wait for loading to complete
    await waitFor(() => {
      expect(screen.getByText(/plugin\.title/)).toBeInTheDocument();
    });

    // Verify empty state message is displayed
    expect(screen.getByText('plugin.empty')).toBeInTheDocument();

    // Verify the search bar is NOT shown when there are no plugins
    expect(screen.queryByPlaceholderText('plugin.search_placeholder')).not.toBeInTheDocument();

    // Verify empty state has the Blocks icon (rendered inside .card container)
    const emptyStateCard = screen.getByText('plugin.empty').closest('.card');
    expect(emptyStateCard).toBeInTheDocument();
    expect(emptyStateCard?.querySelector('svg')).toBeInTheDocument();
  });

  it('shows plugins count as zero in header when no plugins', async () => {
    mockGetPlugins.mockResolvedValueOnce({ plugins: [] });

    render(
      <MemoryRouter initialEntries={['/plugins']}>
        <Routes>
          <Route path="/plugins" element={<Plugins />} />
        </Routes>
      </MemoryRouter>
    );

    // Wait for loading to complete
    await waitFor(() => {
      expect(screen.getByText(/plugin\.title/)).toBeInTheDocument();
    });

    // Verify plugin count shows (0)
    expect(screen.getByText(/plugin\.title.*\(0\)/)).toBeInTheDocument();
  });
});

describe('Loading State', () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it('shows loading spinner while fetching plugins', async () => {
    // Create a deferred promise to control when getPlugins resolves
    let resolvePlugins: (value: { plugins: Plugin[] }) => void;
    const pluginsPromise = new Promise<{ plugins: Plugin[] }>((resolve) => {
      resolvePlugins = resolve;
    });
    mockGetPlugins.mockReturnValueOnce(pluginsPromise);

    const { container } = render(
      <MemoryRouter initialEntries={['/plugins']}>
        <Routes>
          <Route path="/plugins" element={<Plugins />} />
        </Routes>
      </MemoryRouter>
    );

    // Loading spinner should be visible (animate-spin class on the border div)
    const spinner = container.querySelector('.animate-spin');
    expect(spinner).toBeInTheDocument();

    // Plugin content should NOT be visible yet
    expect(screen.queryByText(/plugin\.title/)).not.toBeInTheDocument();

    // Resolve the promise
    resolvePlugins!({ plugins: [] });

    // Wait for loading to complete
    await waitFor(() => {
      expect(screen.getByText(/plugin\.title/)).toBeInTheDocument();
    });

    // Spinner should no longer be visible
    expect(container.querySelector('.h-8.w-8.animate-spin')).not.toBeInTheDocument();
  });

  it('loading spinner is centered in viewport', async () => {
    let resolvePlugins: (value: { plugins: Plugin[] }) => void;
    const pluginsPromise = new Promise<{ plugins: Plugin[] }>((resolve) => {
      resolvePlugins = resolve;
    });
    mockGetPlugins.mockReturnValueOnce(pluginsPromise);

    const { container } = render(
      <MemoryRouter initialEntries={['/plugins']}>
        <Routes>
          <Route path="/plugins" element={<Plugins />} />
        </Routes>
      </MemoryRouter>
    );

    // Loading container should have centering classes
    const loadingContainer = container.querySelector('.flex.items-center.justify-center');
    expect(loadingContainer).toBeInTheDocument();

    // Cleanup
    resolvePlugins!({ plugins: [] });
    await waitFor(() => {
      expect(screen.getByText(/plugin\.title/)).toBeInTheDocument();
    });
  });
});

interface Plugin {
  name: string;
  version: string;
  status: 'loaded' | 'discovered';
  tools: { name: string }[];
  capabilities: string[];
  description: string;
}

describe('Error State', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mockGetPlugins.mockReset();
    mockGetPlugins.mockResolvedValue({ plugins: [] });
  });

  it('shows error state when getPlugins() API call fails', async () => {
    mockGetPlugins.mockRejectedValueOnce(new Error('Network connection failed'));

    render(
      <MemoryRouter initialEntries={['/plugins']}>
        <Routes>
          <Route path="/plugins" element={<Plugins />} />
        </Routes>
      </MemoryRouter>
    );

    // Wait for error state to appear
    await waitFor(() => {
      expect(screen.getByText(/plugin\.load_error/)).toBeInTheDocument();
    });

    // Error message should include the actual error
    expect(screen.getByText(/Network connection failed/)).toBeInTheDocument();
  });

  it('error state has red styling', async () => {
    mockGetPlugins.mockRejectedValueOnce(new Error('API unavailable'));

    render(
      <MemoryRouter initialEntries={['/plugins']}>
        <Routes>
          <Route path="/plugins" element={<Plugins />} />
        </Routes>
      </MemoryRouter>
    );

    // Wait for error state
    await waitFor(() => {
      expect(screen.getByText(/plugin\.load_error/)).toBeInTheDocument();
    });

    // Error container should have red styling
    const errorContainer = screen.getByText(/plugin\.load_error/).closest('div');
    expect(errorContainer).toHaveStyle({ color: '#f87171' });
  });

  it('error state does not show plugins list or search bar', async () => {
    mockGetPlugins.mockRejectedValueOnce(new Error('Server error'));

    render(
      <MemoryRouter initialEntries={['/plugins']}>
        <Routes>
          <Route path="/plugins" element={<Plugins />} />
        </Routes>
      </MemoryRouter>
    );

    // Wait for error state
    await waitFor(() => {
      expect(screen.getByText(/plugin\.load_error/)).toBeInTheDocument();
    });

    // Should not show plugin title header (plugin list view)
    expect(screen.queryByText(/plugin\.title/)).not.toBeInTheDocument();

    // Should not show search bar
    expect(screen.queryByPlaceholderText('plugin.search_placeholder')).not.toBeInTheDocument();
  });

  it('shows different error messages based on the error thrown', async () => {
    // First error - mock must be set BEFORE render
    mockGetPlugins.mockRejectedValueOnce(new Error('Timeout exceeded'));

    const { unmount } = render(
      <MemoryRouter initialEntries={['/plugins']}>
        <Routes>
          <Route path="/plugins" element={<Plugins />} />
        </Routes>
      </MemoryRouter>
    );

    await waitFor(() => {
      expect(screen.getByText(/Timeout exceeded/)).toBeInTheDocument();
    });

    // Re-render with different error
    unmount();
    mockGetPlugins.mockRejectedValueOnce(new Error('Unauthorized access'));

    render(
      <MemoryRouter initialEntries={['/plugins']}>
        <Routes>
          <Route path="/plugins" element={<Plugins />} />
        </Routes>
      </MemoryRouter>
    );

    await waitFor(() => {
      expect(screen.getByText(/Unauthorized access/)).toBeInTheDocument();
    });
  });
});

describe('Plugins API Integration', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mockGetPlugins.mockReset();
    mockGetPlugins.mockResolvedValue({ plugins: [] });
  });

  it('calls getPlugins() API and displays all plugins', async () => {
    const mockPlugins = [
      {
        name: 'test-plugin-alpha',
        version: '1.0.0',
        status: 'loaded' as const,
        tools: [{ name: 'tool1' }, { name: 'tool2' }],
        capabilities: ['cli', 'memory'],
        description: 'First test plugin',
      },
      {
        name: 'test-plugin-beta',
        version: '2.5.3',
        status: 'discovered' as const,
        tools: [{ name: 'tool3' }],
        capabilities: ['tools'],
        description: 'Second test plugin',
      },
      {
        name: 'test-plugin-gamma',
        version: '0.1.0',
        status: 'loaded' as const,
        tools: [],
        capabilities: [],
        description: 'Third test plugin',
      },
    ];

    mockGetPlugins.mockResolvedValueOnce({ plugins: mockPlugins });

    render(
      <MemoryRouter initialEntries={['/plugins']}>
        <Routes>
          <Route path="/plugins" element={<Plugins />} />
        </Routes>
      </MemoryRouter>
    );

    // Verify getPlugins was called
    expect(mockGetPlugins).toHaveBeenCalledTimes(1);

    // Wait for loading to complete and verify all plugins are displayed
    await waitFor(() => {
      expect(screen.getByText('test-plugin-alpha')).toBeInTheDocument();
    });

    // Verify all plugin names are displayed
    expect(screen.getByText('test-plugin-alpha')).toBeInTheDocument();
    expect(screen.getByText('test-plugin-beta')).toBeInTheDocument();
    expect(screen.getByText('test-plugin-gamma')).toBeInTheDocument();

    // Verify versions are displayed
    expect(screen.getByText('v1.0.0')).toBeInTheDocument();
    expect(screen.getByText('v2.5.3')).toBeInTheDocument();
    expect(screen.getByText('v0.1.0')).toBeInTheDocument();

    // Verify plugin count in header shows correct total
    expect(screen.getByText(/plugin\.title.*\(3\)/)).toBeInTheDocument();
  });
});

describe('Install Plugin Progress Indicator', () => {
  const mockInstallPlugin = vi.fn();

  beforeEach(() => {
    vi.clearAllMocks();
    mockGetPlugins.mockResolvedValue({ plugins: [] });
    // Mock installPlugin API - module mock needs to be updated
    vi.mocked(api).installPlugin = mockInstallPlugin;
  });

  it('shows progress indicator while installing a plugin', async () => {
    const user = userEvent.setup();

    // Make installPlugin take some time to resolve
    let resolveInstall: (value: { ok: boolean; plugin_name: string }) => void;
    const installPromise = new Promise<{ ok: boolean; plugin_name: string }>((resolve) => {
      resolveInstall = resolve;
    });
    mockInstallPlugin.mockReturnValue(installPromise);

    render(
      <MemoryRouter initialEntries={['/plugins']}>
        <Routes>
          <Route path="/plugins" element={<Plugins />} />
        </Routes>
      </MemoryRouter>
    );

    // Wait for initial load
    await waitFor(() => {
      expect(screen.getByText(/plugin\.title/)).toBeInTheDocument();
    });

    // Click the "Install Plugin" button
    const installButton = screen.getByRole('button', { name: /plugin\.install/i });
    await user.click(installButton);

    // Modal should appear with source input
    await waitFor(() => {
      expect(screen.getByPlaceholderText(/plugin\.install_source_placeholder/i)).toBeInTheDocument();
    });

    // Enter source path
    const sourceInput = screen.getByPlaceholderText(/plugin\.install_source_placeholder/i);
    await user.type(sourceInput, '/path/to/my-plugin');

    // Click the modal's install/confirm button
    const confirmButton = screen.getByRole('button', { name: /plugin\.install_confirm/i });
    await user.click(confirmButton);

    // Progress indicator should be visible during installation
    await waitFor(() => {
      const progressIndicator = screen.getByTestId('install-progress-indicator');
      expect(progressIndicator).toBeInTheDocument();
    });

    // Progress indicator should have spinner animation
    const spinner = screen.getByTestId('install-progress-indicator');
    expect(spinner.querySelector('.animate-spin')).toBeInTheDocument();

    // Resolve the install promise
    resolveInstall!({ ok: true, plugin_name: 'my-plugin' });

    // Progress indicator should disappear after installation completes
    await waitFor(() => {
      expect(screen.queryByTestId('install-progress-indicator')).not.toBeInTheDocument();
    });
  });

  it('progress indicator is visible throughout the entire install duration', async () => {
    const user = userEvent.setup();

    // Simulate a longer installation
    let resolveInstall: (value: { ok: boolean; plugin_name: string }) => void;
    const installPromise = new Promise<{ ok: boolean; plugin_name: string }>((resolve) => {
      resolveInstall = resolve;
    });
    mockInstallPlugin.mockReturnValue(installPromise);

    render(
      <MemoryRouter initialEntries={['/plugins']}>
        <Routes>
          <Route path="/plugins" element={<Plugins />} />
        </Routes>
      </MemoryRouter>
    );

    await waitFor(() => {
      expect(screen.getByText(/plugin\.title/)).toBeInTheDocument();
    });

    // Open install modal and start install
    const installButton = screen.getByRole('button', { name: /plugin\.install/i });
    await user.click(installButton);

    await waitFor(() => {
      expect(screen.getByPlaceholderText(/plugin\.install_source_placeholder/i)).toBeInTheDocument();
    });

    const sourceInput = screen.getByPlaceholderText(/plugin\.install_source_placeholder/i);
    await user.type(sourceInput, 'https://example.com/plugin.wasm');

    const confirmButton = screen.getByRole('button', { name: /plugin\.install_confirm/i });
    await user.click(confirmButton);

    // Progress indicator should appear
    await waitFor(() => {
      expect(screen.getByTestId('install-progress-indicator')).toBeInTheDocument();
    });

    // Verify the install button is disabled during installation
    expect(confirmButton).toBeDisabled();

    // Complete installation
    resolveInstall!({ ok: true, plugin_name: 'remote-plugin' });

    await waitFor(() => {
      expect(screen.queryByTestId('install-progress-indicator')).not.toBeInTheDocument();
    });
  });

  it('progress indicator shows descriptive text during install', async () => {
    const user = userEvent.setup();

    let resolveInstall: (value: { ok: boolean; plugin_name: string }) => void;
    const installPromise = new Promise<{ ok: boolean; plugin_name: string }>((resolve) => {
      resolveInstall = resolve;
    });
    mockInstallPlugin.mockReturnValue(installPromise);

    render(
      <MemoryRouter initialEntries={['/plugins']}>
        <Routes>
          <Route path="/plugins" element={<Plugins />} />
        </Routes>
      </MemoryRouter>
    );

    await waitFor(() => {
      expect(screen.getByText(/plugin\.title/)).toBeInTheDocument();
    });

    const installButton = screen.getByRole('button', { name: /plugin\.install/i });
    await user.click(installButton);

    await waitFor(() => {
      expect(screen.getByPlaceholderText(/plugin\.install_source_placeholder/i)).toBeInTheDocument();
    });

    const sourceInput = screen.getByPlaceholderText(/plugin\.install_source_placeholder/i);
    await user.type(sourceInput, '/plugins/my-wasm-plugin');

    const confirmButton = screen.getByRole('button', { name: /plugin\.install_confirm/i });
    await user.click(confirmButton);

    // Progress indicator should show "Installing..." or similar text
    await waitFor(() => {
      expect(screen.getByText(/plugin\.installing/i)).toBeInTheDocument();
    });

    resolveInstall!({ ok: true, plugin_name: 'my-wasm-plugin' });

    await waitFor(() => {
      expect(screen.queryByText(/plugin\.installing/i)).not.toBeInTheDocument();
    });
  });
});

describe('Install Plugin Success Flow', () => {
  const mockInstallPlugin = vi.fn();

  beforeEach(() => {
    vi.clearAllMocks();
    vi.mocked(api).installPlugin = mockInstallPlugin;
  });

  it('refreshes plugin list after successful installation', async () => {
    const user = userEvent.setup();

    // Initial plugins before install
    const initialPlugins = [
      {
        name: 'existing-plugin',
        version: '1.0.0',
        status: 'loaded' as const,
        tools: [],
        capabilities: [],
        description: 'Existing plugin',
      },
    ];

    // Updated plugins after install (includes newly installed plugin)
    const updatedPlugins = [
      ...initialPlugins,
      {
        name: 'newly-installed-plugin',
        version: '1.0.0',
        status: 'loaded' as const,
        tools: [{ name: 'new-tool' }],
        capabilities: ['cli'],
        description: 'Plugin installed via the UI',
      },
    ];

    // First call returns initial plugins, second call (after install) returns updated list
    mockGetPlugins
      .mockResolvedValueOnce({ plugins: initialPlugins })
      .mockResolvedValueOnce({ plugins: updatedPlugins });

    mockInstallPlugin.mockResolvedValueOnce({ ok: true, plugin_name: 'newly-installed-plugin' });

    render(
      <MemoryRouter initialEntries={['/plugins']}>
        <Routes>
          <Route path="/plugins" element={<Plugins />} />
        </Routes>
      </MemoryRouter>
    );

    // Wait for initial plugins to load
    await waitFor(() => {
      expect(screen.getByText('existing-plugin')).toBeInTheDocument();
    });

    // Verify new plugin is NOT yet visible
    expect(screen.queryByText('newly-installed-plugin')).not.toBeInTheDocument();

    // Verify initial plugin count
    expect(screen.getByText(/plugin\.title.*\(1\)/)).toBeInTheDocument();

    // Open install modal
    const installButton = screen.getByRole('button', { name: /plugin\.install/i });
    await user.click(installButton);

    // Enter source path in modal
    await waitFor(() => {
      expect(screen.getByPlaceholderText(/plugin\.install_source_placeholder/i)).toBeInTheDocument();
    });

    const sourceInput = screen.getByPlaceholderText(/plugin\.install_source_placeholder/i);
    await user.type(sourceInput, '/path/to/newly-installed-plugin');

    // Click confirm to install
    const confirmButton = screen.getByRole('button', { name: /plugin\.install_confirm/i });
    await user.click(confirmButton);

    // Wait for install to complete and verify plugin list is refreshed
    await waitFor(() => {
      // New plugin should now be visible
      expect(screen.getByText('newly-installed-plugin')).toBeInTheDocument();
    });

    // Verify both plugins are now displayed
    expect(screen.getByText('existing-plugin')).toBeInTheDocument();
    expect(screen.getByText('newly-installed-plugin')).toBeInTheDocument();

    // Verify plugin count updated
    expect(screen.getByText(/plugin\.title.*\(2\)/)).toBeInTheDocument();

    // Verify getPlugins was called twice: initial load + refresh after install
    expect(mockGetPlugins).toHaveBeenCalledTimes(2);
  });

  it('shows success confirmation notification after successful installation', async () => {
    const user = userEvent.setup();

    mockGetPlugins.mockResolvedValue({ plugins: [] });
    mockInstallPlugin.mockResolvedValueOnce({ ok: true, plugin_name: 'my-cool-plugin' });

    render(
      <MemoryRouter initialEntries={['/plugins']}>
        <Routes>
          <Route path="/plugins" element={<Plugins />} />
        </Routes>
      </MemoryRouter>
    );

    // Wait for initial load
    await waitFor(() => {
      expect(screen.getByText(/plugin\.title/)).toBeInTheDocument();
    });

    // Verify no notification initially
    expect(screen.queryByText(/installed successfully/)).not.toBeInTheDocument();

    // Open install modal
    const installButton = screen.getByRole('button', { name: /plugin\.install/i });
    await user.click(installButton);

    // Enter source and confirm
    await waitFor(() => {
      expect(screen.getByPlaceholderText(/plugin\.install_source_placeholder/i)).toBeInTheDocument();
    });

    const sourceInput = screen.getByPlaceholderText(/plugin\.install_source_placeholder/i);
    await user.type(sourceInput, '/path/to/my-cool-plugin');

    const confirmButton = screen.getByRole('button', { name: /plugin\.install_confirm/i });
    await user.click(confirmButton);

    // Wait for success notification to appear
    await waitFor(() => {
      expect(screen.getByText(/installed successfully/)).toBeInTheDocument();
    });

    // Verify success notification has correct styling (green background)
    const notification = screen.getByText(/installed successfully/).closest('div');
    expect(notification).toHaveStyle({ color: '#00e68a' });
  });

  it('success notification includes plugin name', async () => {
    const user = userEvent.setup();

    mockGetPlugins.mockResolvedValue({ plugins: [] });
    mockInstallPlugin.mockResolvedValueOnce({ ok: true, plugin_name: 'awesome-plugin' });

    render(
      <MemoryRouter initialEntries={['/plugins']}>
        <Routes>
          <Route path="/plugins" element={<Plugins />} />
        </Routes>
      </MemoryRouter>
    );

    await waitFor(() => {
      expect(screen.getByText(/plugin\.title/)).toBeInTheDocument();
    });

    // Open install modal and install
    const installButton = screen.getByRole('button', { name: /plugin\.install/i });
    await user.click(installButton);

    await waitFor(() => {
      expect(screen.getByPlaceholderText(/plugin\.install_source_placeholder/i)).toBeInTheDocument();
    });

    const sourceInput = screen.getByPlaceholderText(/plugin\.install_source_placeholder/i);
    await user.type(sourceInput, '/plugins/awesome-plugin');

    const confirmButton = screen.getByRole('button', { name: /plugin\.install_confirm/i });
    await user.click(confirmButton);

    // Wait for success notification that includes the plugin name
    await waitFor(() => {
      // The notification should contain the plugin name from the API response
      expect(screen.getByText(/awesome-plugin/)).toBeInTheDocument();
    });
  });

  it('success notification includes check icon', async () => {
    const user = userEvent.setup();

    mockGetPlugins.mockResolvedValue({ plugins: [] });
    mockInstallPlugin.mockResolvedValueOnce({ ok: true, plugin_name: 'icon-test-plugin' });

    render(
      <MemoryRouter initialEntries={['/plugins']}>
        <Routes>
          <Route path="/plugins" element={<Plugins />} />
        </Routes>
      </MemoryRouter>
    );

    await waitFor(() => {
      expect(screen.getByText(/plugin\.title/)).toBeInTheDocument();
    });

    // Open install modal and install
    const installButton = screen.getByRole('button', { name: /plugin\.install/i });
    await user.click(installButton);

    await waitFor(() => {
      expect(screen.getByPlaceholderText(/plugin\.install_source_placeholder/i)).toBeInTheDocument();
    });

    const sourceInput = screen.getByPlaceholderText(/plugin\.install_source_placeholder/i);
    await user.type(sourceInput, '/plugins/icon-test-plugin');

    const confirmButton = screen.getByRole('button', { name: /plugin\.install_confirm/i });
    await user.click(confirmButton);

    // Wait for success notification
    await waitFor(() => {
      expect(screen.getByText(/installed successfully/)).toBeInTheDocument();
    });

    // Verify notification contains a check icon (svg element)
    const notification = screen.getByText(/installed successfully/).closest('div');
    expect(notification?.querySelector('svg')).toBeInTheDocument();
  });

  it('modal closes after successful installation', async () => {
    const user = userEvent.setup();

    mockGetPlugins.mockResolvedValue({ plugins: [] });
    mockInstallPlugin.mockResolvedValueOnce({ ok: true, plugin_name: 'modal-close-test' });

    render(
      <MemoryRouter initialEntries={['/plugins']}>
        <Routes>
          <Route path="/plugins" element={<Plugins />} />
        </Routes>
      </MemoryRouter>
    );

    await waitFor(() => {
      expect(screen.getByText(/plugin\.title/)).toBeInTheDocument();
    });

    // Open install modal
    const installButton = screen.getByRole('button', { name: /plugin\.install/i });
    await user.click(installButton);

    // Verify modal is open (source input is visible)
    await waitFor(() => {
      expect(screen.getByPlaceholderText(/plugin\.install_source_placeholder/i)).toBeInTheDocument();
    });

    const sourceInput = screen.getByPlaceholderText(/plugin\.install_source_placeholder/i);
    await user.type(sourceInput, '/plugins/modal-close-test');

    const confirmButton = screen.getByRole('button', { name: /plugin\.install_confirm/i });
    await user.click(confirmButton);

    // Wait for install to complete
    await waitFor(() => {
      expect(screen.getByText(/installed successfully/)).toBeInTheDocument();
    });

    // Modal should be closed (source input no longer visible)
    expect(screen.queryByPlaceholderText(/plugin\.install_source_placeholder/i)).not.toBeInTheDocument();
  });
});

describe('Install Plugin Failure Flow', () => {
  const mockInstallPlugin = vi.fn();

  beforeEach(() => {
    vi.clearAllMocks();
    mockGetPlugins.mockResolvedValue({ plugins: [] });
    vi.mocked(api).installPlugin = mockInstallPlugin;
  });

  it('shows descriptive error message when install returns ok: false', async () => {
    const user = userEvent.setup();
    const descriptiveError = 'manifest.toml not found at /invalid/path';
    mockInstallPlugin.mockResolvedValueOnce({ ok: false, error: descriptiveError });

    render(
      <MemoryRouter initialEntries={['/plugins']}>
        <Routes>
          <Route path="/plugins" element={<Plugins />} />
        </Routes>
      </MemoryRouter>
    );

    await waitFor(() => {
      expect(screen.getByText(/plugin\.title/)).toBeInTheDocument();
    });

    // Open install modal
    const installButton = screen.getByRole('button', { name: /plugin\.install/i });
    await user.click(installButton);

    // Enter source and confirm
    await waitFor(() => {
      expect(screen.getByPlaceholderText(/plugin\.install_source_placeholder/i)).toBeInTheDocument();
    });

    const sourceInput = screen.getByPlaceholderText(/plugin\.install_source_placeholder/i);
    await user.type(sourceInput, '/invalid/path');

    const confirmButton = screen.getByRole('button', { name: /plugin\.install_confirm/i });
    await user.click(confirmButton);

    // Wait for error message to appear in modal
    await waitFor(() => {
      expect(screen.getByTestId('install-error-message')).toBeInTheDocument();
    });

    // Verify the descriptive error message is shown
    expect(screen.getByText(descriptiveError)).toBeInTheDocument();
  });

  it('shows descriptive error message when install throws exception', async () => {
    const user = userEvent.setup();
    mockInstallPlugin.mockRejectedValueOnce(new Error('Network connection failed'));

    render(
      <MemoryRouter initialEntries={['/plugins']}>
        <Routes>
          <Route path="/plugins" element={<Plugins />} />
        </Routes>
      </MemoryRouter>
    );

    await waitFor(() => {
      expect(screen.getByText(/plugin\.title/)).toBeInTheDocument();
    });

    // Open install modal
    const installButton = screen.getByRole('button', { name: /plugin\.install/i });
    await user.click(installButton);

    await waitFor(() => {
      expect(screen.getByPlaceholderText(/plugin\.install_source_placeholder/i)).toBeInTheDocument();
    });

    const sourceInput = screen.getByPlaceholderText(/plugin\.install_source_placeholder/i);
    await user.type(sourceInput, 'https://example.com/bad-plugin');

    const confirmButton = screen.getByRole('button', { name: /plugin\.install_confirm/i });
    await user.click(confirmButton);

    // Wait for error message to appear
    await waitFor(() => {
      expect(screen.getByTestId('install-error-message')).toBeInTheDocument();
    });

    // Verify the exception message is shown
    expect(screen.getByText('Network connection failed')).toBeInTheDocument();
  });

  it('error message has red styling', async () => {
    const user = userEvent.setup();
    mockInstallPlugin.mockResolvedValueOnce({ ok: false, error: 'Plugin load failed' });

    render(
      <MemoryRouter initialEntries={['/plugins']}>
        <Routes>
          <Route path="/plugins" element={<Plugins />} />
        </Routes>
      </MemoryRouter>
    );

    await waitFor(() => {
      expect(screen.getByText(/plugin\.title/)).toBeInTheDocument();
    });

    const installButton = screen.getByRole('button', { name: /plugin\.install/i });
    await user.click(installButton);

    await waitFor(() => {
      expect(screen.getByPlaceholderText(/plugin\.install_source_placeholder/i)).toBeInTheDocument();
    });

    const sourceInput = screen.getByPlaceholderText(/plugin\.install_source_placeholder/i);
    await user.type(sourceInput, '/bad/path');

    const confirmButton = screen.getByRole('button', { name: /plugin\.install_confirm/i });
    await user.click(confirmButton);

    await waitFor(() => {
      expect(screen.getByTestId('install-error-message')).toBeInTheDocument();
    });

    // Verify error has red styling
    const errorDiv = screen.getByTestId('install-error-message');
    expect(errorDiv).toHaveStyle({ color: '#f87171' });
  });

  it('modal stays open on failure so user can retry', async () => {
    const user = userEvent.setup();
    mockInstallPlugin.mockResolvedValueOnce({ ok: false, error: 'Invalid plugin format' });

    render(
      <MemoryRouter initialEntries={['/plugins']}>
        <Routes>
          <Route path="/plugins" element={<Plugins />} />
        </Routes>
      </MemoryRouter>
    );

    await waitFor(() => {
      expect(screen.getByText(/plugin\.title/)).toBeInTheDocument();
    });

    const installButton = screen.getByRole('button', { name: /plugin\.install/i });
    await user.click(installButton);

    await waitFor(() => {
      expect(screen.getByPlaceholderText(/plugin\.install_source_placeholder/i)).toBeInTheDocument();
    });

    const sourceInput = screen.getByPlaceholderText(/plugin\.install_source_placeholder/i);
    await user.type(sourceInput, '/wrong/path');

    const confirmButton = screen.getByRole('button', { name: /plugin\.install_confirm/i });
    await user.click(confirmButton);

    // Wait for error
    await waitFor(() => {
      expect(screen.getByTestId('install-error-message')).toBeInTheDocument();
    });

    // Modal should still be open (source input still visible)
    expect(screen.getByPlaceholderText(/plugin\.install_source_placeholder/i)).toBeInTheDocument();
  });

  it('error clears when user modifies input', async () => {
    const user = userEvent.setup();
    mockInstallPlugin.mockResolvedValueOnce({ ok: false, error: 'Some error' });

    render(
      <MemoryRouter initialEntries={['/plugins']}>
        <Routes>
          <Route path="/plugins" element={<Plugins />} />
        </Routes>
      </MemoryRouter>
    );

    await waitFor(() => {
      expect(screen.getByText(/plugin\.title/)).toBeInTheDocument();
    });

    const installButton = screen.getByRole('button', { name: /plugin\.install/i });
    await user.click(installButton);

    await waitFor(() => {
      expect(screen.getByPlaceholderText(/plugin\.install_source_placeholder/i)).toBeInTheDocument();
    });

    const sourceInput = screen.getByPlaceholderText(/plugin\.install_source_placeholder/i);
    await user.type(sourceInput, '/bad');

    const confirmButton = screen.getByRole('button', { name: /plugin\.install_confirm/i });
    await user.click(confirmButton);

    // Wait for error
    await waitFor(() => {
      expect(screen.getByTestId('install-error-message')).toBeInTheDocument();
    });

    // Type more to modify input
    await user.type(sourceInput, '/corrected');

    // Error should be cleared
    expect(screen.queryByTestId('install-error-message')).not.toBeInTheDocument();
  });
});

describe('Remove Plugin Button in List View - Acceptance: Remove button available on each plugin in the list view', () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it('each plugin card has a Remove button', async () => {
    const mockPlugins = [
      {
        name: 'plugin-one',
        version: '1.0.0',
        status: 'loaded',
        description: 'First plugin',
        capabilities: ['tool'],
        tools: [],
      },
      {
        name: 'plugin-two',
        version: '2.0.0',
        status: 'discovered',
        description: 'Second plugin',
        capabilities: ['cron'],
        tools: ['tool1'],
      },
    ];
    mockGetPlugins.mockResolvedValue({ plugins: mockPlugins });

    render(
      <MemoryRouter initialEntries={['/plugins']}>
        <Routes>
          <Route path="/plugins" element={<Plugins />} />
        </Routes>
      </MemoryRouter>
    );

    // Wait for plugins to load
    await waitFor(() => {
      expect(screen.getByText('plugin-one')).toBeInTheDocument();
      expect(screen.getByText('plugin-two')).toBeInTheDocument();
    });

    // Verify each plugin has a Remove button
    const removeButtons = screen.getAllByRole('button', { name: /plugin\.remove/i });
    expect(removeButtons).toHaveLength(2);
  });

  it('Remove button has Trash icon', async () => {
    const mockPlugins = [
      {
        name: 'test-plugin',
        version: '1.0.0',
        status: 'loaded',
        description: 'Test plugin',
        capabilities: [],
        tools: [],
      },
    ];
    mockGetPlugins.mockResolvedValue({ plugins: mockPlugins });

    render(
      <MemoryRouter initialEntries={['/plugins']}>
        <Routes>
          <Route path="/plugins" element={<Plugins />} />
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

  it('Remove button is visible for each plugin in a multi-plugin list', async () => {
    const mockPlugins = [
      { name: 'alpha', version: '1.0.0', status: 'loaded', description: '', capabilities: [], tools: [] },
      { name: 'beta', version: '1.0.0', status: 'discovered', description: '', capabilities: [], tools: [] },
      { name: 'gamma', version: '1.0.0', status: 'loaded', description: '', capabilities: [], tools: [] },
    ];
    mockGetPlugins.mockResolvedValue({ plugins: mockPlugins });

    render(
      <MemoryRouter initialEntries={['/plugins']}>
        <Routes>
          <Route path="/plugins" element={<Plugins />} />
        </Routes>
      </MemoryRouter>
    );

    await waitFor(() => {
      expect(screen.getByText('alpha')).toBeInTheDocument();
      expect(screen.getByText('beta')).toBeInTheDocument();
      expect(screen.getByText('gamma')).toBeInTheDocument();
    });

    // All three plugins should have Remove buttons
    const removeButtons = screen.getAllByRole('button', { name: /plugin\.remove/i });
    expect(removeButtons).toHaveLength(3);

    // Each button should be visible
    removeButtons.forEach((button) => {
      expect(button).toBeVisible();
    });
  });
});

describe('Remove Plugin Confirmation Dialog - Acceptance: Confirmation dialog shown before removal', () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it('clicking Remove button shows a confirmation dialog', async () => {
    const user = userEvent.setup();
    const mockPlugins = [
      {
        name: 'test-plugin',
        version: '1.0.0',
        status: 'loaded',
        description: 'Test plugin',
        capabilities: [],
        tools: [],
      },
    ];
    mockGetPlugins.mockResolvedValue({ plugins: mockPlugins });

    render(
      <MemoryRouter initialEntries={['/plugins']}>
        <Routes>
          <Route path="/plugins" element={<Plugins />} />
        </Routes>
      </MemoryRouter>
    );

    await waitFor(() => {
      expect(screen.getByText('test-plugin')).toBeInTheDocument();
    });

    // Click the Remove button
    const removeButton = screen.getByRole('button', { name: /plugin\.remove/i });
    await user.click(removeButton);

    // Confirmation dialog should appear
    const dialog = screen.getByRole('dialog');
    expect(dialog).toBeInTheDocument();
  });

  it('confirmation dialog displays plugin name', async () => {
    const user = userEvent.setup();
    const mockPlugins = [
      {
        name: 'my-awesome-plugin',
        version: '1.0.0',
        status: 'loaded',
        description: '',
        capabilities: [],
        tools: [],
      },
    ];
    mockGetPlugins.mockResolvedValue({ plugins: mockPlugins });

    render(
      <MemoryRouter initialEntries={['/plugins']}>
        <Routes>
          <Route path="/plugins" element={<Plugins />} />
        </Routes>
      </MemoryRouter>
    );

    await waitFor(() => {
      expect(screen.getByText('my-awesome-plugin')).toBeInTheDocument();
    });

    const removeButton = screen.getByRole('button', { name: /plugin\.remove/i });
    await user.click(removeButton);

    // Dialog should mention the plugin name
    const dialog = screen.getByRole('dialog');
    expect(dialog).toHaveTextContent('my-awesome-plugin');
  });

  it('confirmation dialog has confirm and cancel buttons', async () => {
    const user = userEvent.setup();
    const mockPlugins = [
      {
        name: 'test-plugin',
        version: '1.0.0',
        status: 'loaded',
        description: '',
        capabilities: [],
        tools: [],
      },
    ];
    mockGetPlugins.mockResolvedValue({ plugins: mockPlugins });

    render(
      <MemoryRouter initialEntries={['/plugins']}>
        <Routes>
          <Route path="/plugins" element={<Plugins />} />
        </Routes>
      </MemoryRouter>
    );

    await waitFor(() => {
      expect(screen.getByText('test-plugin')).toBeInTheDocument();
    });

    const removeButton = screen.getByRole('button', { name: /plugin\.remove/i });
    await user.click(removeButton);

    // Dialog should have confirm and cancel buttons
    expect(screen.getByRole('button', { name: /plugin\.remove_confirm/i })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /plugin\.remove_cancel/i })).toBeInTheDocument();
  });

  it('clicking cancel closes the dialog without removing the plugin', async () => {
    const user = userEvent.setup();
    const mockPlugins = [
      {
        name: 'test-plugin',
        version: '1.0.0',
        status: 'loaded',
        description: '',
        capabilities: [],
        tools: [],
      },
    ];
    mockGetPlugins.mockResolvedValue({ plugins: mockPlugins });

    render(
      <MemoryRouter initialEntries={['/plugins']}>
        <Routes>
          <Route path="/plugins" element={<Plugins />} />
        </Routes>
      </MemoryRouter>
    );

    await waitFor(() => {
      expect(screen.getByText('test-plugin')).toBeInTheDocument();
    });

    // Open dialog
    const removeButton = screen.getByRole('button', { name: /plugin\.remove/i });
    await user.click(removeButton);

    expect(screen.getByRole('dialog')).toBeInTheDocument();

    // Click cancel
    const cancelButton = screen.getByRole('button', { name: /plugin\.remove_cancel/i });
    await user.click(cancelButton);

    // Dialog should close
    await waitFor(() => {
      expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
    });

    // Plugin should still be in the list
    expect(screen.getByText('test-plugin')).toBeInTheDocument();
  });

  it('clicking outside the dialog closes it', async () => {
    const user = userEvent.setup();
    const mockPlugins = [
      {
        name: 'test-plugin',
        version: '1.0.0',
        status: 'loaded',
        description: '',
        capabilities: [],
        tools: [],
      },
    ];
    mockGetPlugins.mockResolvedValue({ plugins: mockPlugins });

    render(
      <MemoryRouter initialEntries={['/plugins']}>
        <Routes>
          <Route path="/plugins" element={<Plugins />} />
        </Routes>
      </MemoryRouter>
    );

    await waitFor(() => {
      expect(screen.getByText('test-plugin')).toBeInTheDocument();
    });

    const removeButton = screen.getByRole('button', { name: /plugin\.remove/i });
    await user.click(removeButton);

    const dialog = screen.getByRole('dialog');
    expect(dialog).toBeInTheDocument();

    // Click on the backdrop (the first child div with blur effect, not the modal content)
    // The dialog has onClick handler, and clicks on the backdrop bubble up to it
    const backdrop = dialog.querySelector('[class*="absolute inset-0"]');
    if (backdrop) {
      await user.click(backdrop);
    }

    // Dialog should close
    await waitFor(() => {
      expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
    });
  });
});

describe('List Refresh After Removal - Acceptance: List refreshes after successful removal', () => {
  const mockRemovePlugin = api.removePlugin as ReturnType<typeof vi.fn>;

  beforeEach(() => {
    vi.clearAllMocks();
  });

  it('list refreshes after successful removal - plugin is removed from list', async () => {
    const user = userEvent.setup();

    // Initial plugins before removal
    const initialPlugins = [
      {
        name: 'plugin-to-remove',
        version: '1.0.0',
        status: 'loaded' as const,
        tools: [],
        capabilities: [],
        description: 'This plugin will be removed',
      },
      {
        name: 'plugin-to-keep',
        version: '2.0.0',
        status: 'loaded' as const,
        tools: [],
        capabilities: [],
        description: 'This plugin should remain',
      },
    ];

    mockGetPlugins.mockResolvedValue({ plugins: initialPlugins });
    mockRemovePlugin.mockResolvedValue({ ok: true });

    render(
      <MemoryRouter initialEntries={['/plugins']}>
        <Routes>
          <Route path="/plugins" element={<Plugins />} />
        </Routes>
      </MemoryRouter>
    );

    // Wait for initial plugins to load
    await waitFor(() => {
      expect(screen.getByText('plugin-to-remove')).toBeInTheDocument();
      expect(screen.getByText('plugin-to-keep')).toBeInTheDocument();
    });

    // Verify initial plugin count
    expect(screen.getByText(/plugin\.title.*\(2\)/)).toBeInTheDocument();

    // Find the remove button for 'plugin-to-remove'
    const pluginCards = screen.getAllByText(/plugin-to-/).map((el) => el.closest('.card'));
    const targetCard = pluginCards.find((card) => card?.textContent?.includes('plugin-to-remove'));
    const removeButton = targetCard?.querySelector('[aria-label="plugin.remove"]');
    expect(removeButton).toBeInTheDocument();

    await user.click(removeButton!);

    // Confirm removal in dialog
    await waitFor(() => {
      expect(screen.getByRole('dialog')).toBeInTheDocument();
    });

    const confirmButton = screen.getByRole('button', { name: /plugin\.remove_confirm/i });
    await user.click(confirmButton);

    // Wait for the plugin to be removed from the list
    await waitFor(() => {
      expect(screen.queryByText('plugin-to-remove')).not.toBeInTheDocument();
    });

    // Verify the other plugin is still there
    expect(screen.getByText('plugin-to-keep')).toBeInTheDocument();

    // Verify plugin count updated
    expect(screen.getByText(/plugin\.title.*\(1\)/)).toBeInTheDocument();

    // Verify removePlugin API was called with correct name
    expect(mockRemovePlugin).toHaveBeenCalledWith('plugin-to-remove');
  });

  it('list shows empty state when last plugin is removed', async () => {
    const user = userEvent.setup();

    const singlePlugin = [
      {
        name: 'only-plugin',
        version: '1.0.0',
        status: 'loaded' as const,
        tools: [],
        capabilities: [],
        description: 'The only plugin',
      },
    ];

    mockGetPlugins.mockResolvedValue({ plugins: singlePlugin });
    mockRemovePlugin.mockResolvedValue({ ok: true });

    render(
      <MemoryRouter initialEntries={['/plugins']}>
        <Routes>
          <Route path="/plugins" element={<Plugins />} />
        </Routes>
      </MemoryRouter>
    );

    // Wait for plugin to load
    await waitFor(() => {
      expect(screen.getByText('only-plugin')).toBeInTheDocument();
    });

    // Verify initial count
    expect(screen.getByText(/plugin\.title.*\(1\)/)).toBeInTheDocument();

    // Remove the plugin
    const removeButton = screen.getByRole('button', { name: /plugin\.remove/i });
    await user.click(removeButton);

    await waitFor(() => {
      expect(screen.getByRole('dialog')).toBeInTheDocument();
    });

    const confirmButton = screen.getByRole('button', { name: /plugin\.remove_confirm/i });
    await user.click(confirmButton);

    // Wait for removal and verify empty state
    await waitFor(() => {
      expect(screen.queryByText('only-plugin')).not.toBeInTheDocument();
    });

    // Verify empty state is shown
    expect(screen.getByText('plugin.empty')).toBeInTheDocument();

    // Verify count shows 0
    expect(screen.getByText(/plugin\.title.*\(0\)/)).toBeInTheDocument();
  });

  it('list does not change when removal fails', async () => {
    const user = userEvent.setup();

    const plugins = [
      {
        name: 'test-plugin',
        version: '1.0.0',
        status: 'loaded' as const,
        tools: [],
        capabilities: [],
        description: 'Test plugin',
      },
    ];

    mockGetPlugins.mockResolvedValue({ plugins });
    mockRemovePlugin.mockResolvedValue({ ok: false, error: 'Removal failed' });

    render(
      <MemoryRouter initialEntries={['/plugins']}>
        <Routes>
          <Route path="/plugins" element={<Plugins />} />
        </Routes>
      </MemoryRouter>
    );

    await waitFor(() => {
      expect(screen.getByText('test-plugin')).toBeInTheDocument();
    });

    // Try to remove plugin
    const removeButton = screen.getByRole('button', { name: /plugin\.remove/i });
    await user.click(removeButton);

    await waitFor(() => {
      expect(screen.getByRole('dialog')).toBeInTheDocument();
    });

    const confirmButton = screen.getByRole('button', { name: /plugin\.remove_confirm/i });
    await user.click(confirmButton);

    // Wait for error notification
    await waitFor(() => {
      expect(screen.getByText('Removal failed')).toBeInTheDocument();
    });

    // Plugin should still be in the list
    expect(screen.getByText('test-plugin')).toBeInTheDocument();
    expect(screen.getByText(/plugin\.title.*\(1\)/)).toBeInTheDocument();
  });

  it('search results update correctly after removal', async () => {
    const user = userEvent.setup();

    const plugins = [
      {
        name: 'alpha-plugin',
        version: '1.0.0',
        status: 'loaded' as const,
        tools: [],
        capabilities: ['cli'],
        description: 'Alpha plugin',
      },
      {
        name: 'beta-plugin',
        version: '1.0.0',
        status: 'loaded' as const,
        tools: [],
        capabilities: ['cli'],
        description: 'Beta plugin',
      },
    ];

    mockGetPlugins.mockResolvedValue({ plugins });
    mockRemovePlugin.mockResolvedValue({ ok: true });

    render(
      <MemoryRouter initialEntries={['/plugins']}>
        <Routes>
          <Route path="/plugins" element={<Plugins />} />
        </Routes>
      </MemoryRouter>
    );

    await waitFor(() => {
      expect(screen.getByText('alpha-plugin')).toBeInTheDocument();
      expect(screen.getByText('beta-plugin')).toBeInTheDocument();
    });

    // Search for 'alpha'
    const searchInput = screen.getByPlaceholderText(/plugin\.search_placeholder/i);
    await user.type(searchInput, 'alpha');

    // Only alpha-plugin should be visible
    await waitFor(() => {
      expect(screen.getByText('alpha-plugin')).toBeInTheDocument();
      expect(screen.queryByText('beta-plugin')).not.toBeInTheDocument();
    });

    // Remove alpha-plugin while filtered
    const removeButton = screen.getByRole('button', { name: /plugin\.remove/i });
    await user.click(removeButton);

    await waitFor(() => {
      expect(screen.getByRole('dialog')).toBeInTheDocument();
    });

    const confirmButton = screen.getByRole('button', { name: /plugin\.remove_confirm/i });
    await user.click(confirmButton);

    // Wait for removal
    await waitFor(() => {
      expect(screen.queryByText('alpha-plugin')).not.toBeInTheDocument();
    });

    // Should show no results message since search term no longer matches
    expect(screen.getByText('plugin.no_results')).toBeInTheDocument();

    // Clear search to see remaining plugin
    await user.clear(searchInput);

    await waitFor(() => {
      expect(screen.getByText('beta-plugin')).toBeInTheDocument();
    });

    // Verify count reflects actual total (not filtered count)
    expect(screen.getByText(/plugin\.title.*\(1\)/)).toBeInTheDocument();
  });
});

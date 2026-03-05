import { describe, expect, it, vi } from 'vitest';
import { act, fireEvent, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { MemoryRouter } from 'react-router-dom';
import Sidebar from './Sidebar';

function renderSidebar({
  isOpen = false,
  isCollapsed = false,
  onClose = vi.fn(),
  onToggleCollapse = vi.fn(),
}: {
  isOpen?: boolean;
  isCollapsed?: boolean;
  onClose?: () => void;
  onToggleCollapse?: () => void;
} = {}) {
  const view = render(
    <MemoryRouter>
      <Sidebar
        isOpen={isOpen}
        isCollapsed={isCollapsed}
        onClose={onClose}
        onToggleCollapse={onToggleCollapse}
      />
    </MemoryRouter>
  );

  return { ...view, onClose, onToggleCollapse };
}

describe('Sidebar', () => {
  it('toggles open/close state and invokes close handlers for mobile controls', async () => {
    const user = userEvent.setup();
    const closed = renderSidebar({ isOpen: false });
    const closedButtons = closed.getAllByRole('button', {
      name: /Close navigation/i,
    });
    expect(closedButtons.length).toBeGreaterThan(0);
    const closedOverlay = closedButtons[0];
    if (!closedOverlay) {
      throw new Error('Expected sidebar overlay button');
    }
    expect(closedOverlay).toHaveClass('pointer-events-none');
    closed.unmount();

    const opened = renderSidebar({ isOpen: true });
    const openedCloseButtons = opened.getAllByRole('button', {
      name: /Close navigation/i,
    });
    expect(openedCloseButtons.length).toBeGreaterThanOrEqual(2);
    const openedOverlay = openedCloseButtons[0];
    const mobileCloseButton = openedCloseButtons[1];
    if (!openedOverlay || !mobileCloseButton) {
      throw new Error('Expected sidebar overlay and close buttons');
    }

    expect(openedOverlay).toHaveClass('opacity-100');

    await user.click(openedOverlay);
    await user.click(mobileCloseButton);
    expect(opened.onClose).toHaveBeenCalledTimes(2);
  });

  it('supports collapsed mode controls and closes on navigation click', () => {
    vi.useFakeTimers();
    try {
      const view = renderSidebar({ isOpen: true, isCollapsed: true });
      act(() => {
        vi.advanceTimersByTime(1_000);
      });

      const collapseToggle = screen.getByRole('button', {
        name: /Expand navigation/i,
      });
      fireEvent.click(collapseToggle);
      expect(view.onToggleCollapse).toHaveBeenCalledTimes(1);

      const dashboardLink = screen.getByRole('link', { name: 'Dashboard' });
      expect(dashboardLink).toHaveAttribute('title', 'Dashboard');
      fireEvent.click(dashboardLink);
      expect(view.onClose).toHaveBeenCalled();
    } finally {
      vi.useRealTimers();
    }
  });
});

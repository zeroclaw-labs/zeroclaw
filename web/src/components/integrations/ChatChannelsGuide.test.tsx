import { describe, expect, it } from 'vitest';
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import ChatChannelsGuide from './ChatChannelsGuide';
import { CHAT_CHANNEL_SUPPORT } from '@/lib/chatChannels';

describe('ChatChannelsGuide', () => {
  it('renders the supported channel matrix and key notes', () => {
    render(<ChatChannelsGuide />);

    expect(screen.getByText('Supported Chat Channels')).toBeInTheDocument();
    expect(
      screen.getByText(`${CHAT_CHANNEL_SUPPORT.length} channels listed`),
    ).toBeInTheDocument();
    expect(screen.getByText('BlueBubbles')).toBeInTheDocument();
    expect(screen.getByText('WhatsApp')).toBeInTheDocument();
    expect(screen.getByText('Zalo Personal')).toBeInTheDocument();
    expect(screen.getByText('Channel Notes')).toBeInTheDocument();
  });

  it('supports collapsing and expanding the section', async () => {
    const user = userEvent.setup();
    render(<ChatChannelsGuide />);

    const toggle = screen.getByRole('button', {
      name: /Supported Chat Channels/i,
    });

    expect(toggle).toHaveAttribute('aria-expanded', 'true');
    expect(screen.getByText('BlueBubbles')).toBeInTheDocument();

    await user.click(toggle);
    expect(toggle).toHaveAttribute('aria-expanded', 'false');
    expect(screen.queryByText('BlueBubbles')).not.toBeInTheDocument();

    await user.click(toggle);
    expect(toggle).toHaveAttribute('aria-expanded', 'true');
    expect(screen.getByText('BlueBubbles')).toBeInTheDocument();
  });
});

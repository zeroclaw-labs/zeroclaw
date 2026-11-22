import type { ReactNode } from 'react';
import { RelayNotice } from '@/components/auth/RelayNotice';

/**
 * The dashboard's column beside the rail. It is exactly the window's height
 * and only its `<main>` scrolls, so the page itself never does. The relay
 * notice is its first row: it takes part of that height and stays on screen.
 * Above the column it would make the page taller than the window, and any
 * scroll, such as chat jumping to its latest message, would carry it away.
 */
export function MainColumn({ viaRelay, showRail = true, children }: { viaRelay: boolean; showRail?: boolean; children: ReactNode }) {
  return (
    <div className={`flex flex-col flex-1 min-w-0 h-screen ml-0 ${showRail ? "md:ml-14" : ""}`}>
      {viaRelay && <RelayNotice bar />}
      {children}
    </div>
  );
}

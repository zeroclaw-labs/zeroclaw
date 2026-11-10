import type { ConversationCostSummary } from './api';

export type CostScheduler = (callback: () => void, delay: number) => () => void;

/** One visible conversation's read lifecycle. No accounting happens here. */
export class ConversationCostPoller {
  private visible = false;
  private disposed = false;
  private blocked = false;
  private queued = false;
  private failures = 0;
  private controller: AbortController | null = null;
  private cancelTimer: (() => void) | null = null;
  private readonly load: (signal: AbortSignal) => Promise<ConversationCostSummary>;
  private readonly publish: (value: ConversationCostSummary | null) => void;
  private readonly terminal: (error: unknown) => boolean;
  private readonly schedule: CostScheduler;

  constructor(
    load: (signal: AbortSignal) => Promise<ConversationCostSummary>,
    publish: (value: ConversationCostSummary | null) => void,
    terminal: (error: unknown) => boolean,
    schedule: CostScheduler = (callback, delay) => {
      const timer = setTimeout(callback, delay);
      return () => clearTimeout(timer);
    },
  ) {
    this.load = load;
    this.publish = publish;
    this.terminal = terminal;
    this.schedule = schedule;
  }

  setVisible(visible: boolean): void {
    if (this.visible === visible) return;
    this.visible = visible;
    this.cancelTimer?.();
    this.cancelTimer = null;
    if (visible) this.refresh();
  }

  refresh(): void {
    if (this.disposed || !this.visible || this.blocked) return;
    this.cancelTimer?.();
    this.cancelTimer = null;
    if (this.controller) {
      this.queued = true;
      return;
    }
    const controller = new AbortController();
    this.controller = controller;
    void this.load(controller.signal).then((value) => {
      if (this.disposed) return;
      this.failures = 0;
      this.publish(value);
    }).catch((error: unknown) => {
      if (this.disposed) return;
      this.failures += 1;
      this.blocked = this.terminal(error);
      this.publish(null);
    }).finally(() => {
      this.controller = null;
      if (this.disposed || !this.visible || this.blocked) return;
      const delay = this.failures > 0 ? ([5_000, 10_000, 20_000, 30_000][Math.min(this.failures - 1, 3)] ?? 30_000)
        : this.queued ? 0 : 2_000;
      this.queued = false;
      this.cancelTimer = this.schedule(() => this.refresh(), delay);
    });
  }

  dispose(): void {
    this.disposed = true;
    this.cancelTimer?.();
    this.controller?.abort();
  }
}

export function conversationCostAmount(value: number): string {
  return value > 0 && value < 0.0001 ? '<$0.0001' : `$${value.toFixed(4)}`;
}

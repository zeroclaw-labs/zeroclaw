import { config } from "./config";
import { addActivity } from "./state/activity";

const levels: Record<string, number> = { error: 0, warn: 1, info: 2, debug: 3 };

export function log(level: keyof typeof levels, message: string, meta?: Record<string, unknown>) {
  const current = levels[config.logLevel] ?? 2;
  if (levels[level] <= current) {
    const payload = meta ? `${message} ${JSON.stringify(meta)}` : message;
    // eslint-disable-next-line no-console
    console.log(`[mobile:${level}] ${payload}`);
    void addActivity({
      kind: "log",
      source: "chat",
      title: `${level.toUpperCase()}: ${message}`,
      detail: meta ? JSON.stringify(meta) : undefined,
    });
  }
}

export const logger = {
  info: (msg: string, meta?: Record<string, unknown>) => log("info", msg, meta),
  warn: (msg: string, meta?: Record<string, unknown>) => log("warn", msg, meta),
  error: (msg: string, meta?: Record<string, unknown>) => log("error", msg, meta),
  debug: (msg: string, meta?: Record<string, unknown>) => log("debug", msg, meta),
};

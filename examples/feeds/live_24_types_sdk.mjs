import "reflect-metadata";
import { batchUpload } from "@openclaw/sdk/transformer";
import { feed } from "@openclaw/sdk/decorators";

// This is a minimal structure example. For full production script, see:
// openclaw-afw/sdk/scripts/seed-24-feeds.mjs
// Learning: always emit one of the canonical 24 card types only.

const CANONICAL_24 = [
  "stock", "crypto", "prediction", "game", "news", "social", "poll", "chart", "logs", "table", "kv", "metric",
  "code", "integration", "weather", "calendar", "flight", "ci", "github", "image", "video", "audio", "webview", "file",
];

class MinimalNewsFeed {
  async handler() {
    const posts = await fetch("https://jsonplaceholder.typicode.com/posts").then((r) => r.json());
    const items = (Array.isArray(posts) ? posts.slice(0, 5) : []).map((p) => ({
      cardType: "news",
      title: String(p.title),
      source: "JSONPlaceholder",
      metadata: {
        id: String(p.id),
        headline: String(p.title),
        source: "JSONPlaceholder",
        category: "general",
        timestamp: new Date().toISOString(),
        url: `https://jsonplaceholder.typicode.com/posts/${p.id}`,
      },
    }));
    return { success: true, items };
  }
}

feed({ name: "Example News", schedule: "*/10 * * * *", category: "news" })(MinimalNewsFeed);
const d = Object.getOwnPropertyDescriptor(MinimalNewsFeed.prototype, "handler");
feed.handler()(MinimalNewsFeed.prototype, "handler", d);

async function main() {
  const endpoint = (process.env.ARIA_API_URL ?? "http://127.0.0.1:8080").replace(/\/$/, "");
  const token = process.env.ARIA_TOKEN ?? "dev-tenant:local";

  const result = await batchUpload({
    classes: [MinimalNewsFeed],
    endpoint,
    token,
  });

  console.log("Uploaded feed count:", result.feeds?.length ?? 0);
  console.log("Canonical types:", CANONICAL_24.join(", "));
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});

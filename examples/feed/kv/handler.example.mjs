// Kv feed handler example.
// Focus: canonical type-safe structure so feed cards render cleanly.

export class KvFeedExample {
  async handler() {
    return {
      success: true,
      items: [
        {
          cardType: "kv",
          title: "Example Kv card",
          source: "example-source",
          metadata: {
            // Required keys for kv go here.
            // See README.md in this folder for exact enforced shape.
          },
        },
      ],
    };
  }
}

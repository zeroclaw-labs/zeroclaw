// Integration feed handler example.
// Focus: canonical type-safe structure so feed cards render cleanly.

export class IntegrationFeedExample {
  async handler() {
    return {
      success: true,
      items: [
        {
          cardType: "integration",
          title: "Example Integration card",
          source: "example-source",
          metadata: {
            // Required keys for integration go here.
            // See README.md in this folder for exact enforced shape.
          },
        },
      ],
    };
  }
}

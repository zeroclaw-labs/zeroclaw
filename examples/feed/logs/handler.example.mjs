// Logs feed handler example.
// Focus: canonical type-safe structure so feed cards render cleanly.

export class LogsFeedExample {
  async handler() {
    return {
      success: true,
      items: [
        {
          cardType: "logs",
          title: "Example Logs card",
          source: "example-source",
          metadata: {
            // Required keys for logs go here.
            // See README.md in this folder for exact enforced shape.
          },
        },
      ],
    };
  }
}

// Chart feed handler example.
// Focus: canonical type-safe structure so feed cards render cleanly.

export class ChartFeedExample {
  async handler() {
    return {
      success: true,
      items: [
        {
          cardType: "chart",
          title: "Example Chart card",
          source: "example-source",
          metadata: {
            // Required keys for chart go here.
            // See README.md in this folder for exact enforced shape.
          },
        },
      ],
    };
  }
}

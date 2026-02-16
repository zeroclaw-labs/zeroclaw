// Metric feed handler example.
// Focus: canonical type-safe structure so feed cards render cleanly.

export class MetricFeedExample {
  async handler() {
    return {
      success: true,
      items: [
        {
          cardType: "metric",
          title: "Example Metric card",
          source: "example-source",
          metadata: {
            // Required keys for metric go here.
            // See README.md in this folder for exact enforced shape.
          },
        },
      ],
    };
  }
}

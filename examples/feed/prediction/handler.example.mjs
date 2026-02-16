// Prediction feed handler example.
// Focus: canonical type-safe structure so feed cards render cleanly.

export class PredictionFeedExample {
  async handler() {
    return {
      success: true,
      items: [
        {
          cardType: "prediction",
          title: "Example Prediction card",
          source: "example-source",
          metadata: {
            // Required keys for prediction go here.
            // See README.md in this folder for exact enforced shape.
          },
        },
      ],
    };
  }
}

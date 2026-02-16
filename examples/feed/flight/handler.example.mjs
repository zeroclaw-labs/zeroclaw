// Flight feed handler example.
// Focus: canonical type-safe structure so feed cards render cleanly.

export class FlightFeedExample {
  async handler() {
    return {
      success: true,
      items: [
        {
          cardType: "flight",
          title: "Example Flight card",
          source: "example-source",
          metadata: {
            // Required keys for flight go here.
            // See README.md in this folder for exact enforced shape.
          },
        },
      ],
    };
  }
}

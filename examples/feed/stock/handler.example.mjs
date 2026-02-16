// Stock feed handler example.
// Focus: canonical type-safe structure so feed cards render cleanly.

export class StockFeedExample {
  async handler() {
    return {
      success: true,
      items: [
        {
          cardType: "stock",
          title: "Example Stock card",
          source: "example-source",
          metadata: {
            // Required keys for stock go here.
            // See README.md in this folder for exact enforced shape.
          },
        },
      ],
    };
  }
}

// Table feed handler example.
// Focus: canonical type-safe structure so feed cards render cleanly.

export class TableFeedExample {
  async handler() {
    return {
      success: true,
      items: [
        {
          cardType: "table",
          title: "Example Table card",
          source: "example-source",
          metadata: {
            // Required keys for table go here.
            // See README.md in this folder for exact enforced shape.
          },
        },
      ],
    };
  }
}

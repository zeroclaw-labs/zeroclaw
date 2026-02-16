// Ci feed handler example.
// Focus: canonical type-safe structure so feed cards render cleanly.

export class CiFeedExample {
  async handler() {
    return {
      success: true,
      items: [
        {
          cardType: "ci",
          title: "Example Ci card",
          source: "example-source",
          metadata: {
            // Required keys for ci go here.
            // See README.md in this folder for exact enforced shape.
          },
        },
      ],
    };
  }
}

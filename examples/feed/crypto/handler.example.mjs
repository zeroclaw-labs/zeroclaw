// Crypto feed handler example.
// Focus: canonical type-safe structure so feed cards render cleanly.

export class CryptoFeedExample {
  async handler() {
    return {
      success: true,
      items: [
        {
          cardType: "crypto",
          title: "Example Crypto card",
          source: "example-source",
          metadata: {
            // Required keys for crypto go here.
            // See README.md in this folder for exact enforced shape.
          },
        },
      ],
    };
  }
}

// Game feed handler example.
// Focus: canonical type-safe structure so feed cards render cleanly.

export class GameFeedExample {
  async handler() {
    return {
      success: true,
      items: [
        {
          cardType: "game",
          title: "Example Game card",
          source: "example-source",
          metadata: {
            // Required keys for game go here.
            // See README.md in this folder for exact enforced shape.
          },
        },
      ],
    };
  }
}

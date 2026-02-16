// News feed handler example.
// Focus: canonical type-safe structure so feed cards render cleanly.

export class NewsFeedExample {
  async handler() {
    return {
      success: true,
      items: [
        {
          cardType: "news",
          title: "Example News card",
          source: "example-source",
          metadata: {
            // Required keys for news go here.
            // See README.md in this folder for exact enforced shape.
          },
        },
      ],
    };
  }
}

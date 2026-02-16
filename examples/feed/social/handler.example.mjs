// Social feed handler example.
// Focus: canonical type-safe structure so feed cards render cleanly.

export class SocialFeedExample {
  async handler() {
    return {
      success: true,
      items: [
        {
          cardType: "social",
          title: "Example Social card",
          source: "example-source",
          metadata: {
            // Required keys for social go here.
            // See README.md in this folder for exact enforced shape.
          },
        },
      ],
    };
  }
}

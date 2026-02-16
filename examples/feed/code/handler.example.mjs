// Code feed handler example.
// Focus: canonical type-safe structure so feed cards render cleanly.

export class CodeFeedExample {
  async handler() {
    return {
      success: true,
      items: [
        {
          cardType: "code",
          title: "Example Code card",
          source: "example-source",
          metadata: {
            // Required keys for code go here.
            // See README.md in this folder for exact enforced shape.
          },
        },
      ],
    };
  }
}

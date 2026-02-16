// Poll feed handler example.
// Focus: canonical type-safe structure so feed cards render cleanly.

export class PollFeedExample {
  async handler() {
    return {
      success: true,
      items: [
        {
          cardType: "poll",
          title: "Example Poll card",
          source: "example-source",
          metadata: {
            // Required keys for poll go here.
            // See README.md in this folder for exact enforced shape.
          },
        },
      ],
    };
  }
}

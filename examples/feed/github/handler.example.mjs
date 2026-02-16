// Github feed handler example.
// Focus: canonical type-safe structure so feed cards render cleanly.

export class GithubFeedExample {
  async handler() {
    return {
      success: true,
      items: [
        {
          cardType: "github",
          title: "Example Github card",
          source: "example-source",
          metadata: {
            // Required keys for github go here.
            // See README.md in this folder for exact enforced shape.
          },
        },
      ],
    };
  }
}

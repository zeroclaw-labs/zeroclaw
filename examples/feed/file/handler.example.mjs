// File feed handler example.
// Focus: canonical type-safe structure so feed cards render cleanly.

export class FileFeedExample {
  async handler() {
    return {
      success: true,
      items: [
        {
          cardType: "file",
          title: "Example File card",
          source: "example-source",
          metadata: {
            // Required keys for file go here.
            // See README.md in this folder for exact enforced shape.
          },
        },
      ],
    };
  }
}

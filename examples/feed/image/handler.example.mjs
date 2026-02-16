// Image feed handler example.
// Focus: canonical type-safe structure so feed cards render cleanly.

export class ImageFeedExample {
  async handler() {
    return {
      success: true,
      items: [
        {
          cardType: "image",
          title: "Example Image card",
          source: "example-source",
          metadata: {
            // Required keys for image go here.
            // See README.md in this folder for exact enforced shape.
          },
        },
      ],
    };
  }
}

// Video feed handler example.
// Focus: canonical type-safe structure so feed cards render cleanly.

export class VideoFeedExample {
  async handler() {
    return {
      success: true,
      items: [
        {
          cardType: "video",
          title: "Example Video card",
          source: "example-source",
          metadata: {
            // Required keys for video go here.
            // See README.md in this folder for exact enforced shape.
          },
        },
      ],
    };
  }
}

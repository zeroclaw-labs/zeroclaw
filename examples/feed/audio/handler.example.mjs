// Audio feed handler example.
// Focus: canonical type-safe structure so feed cards render cleanly.

export class AudioFeedExample {
  async handler() {
    return {
      success: true,
      items: [
        {
          cardType: "audio",
          title: "Example Audio card",
          source: "example-source",
          metadata: {
            // Required keys for audio go here.
            // See README.md in this folder for exact enforced shape.
          },
        },
      ],
    };
  }
}

// Weather feed handler example.
// Focus: canonical type-safe structure so feed cards render cleanly.

export class WeatherFeedExample {
  async handler() {
    return {
      success: true,
      items: [
        {
          cardType: "weather",
          title: "Example Weather card",
          source: "example-source",
          metadata: {
            // Required keys for weather go here.
            // See README.md in this folder for exact enforced shape.
          },
        },
      ],
    };
  }
}

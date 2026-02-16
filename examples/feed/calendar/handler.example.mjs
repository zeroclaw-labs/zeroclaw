// Calendar feed handler example.
// Focus: canonical type-safe structure so feed cards render cleanly.

export class CalendarFeedExample {
  async handler() {
    return {
      success: true,
      items: [
        {
          cardType: "calendar",
          title: "Example Calendar card",
          source: "example-source",
          metadata: {
            // Required keys for calendar go here.
            // See README.md in this folder for exact enforced shape.
          },
        },
      ],
    };
  }
}

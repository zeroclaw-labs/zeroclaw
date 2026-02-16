// Webview feed handler example.
// Focus: canonical type-safe structure so feed cards render cleanly.

export class WebviewFeedExample {
  async handler() {
    return {
      success: true,
      items: [
        {
          cardType: "webview",
          title: "Example Webview card",
          source: "example-source",
          metadata: {
            // Required keys for webview go here.
            // See README.md in this folder for exact enforced shape.
          },
        },
      ],
    };
  }
}

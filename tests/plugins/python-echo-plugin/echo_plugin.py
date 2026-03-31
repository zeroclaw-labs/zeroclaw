"""Python echo plugin — accepts JSON input and returns it unchanged."""

from zeroclaw_plugin_sdk import plugin_fn


@plugin_fn
def tool_echo(input):
    """Accepts any JSON object and returns it unchanged."""
    return input

"""SDK Example Plugin — "smart greeter" (Python)

Demonstrates a meaningful workflow using all four SDK modules:

1. **context.session** — reads the current channel and conversation ID
2. **memory.recall** — checks if we've greeted this conversation before
3. **memory.store** — remembers the greeting so we don't repeat it
4. **tools.tool_call** — delegates to a tool to look up a fun fact
5. **messaging.get_channels** — lists available channels for the greeting

When invoked, the plugin greets the user with a personalised message
that includes session context, available channels, and for first-time
conversations fetches a fun fact via tool delegation.
"""

from zeroclaw_plugin_sdk import plugin_fn
from zeroclaw_plugin_sdk import context, memory, messaging, tools


@plugin_fn
def tool_greet(input):
    """Greet the user with session context, memory, messaging, and tool delegation."""
    name = (input or {}).get("name", "") or "friend"

    # 1. Get session context
    session = context.session()

    # 2. Check if we've seen this conversation before
    memory_key = f"greeted:{session.conversation_id}"
    try:
        previous = memory.recall(memory_key)
    except Exception:
        previous = ""
    first_visit = not previous

    # 3. Query available channels via messaging module
    try:
        channels = messaging.get_channels()
    except Exception:
        channels = []

    # 4. Build the greeting
    greeting = (
        f"Hello, {name}! You're on the {session.channel_name} channel "
        f"(conversation {session.conversation_id})."
    )

    if channels:
        greeting += f" Available channels: {', '.join(channels)}."

    if first_visit:
        # 5. For first visits, fetch a fun fact via tool delegation
        try:
            fact = tools.tool_call("fun_fact", {"topic": "greeting"})
        except Exception:
            fact = "Waving as a greeting dates back to ancient times!"

        greeting += f" Welcome! Here's a fun fact: {fact}"

        # Remember this conversation
        try:
            memory.store(memory_key, name)
        except Exception:
            pass
    else:
        greeting += " Welcome back!"

    return {
        "greeting": greeting,
        "channel": session.channel_name,
        "conversation_id": session.conversation_id,
        "first_visit": first_visit,
        "available_channels": channels,
    }

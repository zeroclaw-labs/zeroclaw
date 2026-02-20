# JavaScript Plugin Hooks and Events

This document describes the hooks and events system for ZeroClaw JavaScript plugins.

## Table of Contents

- [Overview](#overview)
- [Event System Architecture](#event-system-architecture)
- [Available Events](#available-events)
- [Registering Hooks](#registering-hooks)
- [Permission Requirements](#permission-requirements)
- [Hook Priority](#hook-priority)
- [Event Payloads](#event-payloads)
- [Security Considerations](#security-considerations)
- [Best Practices](#best-practices)

## Overview

The ZeroClaw hooks and events system allows plugins to observe and react to specific lifecycle and runtime moments in the agent. Plugins can register handlers for events such as:

- When a message is received from a channel
- Before/after tool calls
- When LLM requests are made
- When the agent starts

Each hook is a JavaScript function that receives an event payload and can optionally return a value that affects the event flow.

## Event System Architecture

The event system consists of three main components:

1. **EventBus** - Core event broadcasting system using tokio channels
2. **HookRegistry** - Stores plugin handlers with priority-based ordering
3. **PluginEventObserver** - Bridges core `ObserverEvent` to JS plugin events

```
┌─────────────────┐
│  Core Runtime   │
│  (Agent Loop)   │
└────────┬────────┘
         │ emits ObserverEvent
         ▼
┌─────────────────┐
│   Event Bus     │
│  (broadcast)    │
└────────┬────────┘
         │
         ▼
┌─────────────────┐
│ Hook Registry   │
│ (priority sort) │
└────────┬────────┘
         │ dispatches to
         ▼
┌─────────────────┐
│  Plugin Worker  │
│  (JS execution) │
└─────────────────┘
```

## Available Events

| Event Name | Payload | Can Modify | Can Veto | Description |
|------------|---------|------------|----------|-------------|
| `message.received` | {channel_id, channel_type, message, session_id} | No | No | Message received from a channel |
| `tool.call.pre` | {tool_name, input, session_id} | Yes (input) | Yes (error) | Before tool execution |
| `tool.call.post` | {tool_name, result, session_id} | No | No | After tool execution |
| `llm.request` | {provider, model, messages, options} | Yes (messages) | No | LLM request being sent |
| `session.update` | {session_id, context} | No | No | Session context updated |
| `before.agent.start` | {config} | No | No | Agent about to start |
| `*` (custom) | {namespace, name, payload} | Varies | No | Custom plugin events |

## Registering Hooks

Hooks are registered in your plugin's entry point using the `Zeroclaw.on()` global function.

### Basic Hook Registration

```javascript
// Register a handler for message.received events
Zeroclaw.on('message.received', async (msg) => {
    console.log('Message received:', msg.content);

    // React to the message
    if (msg.content.startsWith('!ping')) {
        await msg.session.reply('Pong!');
    }
});
```

### Hook with Options

In your `plugin.toml`, declare the hook with priority and timeout:

```toml
[[hooks]]
name = "message.received"
priority = 100
timeout_ms = 8000

[permissions]
hooks = ["message.received"]
```

Then register in your entry point:

```javascript
// src/index.ts
Zeroclaw.on('message.received', async (msg) => {
    // Your handler logic
});
```

### Multiple Hooks for Same Event

```javascript
// High priority handler (runs first)
Zeroclaw.on('message.received', async (msg) => {
    // Pre-processing
}, { priority: 100 });

// Lower priority handler (runs later)
Zeroclaw.on('message.call.pre', async (data) => {
    // Post-processing
}, { priority: 50 });
```

## Permission Requirements

Plugins must declare which events they want to hook in the `permissions.hooks` array of `plugin.toml`. The permission system supports:

- Exact match: `"message.received"` matches only that event
- Wildcard: `"*"` matches all events
- Prefix pattern: `"message.*"` matches `message.received`, `message.sent`, etc.

### Example Permissions

```toml
[permissions]
# Allow specific events
hooks = ["message.received", "tool.call.pre", "tool.call.post"]

# Allow all message events
hooks = ["message.*"]

# Allow all events (use carefully)
hooks = ["*"]

# Multiple patterns
hooks = ["message.*", "tool.call.*", "llm.request"]
```

### Validation

The manifest validation ensures declared hooks are allowed by permissions:

```toml
[[hooks]]
name = "message.received"  # Must be in permissions.hooks
priority = 50

[permissions]
hooks = ["message.received"]  # Validation passes
```

Attempting to register a hook without permission fails validation:

```toml
[[hooks]]
name = "tool.call.pre"  # NOT in permissions.hooks
priority = 50

[permissions]
hooks = ["message.received"]  # Validation fails
```

## Hook Priority

Hooks execute in priority order (higher priority = runs earlier). Default priority is 50.

### Priority Levels

- `0-49`: Low priority (runs last)
- `50`: Default priority
- `51-100`: High priority (runs first)
- `100+`: Critical priority (runs before everything)

### Example Priority Ordering

```toml
[[hooks]]
name = "message.received"
priority = 100  # Runs first (validation/logging)

[[hooks]]
name = "message.received"
priority = 50   # Runs second (default behavior)

[[hooks]]
name = "message.received"
priority = 10   # Runs last (cleanup/stats)
```

### Deterministic Execution

Within the same priority level, hooks execute in plugin ID order (alphabetical) for deterministic behavior.

## Event Payloads

### message.received

Emitted when a message is received from any configured channel.

```javascript
{
    channelId: "123456789",
    channelType: "discord",
    message: {
        id: "msg-abc",
        content: "Hello bot",
        sender: {
            id: "user-123",
            username: "zeroclaw_user",
            // Channel-specific fields
        },
        // Additional channel-specific data
    },
    sessionId: "session-xyz"
}
```

**Usage Example:**

```javascript
Zeroclaw.on('message.received', async (msg) => {
    const session = msg.session;

    // Reply to the message
    if (msg.content === 'hello') {
        await session.reply('Hi there!');
    }

    // Start typing indicator
    await session.startTyping();

    // Do some work
    await processAsync();

    // Stop typing indicator
    await session.stopTyping();
});
```

### tool.call.pre

Emitted before a tool is executed. Can modify input or veto execution.

```javascript
{
    toolName: "search",
    input: {
        query: "rust language",
        limit: 10
    },
    sessionId: "session-xyz"
}
```

**Usage Example (Modify Input):**

```javascript
Zeroclaw.on('tool.call.pre', async (data) => {
    if (data.toolName === 'search') {
        // Enforce a maximum limit
        if (data.input.limit > 50) {
            data.input.limit = 50;
        }
    }
});
```

**Usage Example (Veto Execution):**

```javascript
Zeroclaw.on('tool.call.pre', async (data) => {
    if (data.toolName === 'delete_file' && isProtected(data.input.path)) {
        throw new Error('Protected file: deletion vetoed');
    }
});
```

### tool.call.post

Emitted after a tool completes execution. Cannot modify result.

```javascript
{
    toolName: "search",
    result: {
        success: true,
        output: "Rust is a systems language...",
        error: null
    },
    sessionId: "session-xyz"
}
```

**Usage Example:**

```javascript
Zeroclaw.on('tool.call.post', async (data) => {
    if (!data.result.success) {
        console.error(`Tool ${data.toolName} failed:`, data.result.error);
        // Log or handle the error
    }
});
```

### llm.request

Emitted when an LLM request is being sent to the provider. Can modify messages.

```javascript
{
    provider: "openai",
    model: "gpt-4",
    messages: [
        { role: "system", content: "You are a helpful assistant." },
        { role: "user", content: "Hello!" }
    ],
    options: {
        temperature: 0.7,
        max_tokens: 1000
    }
}
```

**Usage Example (Inject System Context):**

```javascript
Zeroclaw.on('llm.request', async (data) => {
    // Add custom system instructions
    data.messages.unshift({
        role: 'system',
        content: 'Always respond in a friendly tone.'
    });
});
```

### session.update

Emitted when the session context is updated.

```javascript
{
    sessionId: "session-xyz",
    context: {
        userId: "user-123",
        channelId: "123456789",
        metadata: {}
    }
}
```

### before.agent.start

Emitted when the agent is about to start processing.

```javascript
{
    config: {
        provider: "openai",
        model: "gpt-4",
        channel: "discord",
        // Additional config
    }
}
```

**Usage Example:**

```javascript
Zeroclaw.on('before.agent.start', async (data) => {
    console.log('Agent starting with config:', data.config);
    // Perform initialization
});
```

### Custom Events

Plugins can emit and listen to custom events using a namespace.

```javascript
// Emit a custom event
Zeroclaw.emit('com.example.plugin', 'custom.event', {
    data: 'value'
});

// Listen to custom events
Zeroclaw.on('com.example.plugin:custom.event', async (payload) => {
    console.log('Custom event:', payload);
});
```

## Security Considerations

### Sensitive Data Handling

Event payloads may contain sensitive information:

- `message.received`: User messages, PII, potential secrets
- `tool.call.pre`: Tool inputs with parameters or secrets
- `tool.call.post`: Tool outputs with sensitive data
- `llm.request`: Prompts containing secrets or PII

**Security Requirements:**

```javascript
// DO NOT log raw event payloads
Zeroclaw.on('message.received', async (msg) => {
    console.log(msg.content);  // WRONG: logs user data

    // CORRECT: log only non-sensitive metadata
    console.log(`Message from ${msg.channelId}`);
});
```

### Permission Enforcement

Hooks are validated before registration:

1. Plugin declares hook in `[[hooks]]` section
2. Hook name must be in `permissions.hooks`
3. Wildcard patterns expand to match specific events
4. Validation fails if any declared hook is not allowed

### Secure-by-Default

The permission system is deny-by-default:

- Empty `hooks` array means no events allowed
- Each event must be explicitly allowed
- Wildcards (`*`, `message.*`) provide convenient broad access

## Best Practices

### 1. Use Appropriate Priorities

```javascript
// Logging/stats: run last (low priority)
Zeroclaw.on('message.received', async (msg) => {
    stats.record('message');
}, { priority: 10 });

// Validation: run first (high priority)
Zeroclaw.on('tool.call.pre', async (data) => {
    validateInput(data.input);
}, { priority: 100 });
```

### 2. Set Reasonable Timeouts

```toml
[[hooks]]
name = "llm.request"
timeout_ms = 10000  # 10 seconds for LLM hook

[[hooks]]
name = "message.received"
timeout_ms = 5000   # 5 seconds for message hook
```

### 3. Handle Errors Gracefully

```javascript
Zeroclaw.on('tool.call.post', async (data) => {
    try {
        await processResult(data.result);
    } catch (error) {
        console.error('Hook error:', error);
        // Don't throw: other hooks should still run
    }
});
```

### 4. Avoid Long-Running Operations

```javascript
// BAD: blocks hook execution
Zeroclaw.on('message.received', async (msg) => {
    await longRunningTask();  // May timeout
});

// GOOD: schedule async work
Zeroclaw.on('message.received', async (msg) => {
    setImmediate(() => longRunningTask());
});
```

### 5. Use Specific Event Patterns

```toml
# PREFER: specific patterns
hooks = ["message.received", "tool.call.pre"]

# AVOID: overly broad wildcards
hooks = ["*"]  # Use only when truly needed
```

## Related Documentation

- [JavaScript Plugin Authoring Guide](js-plugin-authoring-guide.md)
- [Plugin Manifest Reference](#) (TBD)
- [Security Best Practices](../security/README.md)

## API Reference

### Zeroclaw.on(event, handler)

Register a hook for an event.

- `event` (string): Event name (e.g., "message.received")
- `handler` (function): Async function to call when event occurs

### Zeroclaw.emit(namespace, name, payload)

Emit a custom event.

- `namespace` (string): Event namespace (e.g., "com.example.plugin")
- `name` (string): Event name
- `payload` (object): Event data

## Changelog

### v2.0.0 (Current)

- Added hooks and events system
- Extended manifest with `[[hooks]]` declarations
- Extended permissions with `hooks`, `apis`, `channels` arrays
- Implemented priority-based hook ordering
- Added permission validation for hooks
- Changed `file_write` from bool to glob patterns array

# zeroclaw-plugin-sdk (Python)

Python SDK for building ZeroClaw WASM plugins with Extism PDK.

## Quickstart

### Prerequisites

- Python 3.10+
- [Extism CLI](https://extism.org/docs/install/) (for compiling WASM plugins)

### Install

```bash
pip install -e sdks/python
```

Or add to your project's dependencies:

```toml
[project]
dependencies = [
    "zeroclaw-plugin-sdk",
]
```

### Write a plugin

Create a Python file with a `@plugin_fn`-decorated function. The decorator handles JSON serialization/deserialization over Extism shared memory automatically.

```python
from zeroclaw_plugin_sdk import plugin_fn

@plugin_fn
def greet(input):
    name = input.get("name", "world")
    return {"message": f"Hello, {name}!"}
```

The decorated function:
- Receives a parsed JSON object from Extism input
- Returns a value that is JSON-serialized back to Extism output

### Run tests

```bash
pip install -e "sdks/python[dev]"
pytest sdks/python/tests/
```

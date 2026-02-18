"""
File read/write tools.
"""

import os

from langchain_core.tools import tool


MAX_FILE_SIZE = 100_000


@tool
def file_read(path: str) -> str:
    """
    Read the contents of a file at the given path.

    Args:
        path: The file path to read (absolute or relative)

    Returns:
        The file contents, or an error message
    """
    try:
        with open(path, "r", encoding="utf-8", errors="replace") as f:
            content = f.read()
            if len(content) > MAX_FILE_SIZE:
                return content[:MAX_FILE_SIZE] + f"\n... (truncated, {len(content)} bytes total)"
            return content
    except FileNotFoundError:
        return f"Error: File not found: {path}"
    except PermissionError:
        return f"Error: Permission denied: {path}"
    except Exception as e:
        return f"Error: {e}"


@tool
def file_write(path: str, content: str) -> str:
    """
    Write content to a file, creating directories if needed.

    Args:
        path: The file path to write to
        content: The content to write

    Returns:
        Success message or error
    """
    try:
        parent = os.path.dirname(path)
        if parent:
            os.makedirs(parent, exist_ok=True)
        with open(path, "w", encoding="utf-8") as f:
            f.write(content)
        return f"Successfully wrote {len(content)} bytes to {path}"
    except PermissionError:
        return f"Error: Permission denied: {path}"
    except Exception as e:
        return f"Error: {e}"

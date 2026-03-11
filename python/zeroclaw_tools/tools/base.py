"""
Base utilities for creating tools.
"""

from typing import Any, Callable, Optional

from langchain_core.tools import tool as langchain_tool


def tool(
    func: Optional[Callable] = None,
    *,
    name: Optional[str] = None,
    description: Optional[str] = None,
) -> Any:
    """
    Decorator to create a LangChain tool from a function.

    This is a convenience wrapper around langchain_core.tools.tool that
    provides a simpler interface for ZeroClaw users.

    Args:
        func: The function to wrap (when used without parentheses)
        name: Optional custom name for the tool
        description: Optional custom description

    Returns:
        A BaseTool instance

    Example:
        ```python
        from zeroclaw_tools import tool

        @tool
        def my_tool(query: str) -> str:
            \"\"\"Description of what this tool does.\"\"\"
            return f"Result: {query}"
        ```
    """
    if func is not None:
        if name is not None:
            return langchain_tool(name, func, description=description)
        return langchain_tool(func, description=description)

    def decorator(f: Callable) -> Any:
        if name is not None:
            return langchain_tool(name, f, description=description)
        return langchain_tool(f, description=description)

    return decorator

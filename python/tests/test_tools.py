"""
Tests for zeroclaw-tools package.
"""

import pytest


def test_import_main():
    """Test that main package imports work."""
    from zeroclaw_tools import create_agent, shell, file_read, file_write

    assert callable(create_agent)
    assert hasattr(shell, "invoke")
    assert hasattr(file_read, "invoke")
    assert hasattr(file_write, "invoke")


def test_import_tool_decorator():
    """Test that tool decorator works."""
    from zeroclaw_tools import tool

    @tool
    def test_func(x: str) -> str:
        """Test tool."""
        return x

    assert hasattr(test_func, "invoke")


def test_agent_creation():
    """Test that agent can be created with default tools."""
    from zeroclaw_tools import create_agent, shell, file_read, file_write

    agent = create_agent(
        tools=[shell, file_read, file_write], model="test-model", api_key="test-key"
    )

    assert agent is not None
    assert agent.model == "test-model"


@pytest.mark.asyncio
async def test_shell_tool():
    """Test shell tool execution."""
    from zeroclaw_tools import shell

    result = await shell.ainvoke({"command": "echo hello"})
    assert "hello" in result


@pytest.mark.asyncio
async def test_file_tools(tmp_path):
    """Test file read/write tools."""
    from zeroclaw_tools import file_read, file_write

    test_file = tmp_path / "test.txt"

    write_result = await file_write.ainvoke({"path": str(test_file), "content": "Hello, World!"})
    assert "Successfully" in write_result

    read_result = await file_read.ainvoke({"path": str(test_file)})
    assert "Hello, World!" in read_result

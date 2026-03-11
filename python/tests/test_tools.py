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


def test_tool_decorator_custom_metadata():
    """Test that custom tool metadata is preserved."""
    from zeroclaw_tools import tool

    @tool(name="echo_tool", description="Echo input back")
    def echo(value: str) -> str:
        return value

    assert echo.name == "echo_tool"
    assert "Echo input back" in echo.description


def test_agent_creation():
    """Test that agent can be created with default tools."""
    from zeroclaw_tools import create_agent, shell, file_read, file_write

    agent = create_agent(
        tools=[shell, file_read, file_write], model="test-model", api_key="test-key"
    )

    assert agent is not None
    assert agent.model == "test-model"


def test_cli_allows_interactive_without_message():
    """Interactive mode should not require positional message."""
    from zeroclaw_tools.__main__ import parse_args

    args = parse_args(["-i"])

    assert args.interactive is True
    assert args.message == []


def test_cli_requires_message_when_not_interactive():
    """Non-interactive mode requires at least one message token."""
    from zeroclaw_tools.__main__ import parse_args

    with pytest.raises(SystemExit):
        parse_args([])


@pytest.mark.asyncio
async def test_invoke_in_event_loop_raises():
    """invoke() should fail fast when called from an active event loop."""
    from zeroclaw_tools import create_agent, shell

    agent = create_agent(tools=[shell], model="test-model", api_key="test-key")

    with pytest.raises(RuntimeError, match="ainvoke"):
        agent.invoke({"messages": []})


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

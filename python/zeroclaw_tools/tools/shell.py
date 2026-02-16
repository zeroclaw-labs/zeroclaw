"""
Shell execution tool.
"""

import subprocess

from langchain_core.tools import tool


@tool
def shell(command: str) -> str:
    """
    Execute a shell command and return the output.

    Args:
        command: The shell command to execute

    Returns:
        The command output (stdout and stderr combined)
    """
    try:
        result = subprocess.run(command, shell=True, capture_output=True, text=True, timeout=60)
        output = result.stdout
        if result.stderr:
            output += f"\nSTDERR: {result.stderr}"
        if result.returncode != 0:
            output += f"\nExit code: {result.returncode}"
        return output or "(no output)"
    except subprocess.TimeoutExpired:
        return "Error: Command timed out after 60 seconds"
    except Exception as e:
        return f"Error: {e}"

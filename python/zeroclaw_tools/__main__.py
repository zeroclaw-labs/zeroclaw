"""
CLI entry point for zeroclaw-tools.
"""

import argparse
import asyncio
import os
import sys
from typing import Optional

from langchain_core.messages import HumanMessage

from .agent import create_agent
from .tools import (
    shell,
    file_read,
    file_write,
    web_search,
    http_request,
    memory_store,
    memory_recall,
)


DEFAULT_SYSTEM_PROMPT = """You are ZeroClaw, an AI assistant with full system access. Use tools to accomplish tasks.
Be concise and helpful. Execute tools directly without excessive explanation."""


async def chat(message: str, api_key: str, base_url: Optional[str], model: str) -> str:
    """Run a single chat message through the agent."""
    agent = create_agent(
        tools=[shell, file_read, file_write, web_search, http_request, memory_store, memory_recall],
        model=model,
        api_key=api_key,
        base_url=base_url,
        system_prompt=DEFAULT_SYSTEM_PROMPT,
    )

    result = await agent.ainvoke({"messages": [HumanMessage(content=message)]})
    return result["messages"][-1].content or "Done."


def _build_parser() -> argparse.ArgumentParser:
    """Build CLI argument parser."""
    parser = argparse.ArgumentParser(
        description="ZeroClaw Tools - LangGraph-based tool calling for LLMs"
    )
    parser.add_argument(
        "message",
        nargs="*",
        help="Message to send to the agent (optional in interactive mode)",
    )
    parser.add_argument("--model", "-m", default="glm-5", help="Model to use")
    parser.add_argument("--api-key", "-k", default=None, help="API key")
    parser.add_argument("--base-url", "-u", default=None, help="API base URL")
    parser.add_argument("--interactive", "-i", action="store_true", help="Interactive mode")
    return parser


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    """Parse CLI arguments and enforce mode-specific requirements."""
    parser = _build_parser()
    args = parser.parse_args(argv)

    if not args.interactive and not args.message:
        parser.error("message is required unless --interactive is set")

    return args


def main(argv: list[str] | None = None):
    """CLI main entry point."""
    args = parse_args(argv)

    api_key = args.api_key or os.environ.get("API_KEY") or os.environ.get("GLM_API_KEY")
    base_url = args.base_url or os.environ.get("API_BASE")

    if not api_key:
        print("Error: API key required. Set API_KEY env var or use --api-key", file=sys.stderr)
        sys.exit(1)

    if args.interactive:
        print("ZeroClaw Tools CLI (Interactive Mode)")
        print("Type 'exit' to quit\n")

        agent = create_agent(
            tools=[
                shell,
                file_read,
                file_write,
                web_search,
                http_request,
                memory_store,
                memory_recall,
            ],
            model=args.model,
            api_key=api_key,
            base_url=base_url,
            system_prompt=DEFAULT_SYSTEM_PROMPT,
        )

        history = []

        while True:
            try:
                user_input = input("You: ").strip()
                if not user_input:
                    continue
                if user_input.lower() in ["exit", "quit", "q"]:
                    print("Goodbye!")
                    break

                history.append(HumanMessage(content=user_input))

                result = asyncio.run(agent.ainvoke({"messages": history}))

                for msg in result["messages"][len(history) :]:
                    history.append(msg)

                response = result["messages"][-1].content or "Done."
                print(f"\nZeroClaw: {response}\n")

            except KeyboardInterrupt:
                print("\nGoodbye!")
                break
    else:
        message = " ".join(args.message)
        result = asyncio.run(chat(message, api_key, base_url, args.model))
        print(result)


if __name__ == "__main__":
    main()

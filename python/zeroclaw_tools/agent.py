"""
LangGraph-based agent factory for consistent tool calling.
"""

import os
from typing import Any, Optional

from langchain_core.messages import HumanMessage, SystemMessage
from langchain_core.tools import BaseTool
from langchain_openai import ChatOpenAI
from langgraph.graph import StateGraph, MessagesState, END
from langgraph.prebuilt import ToolNode


SYSTEM_PROMPT = """You are ZeroClaw, an AI assistant with tool access. Use tools to accomplish tasks.
Be concise and helpful. Execute tools directly when needed without excessive explanation."""
GLM_DEFAULT_BASE_URL = "https://api.z.ai/api/coding/paas/v4"


class ZeroclawAgent:
    """
    LangGraph-based agent with consistent tool calling behavior.

    This agent wraps an LLM with LangGraph's tool execution loop, ensuring
    reliable tool calling even with providers that have inconsistent native
    tool calling support.
    """

    def __init__(
        self,
        tools: list[BaseTool],
        model: str = "glm-5",
        api_key: Optional[str] = None,
        base_url: Optional[str] = None,
        temperature: float = 0.7,
        system_prompt: Optional[str] = None,
    ):
        self.tools = tools
        self.model = model
        self.temperature = temperature
        self.system_prompt = system_prompt or SYSTEM_PROMPT

        api_key = api_key or os.environ.get("API_KEY") or os.environ.get("GLM_API_KEY")
        base_url = base_url or os.environ.get("API_BASE")

        if base_url is None and model.lower().startswith(("glm", "zhipu")):
            base_url = GLM_DEFAULT_BASE_URL

        if not api_key:
            raise ValueError(
                "API key required. Set API_KEY environment variable or pass api_key parameter."
            )

        self.llm = ChatOpenAI(
            model=model,
            api_key=api_key,
            base_url=base_url,
            temperature=temperature,
        ).bind_tools(tools)

        self._graph = self._build_graph()

    def _build_graph(self) -> StateGraph:
        """Build the LangGraph execution graph."""
        tool_node = ToolNode(self.tools)

        def should_continue(state: MessagesState) -> str:
            messages = state["messages"]
            last_message = messages[-1]
            if hasattr(last_message, "tool_calls") and last_message.tool_calls:
                return "tools"
            return END

        async def call_model(state: MessagesState) -> dict:
            response = await self.llm.ainvoke(state["messages"])
            return {"messages": [response]}

        workflow = StateGraph(MessagesState)
        workflow.add_node("agent", call_model)
        workflow.add_node("tools", tool_node)
        workflow.set_entry_point("agent")
        workflow.add_conditional_edges("agent", should_continue, {"tools": "tools", END: END})
        workflow.add_edge("tools", "agent")

        return workflow.compile()

    async def ainvoke(self, input: dict[str, Any], config: Optional[dict] = None) -> dict:
        """
        Asynchronously invoke the agent.

        Args:
            input: Dict with "messages" key containing list of messages
            config: Optional LangGraph config

        Returns:
            Dict with "messages" key containing the conversation
        """
        messages = input.get("messages", [])

        if messages and isinstance(messages[0], HumanMessage):
            if not any(isinstance(m, SystemMessage) for m in messages):
                messages = [SystemMessage(content=self.system_prompt)] + messages

        return await self._graph.ainvoke({"messages": messages}, config)

    def invoke(self, input: dict[str, Any], config: Optional[dict] = None) -> dict:
        """
        Synchronously invoke the agent.
        """
        import asyncio

        try:
            asyncio.get_running_loop()
        except RuntimeError:
            return asyncio.run(self.ainvoke(input, config))

        raise RuntimeError(
            "ZeroclawAgent.invoke() cannot be called inside an active event loop. "
            "Use 'await ZeroclawAgent.ainvoke(...)' instead."
        )


def create_agent(
    tools: Optional[list[BaseTool]] = None,
    model: str = "glm-5",
    api_key: Optional[str] = None,
    base_url: Optional[str] = None,
    temperature: float = 0.7,
    system_prompt: Optional[str] = None,
) -> ZeroclawAgent:
    """
    Create a ZeroClaw agent with LangGraph-based tool calling.

    Args:
        tools: List of tools. Defaults to shell, file_read, file_write.
        model: Model name to use
        api_key: API key for the provider
        base_url: Base URL for the provider API
        temperature: Sampling temperature
        system_prompt: Custom system prompt

    Returns:
        Configured ZeroclawAgent instance

    Example:
        ```python
        from zeroclaw_tools import create_agent, shell, file_read
        from langchain_core.messages import HumanMessage

        agent = create_agent(
            tools=[shell, file_read],
            model="glm-5",
            api_key="your-key"
        )

        result = await agent.ainvoke({
            "messages": [HumanMessage(content="List files in /tmp")]
        })
        ```
    """
    if tools is None:
        from .tools import shell, file_read, file_write

        tools = [shell, file_read, file_write]

    return ZeroclawAgent(
        tools=tools,
        model=model,
        api_key=api_key,
        base_url=base_url,
        temperature=temperature,
        system_prompt=system_prompt,
    )

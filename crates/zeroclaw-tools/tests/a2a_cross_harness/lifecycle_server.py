#!/usr/bin/env python3
"""A local A2A v1 server supporting the full lifecycle.
Independent implementation (official a2a-sdk-python server stack) used to
verify our Rust outbound client's nonterminal send -> poll -> cancel flow.
"""
import asyncio
import uvicorn

from a2a.helpers import (
    new_task_from_user_message,
    new_text_message,
    new_text_part,
)
from a2a.server.agent_execution import AgentExecutor, RequestContext
from a2a.server.events import EventQueue
from a2a.server.request_handlers import DefaultRequestHandler
from a2a.server.routes import create_agent_card_routes, create_jsonrpc_routes
from a2a.server.tasks import InMemoryTaskStore, TaskUpdater
from a2a.types import (
    AgentCapabilities,
    AgentCard,
    AgentInterface,
    AgentSkill,
    Task,
    TaskState,
)
from starlette.applications import Starlette


class LifecycleAgentExecutor(AgentExecutor):
    """Agent whose task enters WORKING and stays there until canceled."""

    async def execute(self, context: RequestContext, event_queue: EventQueue) -> None:
        # Enqueue the task first, then mark it WORKING (non-terminal), matching
        # the official a2a-sdk-python execution contract.
        if context.current_task:
            task = context.current_task
        else:
            task = new_task_from_user_message(context.message)
            await event_queue.enqueue_event(task)
        task_updater = TaskUpdater(
            event_queue=event_queue, task_id=task.id, context_id=task.context_id
        )
        await task_updater.update_status(
            state=TaskState.TASK_STATE_WORKING,
            message=new_text_message("task started, processing..."),
        )
        # Simulate a long-running task that never completes on its own (the
        # test cancels it via CancelTask; this proves the nonterminal poll).
        await asyncio.sleep(3600)

    async def cancel(self, context: RequestContext, event_queue: EventQueue) -> Task | None:
        tid = context.task_id
        # Mark the task CANCELED so a later GetTask observes the terminal state.
        task_updater = TaskUpdater(
            event_queue=event_queue, task_id=tid, context_id=context.context_id
        )
        await task_updater.update_status(
            state=TaskState.TASK_STATE_CANCELED,
            message=new_text_message("task canceled"),
        )
        return None


def build_server() -> Starlette:
    skill = AgentSkill(
        id="lifecycle",
        name="Lifecycle",
        description="long-running lifecycle task",
        input_modes=["text/plain"],
        output_modes=["text/plain"],
    )
    card = AgentCard(
        name="Lifecycle Agent",
        description="independent async lifecycle agent",
        version="1.0",
        default_input_modes=["text/plain"],
        default_output_modes=["text/plain"],
        capabilities=AgentCapabilities(),
        supported_interfaces=[
            AgentInterface(
                protocol_binding="JSONRPC",
                url="http://127.0.0.1:43100",
                protocol_version="1.0",
            )
        ],
        skills=[skill],
    )
    handler = DefaultRequestHandler(
        agent_executor=LifecycleAgentExecutor(),
        task_store=InMemoryTaskStore(),
        agent_card=card,
    )
    routes = []
    routes.extend(create_agent_card_routes(card))
    routes.extend(create_jsonrpc_routes(handler, "/"))
    return Starlette(routes=routes)


if __name__ == "__main__":
    uvicorn.run(build_server(), host="127.0.0.1", port=43100)
"""
Discord bot integration for ZeroClaw.
"""

import os
from typing import Optional, Set

try:
    import discord

    DISCORD_AVAILABLE = True
except ImportError:
    DISCORD_AVAILABLE = False
    discord = None

from langchain_core.messages import HumanMessage

from ..agent import create_agent
from ..tools import shell, file_read, file_write, web_search


class DiscordBot:
    """
    Discord bot powered by ZeroClaw agent with LangGraph tool calling.

    Example:
        ```python
        import os
        from zeroclaw_tools.integrations import DiscordBot

        bot = DiscordBot(
            token=os.environ["DISCORD_TOKEN"],
            guild_id=123456789,
            allowed_users=["123456789"],
            api_key=os.environ["API_KEY"]
        )

        bot.run()
        ```
    """

    def __init__(
        self,
        token: str,
        guild_id: int,
        allowed_users: list[str],
        api_key: Optional[str] = None,
        base_url: Optional[str] = None,
        model: str = "glm-5",
        prefix: str = "",
    ):
        if not DISCORD_AVAILABLE:
            raise ImportError(
                "discord.py is required for Discord integration. "
                "Install with: pip install zeroclaw-tools[discord]"
            )

        self.token = token
        self.guild_id = guild_id
        self.allowed_users: Set[str] = set(allowed_users)
        self.api_key = api_key or os.environ.get("API_KEY")
        self.base_url = base_url or os.environ.get("API_BASE")
        self.model = model
        self.prefix = prefix

        if not self.api_key:
            raise ValueError(
                "API key required. Set API_KEY environment variable or pass api_key parameter."
            )

        self.agent = create_agent(
            tools=[shell, file_read, file_write, web_search],
            model=self.model,
            api_key=self.api_key,
            base_url=self.base_url,
        )

        self._histories: dict[str, list] = {}
        self._max_history = 20

        intents = discord.Intents.default()
        intents.message_content = True
        intents.guilds = True

        self.client = discord.Client(intents=intents)
        self._setup_events()

    def _setup_events(self):
        @self.client.event
        async def on_ready():
            print(f"ZeroClaw Discord Bot ready: {self.client.user}")
            print(f"Guild: {self.guild_id}")
            print(f"Allowed users: {self.allowed_users}")

        @self.client.event
        async def on_message(message):
            if message.author == self.client.user:
                return

            if message.guild and message.guild.id != self.guild_id:
                return

            user_id = str(message.author.id)
            if user_id not in self.allowed_users:
                return

            content = message.content.strip()
            if not content:
                return

            if self.prefix and not content.startswith(self.prefix):
                return

            if self.prefix:
                content = content[len(self.prefix) :].strip()

            print(f"[{message.author}] {content[:50]}...")

            async with message.channel.typing():
                try:
                    response = await self._process_message(content, user_id)
                    for chunk in self._split_message(response):
                        await message.reply(chunk)
                except Exception as e:
                    print(f"Error: {e}")
                    await message.reply(f"Error: {e}")

    async def _process_message(self, content: str, user_id: str) -> str:
        """Process a message and return the response."""
        messages = []

        if user_id in self._histories:
            for msg in self._histories[user_id][-10:]:
                messages.append(msg)

        messages.append(HumanMessage(content=content))

        result = await self.agent.ainvoke({"messages": messages})

        if user_id not in self._histories:
            self._histories[user_id] = []
        self._histories[user_id].append(HumanMessage(content=content))

        for msg in result["messages"][len(messages) :]:
            self._histories[user_id].append(msg)

        self._histories[user_id] = self._histories[user_id][-self._max_history * 2 :]

        final = result["messages"][-1]
        return final.content or "Done."

    @staticmethod
    def _split_message(text: str, max_len: int = 1900) -> list[str]:
        """Split long messages for Discord's character limit."""
        if len(text) <= max_len:
            return [text]

        chunks = []
        while text:
            if len(text) <= max_len:
                chunks.append(text)
                break

            pos = text.rfind("\n", 0, max_len)
            if pos == -1:
                pos = text.rfind(" ", 0, max_len)
            if pos == -1:
                pos = max_len

            chunks.append(text[:pos].strip())
            text = text[pos:].strip()

        return chunks

    def run(self):
        """Start the Discord bot."""
        self.client.run(self.token)

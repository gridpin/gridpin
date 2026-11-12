"""GridPin offline geocoding as tools for agent frameworks: LlamaIndex, CrewAI, PydanticAI, smolagents and the
OpenAI Agents SDK. Each framework has its own module, imported only when you use it:

    from gridpin_agents.llamaindex import gridpin_tools     # FunctionTool
    from gridpin_agents.crewai import gridpin_tools         # BaseTool
    from gridpin_agents.pydantic_ai import gridpin_tools    # Tool
    from gridpin_agents.smolagents import gridpin_tools     # Tool
    from gridpin_agents.openai_agents import gridpin_tools  # FunctionTool

`gridpin_tools(country, sheet=None)` returns [geocode, reverse] for one country sheet. LangChain has its
own package, langchain-gridpin; the Vercel AI SDK tool is vercel/tool.ts in the source tree.
"""

from .core import MAX_K, Calls, GridPinError

__all__ = ["Calls", "GridPinError", "MAX_K"]
__version__ = "0.1.0"

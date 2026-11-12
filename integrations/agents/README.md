# gridpin-agents

Offline geocoding as tools for agent frameworks. Each tool reads one GridPin country sheet on your disk: the
address never leaves the machine, there is no API key and no rate limit. The answers are those of the GridPin
MCP server, field for field.

```sh
pip install "gridpin-agents[llamaindex]"   # or [crewai], [pydantic-ai], [smolagents], [openai-agents]
gridpin-fetch us                           # once: download and verify the United States sheet
```

| Framework | Import | Tool type |
|---|---|---|
| LlamaIndex | `from gridpin_agents.llamaindex import gridpin_tools` | `FunctionTool` |
| CrewAI | `from gridpin_agents.crewai import gridpin_tools` | `BaseTool` |
| PydanticAI | `from gridpin_agents.pydantic_ai import gridpin_tools` | `Tool` |
| smolagents | `from gridpin_agents.smolagents import gridpin_tools` | `Tool` |
| OpenAI Agents SDK | `from gridpin_agents.openai_agents import gridpin_tools` | `FunctionTool` |

`gridpin_tools("us")` returns two tools, `gridpin_geocode_us` and `gridpin_reverse_us`; pass `sheet=` to read a
file of your own. LangChain has its own package, `langchain-gridpin`. For the Vercel AI SDK see
`vercel/tool.ts` (it uses the `gridpin` npm package).

```python
from pydantic_ai import Agent
from gridpin_agents.pydantic_ai import gridpin_tools

agent = Agent("openai:gpt-5-mini", tools=gridpin_tools("us"))
print(agent.run_sync("Where is 1600 Pennsylvania Ave NW, Washington?").output)
```

Licence: Apache-2.0. The sheet's own licence and attribution come with the sheet (`gridpin meta`).

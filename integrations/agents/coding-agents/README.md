# GridPin instructions for coding agents

One text, three file names: copy the one your agent reads into your project.

| Agent | File | Where it goes |
|---|---|---|
| Claude Code | `integrations/claude-plugin/skills/geocode-addresses/SKILL.md` | the GridPin plugin installs it |
| Cursor | `gridpin.mdc` | `.cursor/rules/gridpin.mdc` |
| GitHub Copilot | `copilot-instructions.md` | `.github/copilot-instructions.md` (or append to yours) |

They assume the GridPin MCP server (`gridpin-mcp`) is configured in the agent.

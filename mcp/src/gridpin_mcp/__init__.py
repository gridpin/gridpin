"""GridPin for AI agents: an MCP server and one-command sheet downloads.

Python use without MCP:

    import gridpin
    from gridpin_mcp import sheets
    path = sheets.download("fr")          # once; verified against the signed release
    gridpin.Geocoder(str(path)).geocode("1 rue de la Paix, Paris", 1)
"""

__version__ = "0.2.0"

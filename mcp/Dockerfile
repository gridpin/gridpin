# GridPin MCP server: offline geocoding and reverse geocoding tools for AI agents, over stdio.
#
# Build context: this directory (mcp/). The package is installed from this source; the geocoding
# engine (gridpin) comes from PyPI as a prebuilt wheel for linux/amd64 and linux/arm64. Country
# files are downloaded at run time from https://dl.gridpin.dev into $GRIDPIN_HOME (/data, keep it
# on a volume so a country is downloaded once) and checked against the signed release before use.
# No API key, no configuration; queries are answered inside the container.
#
#   docker build -t gridpin-mcp mcp/
#   docker run -i --rm -v gridpin-sheets:/data gridpin-mcp
FROM python:3.12-slim@sha256:a6e34c598f2467ed0e9a8d349809fcd8b5c603269512df273a0bb1784edc11b1

ENV PYTHONDONTWRITEBYTECODE=1 \
    PYTHONUNBUFFERED=1 \
    PIP_NO_CACHE_DIR=1 \
    PIP_DISABLE_PIP_VERSION_CHECK=1 \
    GRIDPIN_HOME=/data

WORKDIR /src
COPY pyproject.toml README.md LICENSE ./
COPY src ./src
RUN pip install . \
    && useradd --create-home --uid 10001 gridpin \
    && mkdir -p /data \
    && chown gridpin /data

USER gridpin
WORKDIR /home/gridpin
ENTRYPOINT ["gridpin-mcp"]

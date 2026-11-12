# A DeGAUSS-compatible GridPin container: `docker run --rm -v $PWD:/tmp <image> my_address_file.csv [0.5|all]`.
# BASE is a GridPin country image (the engine in /usr/local/bin/gridpin and the sheet in /data/sheet.bin).
ARG BASE=ghcr.io/gridpin/gridpin:us
FROM ${BASE} AS gridpin
FROM python:3.12-slim
COPY --from=gridpin /usr/local/bin/gridpin /usr/local/bin/gridpin
COPY --from=gridpin /data/sheet.bin /data/sheet.bin
COPY --from=gridpin /usr/share/doc/gridpin /usr/share/doc/gridpin
COPY degauss.py /app/degauss.py
WORKDIR /tmp
ENTRYPOINT ["python3", "/app/degauss.py"]

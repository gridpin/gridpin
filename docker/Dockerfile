# GridPin offline geocoder in a container: `gridpin serve` on port 8080, one country sheet inside.
#
# The build context is prepared by the release workflow (or CI), not by this file: it holds the
# `gridpin` binary taken from the signed release archive and checked against SHA256SUMS, the
# archive's LICENSE and THIRDPARTY.yml (license texts of the compiled-in libraries),
# ATTRIBUTIONS.en.md, and for the `country` target the sheet checked against its release SHA-256.
# Nothing is downloaded while the image is built, and nothing leaves the container at run time.
#
#   docker run --rm -p 8080:8080 ghcr.io/gridpin/gridpin:us
#   curl "http://127.0.0.1:8080/search?q=1600+Pennsylvania+Ave+NW,+Washington,+DC&format=json"
#
# Targets: `engine` — no sheet, mount your own at /data/sheet.bin; `country` — the sheet inside.

# distroless cc (glibc, no shell, no package manager), non-root user 65532
FROM gcr.io/distroless/cc-debian13:nonroot@sha256:08476c244888bfb425bae1ae4dd7c17f093e972aab8426883d97f3c6f280f7c1 AS engine
ARG VERSION=dev
LABEL org.opencontainers.image.source="https://github.com/gridpin/gridpin" \
      org.opencontainers.image.documentation="https://gridpin.dev/docker-geocoding" \
      org.opencontainers.image.licenses="Apache-2.0" \
      org.opencontainers.image.version="${VERSION}" \
      org.opencontainers.image.title="GridPin" \
      org.opencontainers.image.description="Offline geocoder: gridpin serve, own API and Nominatim-compatible paths"
COPY --chmod=0555 gridpin /usr/local/bin/gridpin
COPY LICENSE THIRDPARTY.yml ATTRIBUTIONS.en.md /usr/share/doc/gridpin/
# Create /data (0755, root) before any COPY into it: `COPY --chmod=0444 … /data/sheet.bin` would
# otherwise create the directory with the file's mode, r--r--r--, and user 65532 could not enter it.
WORKDIR /data
USER 65532:65532
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=5s --start-period=120s --retries=3 \
  CMD ["/usr/local/bin/gridpin", "serve", "--probe", "http://127.0.0.1:8080/health"]
ENTRYPOINT ["/usr/local/bin/gridpin", "serve", "/data/sheet.bin", "--host", "0.0.0.0", "--port", "8080"]

FROM engine AS country
ARG SHEET
ARG COUNTRY
ARG SHEET_SHA256
ARG SHEET_RELEASE
LABEL io.gridpin.sheet.country="${COUNTRY}" \
      io.gridpin.sheet.sha256="${SHEET_SHA256}" \
      io.gridpin.sheet.release="${SHEET_RELEASE}" \
      org.opencontainers.image.description="Offline geocoder for one country (${COUNTRY}): gridpin serve, own API and Nominatim-compatible paths"
COPY --chmod=0444 ${SHEET} /data/sheet.bin

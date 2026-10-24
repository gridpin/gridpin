"""GridPin live demo for Streamlit Community Cloud: geocode an address or a point from a local country file.

The app downloads a country sheet once (checked against the signed GridPin release) and geocodes from it on the
server running the app; queries are not sent to any geocoding service. Run locally:

    pip install -r requirements.txt
    streamlit run streamlit_app.py
"""

from __future__ import annotations

from typing import Callable, Dict, List, Optional

from gridpin_mcp import sheets, tools

EXAMPLES: Dict[str, str] = {
    "us": "1600 Pennsylvania Avenue NW, Washington, DC 20500",
    "rs": "Knez Mihailova 5, Beograd",
    "nl": "Damrak 1, Amsterdam",
    "es": "Calle de Alcalá 50, Madrid",
    "de": "Römerberg 23, Frankfurt am Main",
    "fr": "8 Boulevard du Port, Amiens",
    "it": "Via del Corso 300, Roma",
    "br": "Avenida Paulista 1578, São Paulo",
}
# A point for the reverse tab: where each example address resolves.
POINTS: Dict[str, tuple] = {
    "us": (38.8977, -77.0365),
    "rs": (44.8155, 20.4594), "nl": (52.3771, 4.8980), "es": (40.4194, -3.6916), "de": (50.1107, 8.6819),
    "fr": (49.8974, 2.2901), "it": (41.9056, 12.4785), "br": (-23.5620, -46.6557),
}
COLUMNS = ["precision", "street", "housenumber", "commune", "postcode", "lat", "lon", "distance_m"]


def country_label(code: str) -> str:
    sheet = sheets.SHEETS[code]
    return f"{sheet.name} ({sheet.size / 1e6:.0f} MB)"


def new_session(open_sheet: Optional[Callable[[str], object]] = None) -> tools.Session:
    return tools.Session(open_sheet=open_sheet or sheets.download)


def rows(hits: List[dict]) -> List[dict]:
    """Engine hits -> flat table rows, rounded coordinates, missing fields as empty strings."""
    out = []
    for hit in hits:
        row = {name: hit.get(name, "") for name in COLUMNS}
        row["lat"], row["lon"] = round(hit["lat"], 6), round(hit["lon"], 6)
        if "distance_m" in hit:
            row["distance_m"] = round(hit["distance_m"], 1)
        out.append(row)
    return out


def geocode(session: tools.Session, query: str, country: str, k: int) -> List[dict]:
    return rows(session.geocode(query, country, k)["results"])


def reverse(session: tools.Session, lat: float, lon: float, country: str, k: int) -> List[dict]:
    return rows(session.reverse(lat, lon, country, k)["results"])


def show(st, table: List[dict], empty: str) -> None:
    if not table:
        st.info(empty)
        return
    st.dataframe(table, width="stretch", hide_index=True)
    st.map([{"lat": r["lat"], "lon": r["lon"]} for r in table], size=40)


def main() -> None:
    import streamlit as st

    st.set_page_config(page_title="GridPin — offline geocoding", page_icon="📍")
    session = st.cache_resource(new_session)()

    st.title("GridPin — offline geocoding")
    st.write(
        "One file per country: no API key, no rate limit, no geocoding service in the loop. In this demo the "
        "country file and your query sit on Streamlit's server; installed on your own machine "
        "(`pip install gridpin-mcp`), nothing leaves it. The first query for a country downloads its file once "
        "(32–677 MB)."
    )
    country = st.selectbox("Country", list(sheets.SHEETS), index=list(sheets.SHEETS).index("rs"),
                           format_func=country_label)
    k = st.slider("Candidates", 1, 10, 1)
    forward, backward = st.tabs(["Address → coordinates", "Coordinates → address"])
    with forward:
        query = st.text_input("Address", value=EXAMPLES[country], key=f"q-{country}")
        if query.strip():
            try:
                with st.spinner("Looking up (the first query for a country downloads its file)…"):
                    table = geocode(session, query, country, k)
                show(st, table, "No match in this country's file. GridPin returns nothing rather than a guess.")
            except tools.ToolError as exc:
                st.error(str(exc))
    with backward:
        lat = st.number_input("Latitude", value=POINTS[country][0], format="%.6f", key=f"lat-{country}")
        lon = st.number_input("Longitude", value=POINTS[country][1], format="%.6f", key=f"lon-{country}")
        try:
            with st.spinner("Looking up…"):
                table = reverse(session, lat, lon, country, k)
            show(st, table, "Nothing indexed near this point in the selected country.")
        except tools.ToolError as exc:
            st.error(str(exc))
    st.caption(
        "precision: house is a door-level match; street and city are approximate. Reverse results are "
        "approximate and carry distance_m. Install: pip install gridpin gridpin-mcp · "
        "[Docs](https://gridpin.dev/docs) · [Source](https://github.com/gridpin/gridpin) · "
        "Data: Overture Maps and national registries, "
        "[attributions](https://github.com/gridpin/gridpin/blob/main/ATTRIBUTIONS.md)."
    )
    st.caption(
        "If GridPin is useful to you, [a star on GitHub](https://github.com/gridpin/gridpin) "
        "helps other people find it."
    )


if __name__ == "__main__":
    main()

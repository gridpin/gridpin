"""A map of geocoded rows in a notebook, without a network: `gridpin.notebook.map(df)`.

The points are drawn as SVG in their own box, labelled, with a scale bar; there is no background map (no tiles, no
request), and the widget says so. `df` is a pandas or polars DataFrame with gp_lat / gp_lon (as `df.gridpin.geocode`
gives) or any sequence of dicts with lat and lon. A weak answer (gp_weak) is drawn like any other point: filter it out
first if the map should show only answers the engine stands behind.
"""

from __future__ import annotations

import html
import math

__all__ = ["map", "svg"]

W, H, PAD = 640, 420, 24


def _points(data):
    if hasattr(data, "to_dicts"):        # polars: to_dict() there gives columns and takes no "records"
        rows = data.to_dicts()
    elif hasattr(data, "to_dict"):       # pandas
        rows = data.to_dict("records")
    else:
        rows = list(data)
    out = []
    for r in rows:
        lat = r.get("gp_lat", r.get("lat"))
        lon = r.get("gp_lon", r.get("lon"))
        if lat is None or lon is None or lat != lat or lon != lon:
            continue
        label = r.get("gp_street") or r.get("street") or r.get("address") or ""
        out.append((float(lat), float(lon), str(label)))
    return out


def svg(data) -> str:
    pts = _points(data)
    if not pts:
        return f'<svg xmlns="http://www.w3.org/2000/svg" width="{W}" height="60"><text x="10" y="30">no point to draw</text></svg>'
    lats, lons = [p[0] for p in pts], [p[1] for p in pts]
    lat0 = sum(lats) / len(lats)
    k = math.cos(math.radians(lat0))
    x0, x1 = min(lons) * k, max(lons) * k
    y0, y1 = min(lats), max(lats)
    span = max(x1 - x0, y1 - y0, 1e-6)
    scale = min(W - 2 * PAD, H - 2 * PAD - 20) / span
    px = lambda lon: PAD + (lon * k - x0) * scale
    py = lambda lat: H - PAD - 20 - (lat - y0) * scale
    dots = "".join(f'<circle cx="{px(lo):.1f}" cy="{py(la):.1f}" r="4" fill="#0b63ce"><title>{html.escape(t)}</title></circle>'
                   for la, lo, t in pts)
    metres = span * 111_320
    bar = 10 ** math.floor(math.log10(max(metres / 4, 1)))
    bar_px = bar / 111_320 * scale
    return (f'<svg xmlns="http://www.w3.org/2000/svg" width="{W}" height="{H}" style="background:#fafafa;border:1px solid #ddd">'
            f'{dots}<line x1="{PAD}" y1="{H - 12}" x2="{PAD + bar_px:.1f}" y2="{H - 12}" stroke="#333" stroke-width="2"/>'
            f'<text x="{PAD + bar_px + 6:.1f}" y="{H - 8}" font-size="11">{bar:,.0f} m</text>'
            f'<text x="{W - 8}" y="{H - 8}" font-size="11" text-anchor="end">{len(pts)} points · no background map (offline)</text></svg>')


class _Map:
    def __init__(self, data):
        self.markup = svg(data)

    def _repr_html_(self):
        return self.markup


def map(data):  # noqa: A001 - the name users expect
    """A widget that shows itself in Jupyter (`_repr_html_`)."""
    return _Map(data)

"""Locator filter: type ``gp <address>`` in the search bar to jump to it."""

from qgis.core import (
    QgsCoordinateReferenceSystem,
    QgsCoordinateTransform,
    QgsGeometry,
    QgsLocatorFilter,
    QgsLocatorResult,
    QgsPointXY,
    QgsProject,
    QgsRectangle,
)

from . import engine, sheets

PER_SHEET = 3
# Half-size of the box shown around a result, in degrees, by precision.
SPAN = {"house": 0.001, "interp": 0.001, "near": 0.002, "street": 0.004, "postcode": 0.02}


def downloaded():
    return [s for s in sheets.SHEETS.values() if sheets.is_downloaded(s)]


def search(text, available=downloaded):
    """Results for every downloaded sheet: (label, description, hit). Pure, for tests."""
    found = []
    for sheet in available():
        for hit in engine.geocode(sheets.local_path(sheet), text, PER_SHEET):
            found.append((engine.address(hit), f"{sheet.name} · {hit['precision']}", hit))
    return found


class GridPinLocator(QgsLocatorFilter):
    def __init__(self, iface=None, available=downloaded):
        super().__init__()
        self.iface = iface
        self.available = available

    def clone(self):
        return GridPinLocator(self.iface, self.available)

    def name(self):
        return "gridpin"

    def displayName(self):
        return "GridPin address"

    def prefix(self):
        return "gp"

    def flags(self):
        return QgsLocatorFilter.FlagFast if hasattr(QgsLocatorFilter, "FlagFast") else QgsLocatorFilter.Flag.FlagFast

    def fetchResults(self, text, context, feedback):
        if len(text.strip()) < 3 or not engine.available():
            return
        for label, description, hit in search(text, self.available):
            if feedback.isCanceled():
                return
            result = QgsLocatorResult()
            result.filter = self
            result.displayString = label or description
            result.description = description
            result.userData = hit
            self.resultFetched.emit(result)

    def triggerResult(self, result):
        hit = result.userData
        if self.iface is None:
            return
        canvas = self.iface.mapCanvas()
        span = SPAN.get(hit["precision"], 0.05)
        box = QgsRectangle(hit["lon"] - span, hit["lat"] - span, hit["lon"] + span, hit["lat"] + span)
        transform = QgsCoordinateTransform(
            QgsCoordinateReferenceSystem("EPSG:4326"), canvas.mapSettings().destinationCrs(),
            QgsProject.instance())
        canvas.setExtent(transform.transformBoundingBox(box))
        canvas.flashGeometries([QgsGeometry.fromPointXY(
            transform.transform(QgsPointXY(hit["lon"], hit["lat"])))])
        canvas.refresh()

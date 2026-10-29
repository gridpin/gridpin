"""Processing provider: geocode an address field, download a country sheet."""

from qgis.core import (
    QgsCoordinateReferenceSystem,
    QgsFeature,
    QgsFeatureSink,
    QgsField,
    QgsFields,
    QgsFileDownloader,
    QgsGeometry,
    QgsPointXY,
    QgsProcessing,
    QgsProcessingAlgorithm,
    QgsProcessingException,
    QgsProcessingParameterEnum,
    QgsProcessingParameterFeatureSink,
    QgsProcessingParameterFeatureSource,
    QgsProcessingParameterField,
    QgsProcessingParameterFile,
    QgsProcessingProvider,
    QgsWkbTypes,
)
from qgis.PyQt.QtCore import QEventLoop, QMetaType, QUrl

from . import engine, sheets

COUNTRY_LABELS = [f"{s.name} ({s.code})" for s in sheets.SHEETS.values()]
RESULT_FIELDS = (
    ("gp_address", QMetaType.Type.QString),
    ("gp_precision", QMetaType.Type.QString),
    ("gp_confidence", QMetaType.Type.Double),
    ("gp_lat", QMetaType.Type.Double),
    ("gp_lon", QMetaType.Type.Double),
)


def _field(name, kind):
    try:
        return QgsField(name, kind)
    except TypeError:  # QGIS before 3.38 takes QVariant.Type
        from qgis.PyQt.QtCore import QVariant

        return QgsField(name, QVariant.Double if kind == QMetaType.Type.Double else QVariant.String)


# Only text fields are offered, so the first field of a table (often a numeric id) is never the default.
_STRING_FIELD = getattr(QgsProcessingParameterField, "String", None)
if _STRING_FIELD is None:  # QGIS 4: scoped enum
    from qgis.core import Qgis

    _STRING_FIELD = Qgis.ProcessingFieldParameterDataType.String


def _point_type():
    return QgsWkbTypes.Point if hasattr(QgsWkbTypes, "Point") else QgsWkbTypes.Type.Point


class GeocodeField(QgsProcessingAlgorithm):
    INPUT, FIELD, COUNTRY, SHEET, OUTPUT = "INPUT", "FIELD", "COUNTRY", "SHEET", "OUTPUT"

    def name(self):
        return "geocode"

    def displayName(self):
        return "Geocode address field"

    def shortHelpString(self):
        return (
            "Turns an address field into points, offline, with the GridPin sheet of one country. "
            "Rows without a match keep their attributes and get no geometry. "
            "Download the sheet first with 'Download country sheet'."
        )

    def createInstance(self):
        return GeocodeField()

    def initAlgorithm(self, config=None):
        self.addParameter(QgsProcessingParameterFeatureSource(
            self.INPUT, "Input layer or table", [QgsProcessing.TypeVector]))
        self.addParameter(QgsProcessingParameterField(
            self.FIELD, "Address field", parentLayerParameterName=self.INPUT, type=_STRING_FIELD))
        self.addParameter(QgsProcessingParameterEnum(
            self.COUNTRY, "Country", options=COUNTRY_LABELS, defaultValue=0))
        self.addParameter(QgsProcessingParameterFile(
            self.SHEET, "Sheet file (overrides country)", optional=True, fileFilter="Sheets (*.bin)"))
        self.addParameter(QgsProcessingParameterFeatureSink(
            self.OUTPUT, "Geocoded", QgsProcessing.TypeVectorPoint))

    def processAlgorithm(self, parameters, context, feedback):
        if not engine.available():
            raise QgsProcessingException(engine.MISSING)
        source = self.parameterAsSource(parameters, self.INPUT, context)
        field = self.parameterAsString(parameters, self.FIELD, context)
        path = self.parameterAsFile(parameters, self.SHEET, context)
        if not path:
            code = sheets.CODES[self.parameterAsEnum(parameters, self.COUNTRY, context)]
            try:
                path = str(sheets.sheet_path(code))
            except sheets.SheetError as e:
                raise QgsProcessingException(str(e))

        fields = QgsFields(source.fields())
        for name, kind in RESULT_FIELDS:
            fields.append(_field(name, kind))
        sink, dest = self.parameterAsSink(
            parameters, self.OUTPUT, context, fields, _point_type(),
            QgsCoordinateReferenceSystem("EPSG:4326"))
        index = source.fields().lookupField(field)
        total = source.featureCount() or 1
        found = 0
        for i, feature in enumerate(source.getFeatures()):
            if feedback.isCanceled():
                break
            value = feature.attributes()[index]
            text = "" if value is None else str(value).strip()
            hits = engine.geocode(path, text, 1) if text and text != "NULL" else []
            out = QgsFeature(fields)
            if hits:
                hit = hits[0]
                found += 1
                out.setGeometry(QgsGeometry.fromPointXY(QgsPointXY(hit["lon"], hit["lat"])))
                extra = [engine.address(hit), hit["precision"], hit["confidence"], hit["lat"], hit["lon"]]
            else:
                extra = [None] * len(RESULT_FIELDS)
            out.setAttributes(feature.attributes() + extra)
            sink.addFeature(out, QgsFeatureSink.FastInsert)
            feedback.setProgress(100 * (i + 1) / total)
        feedback.pushInfo(f"Geocoded {found} of {source.featureCount()} rows.")
        return {self.OUTPUT: dest, "FOUND": found}


def fetch(url, part, progress=None, feedback=None):
    """Download ``url`` into ``part`` through the QGIS network stack (proxy, SSL settings)."""
    loop = QEventLoop()
    errors = []
    downloader = QgsFileDownloader(QUrl(url), part, delayStart=True)
    downloader.downloadError.connect(lambda e: errors.extend(e))
    downloader.downloadExited.connect(loop.quit)
    if progress:
        downloader.downloadProgress.connect(lambda done, total: progress(done, total))
    if feedback:
        feedback.canceled.connect(downloader.cancelDownload)
    downloader.startDownload()
    loop.exec()
    if feedback and feedback.isCanceled():
        raise sheets.SheetError("download canceled")
    if errors:
        raise sheets.SheetError(f"{url}: " + "; ".join(errors))


class DownloadSheet(QgsProcessingAlgorithm):
    COUNTRY = "COUNTRY"

    def name(self):
        return "download_sheet"

    def displayName(self):
        return "Download country sheet"

    def shortHelpString(self):
        return (
            "Downloads one country's sheet from dl.gridpin.dev and checks its size and SHA-256 "
            "against the signed release. This is the only step that uses the network; "
            "geocoding itself runs offline. Sheets are stored in GRIDPIN_HOME or ~/.cache/gridpin."
        )

    def createInstance(self):
        return DownloadSheet()

    def initAlgorithm(self, config=None):
        self.addParameter(QgsProcessingParameterEnum(
            self.COUNTRY, "Country", options=COUNTRY_LABELS, defaultValue=0))

    def processAlgorithm(self, parameters, context, feedback):
        code = sheets.CODES[self.parameterAsEnum(parameters, self.COUNTRY, context)]
        sheet = sheets.SHEETS[code]
        feedback.pushInfo(f"{sheet.url} ({sheet.size / 1e6:.0f} MB)")

        def progress(done, total):
            feedback.setProgress(100 * done / (total if total > 0 else sheet.size))

        try:
            path = sheets.download(
                code, lambda url, part, p: fetch(url, part, p, feedback), progress)
        except sheets.SheetError as e:
            raise QgsProcessingException(str(e))
        return {"SHEET": str(path)}


class Provider(QgsProcessingProvider):
    def id(self):
        return "gridpin"

    def name(self):
        return "GridPin"

    def loadAlgorithms(self):
        self.addAlgorithm(GeocodeField())
        self.addAlgorithm(DownloadSheet())

"""Plugin entry: registers the locator filter and the Processing provider."""

from qgis.core import QgsApplication

from .locator import GridPinLocator
from .processing_gridpin import Provider


class GridPinPlugin:
    def __init__(self, iface):
        self.iface = iface
        self.locator = None
        self.provider = None

    def initProcessing(self):
        self.provider = Provider()
        QgsApplication.processingRegistry().addProvider(self.provider)

    def initGui(self):
        self.initProcessing()
        self.locator = GridPinLocator(self.iface)
        self.iface.registerLocatorFilter(self.locator)

    def unload(self):
        if self.locator:
            self.iface.deregisterLocatorFilter(self.locator)
            self.locator = None
        if self.provider:
            QgsApplication.processingRegistry().removeProvider(self.provider)
            self.provider = None

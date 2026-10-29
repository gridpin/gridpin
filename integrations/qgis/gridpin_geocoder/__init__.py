"""GridPin for QGIS: offline geocoding with one local file per country."""


def classFactory(iface):
    from .plugin import GridPinPlugin

    return GridPinPlugin(iface)

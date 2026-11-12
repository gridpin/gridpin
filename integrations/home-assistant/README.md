# GridPin reverse geocoding for Home Assistant

A sensor per device tracker whose state is its address, from your own `gridpin serve <sheet>` on the local network:
no cloud service sees where you are.

Install by copying: put the folder `custom_components/gridpin` of this directory into the `custom_components` folder
of your Home Assistant configuration (`config/custom_components/gridpin`) and restart Home Assistant. Then
Settings → Devices & services → Add integration → GridPin, and give the server address.

HACS cannot install it from the gridpin/gridpin repository: HACS takes an integration only from a repository whose
root holds `custom_components/` and `hacs.json`, and here the integration is one folder of a larger repository.

The state is written house number first in every country ("9 Zmaj-jovina, Stari Grad"), also where addresses put the
number after the street. Licence: Apache-2.0.

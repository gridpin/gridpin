# Partner geolocation with GridPin (Odoo)

Odoo's `base_geolocalize` asks a geocoding provider for the coordinates of a partner. This module adds GridPin:
a server on your own machine (`gridpin serve <sheet>`), so no address leaves your network and there is no key.

1. Run the server: `gridpin serve us.bin --port 8080`.
2. Put this folder in your addons path and install **Partner geolocation with GridPin**.
3. Settings → Integrations → Geolocation: choose **GridPin (your own server)**. The server address is the system
   parameter `base_geolocalize_gridpin.url` (default `http://127.0.0.1:8080`).
4. On a partner: **Compute based on address**.

The address line is written the way the engine finds it: street, then postcode and town; in the United States,
Canada and Australia "town, STATE postcode". The state or region is left out elsewhere: after the town it made the
answers worse (Serbia and Brazil, revision 0.9.0), so it is written only when the partner has no town.

The server has 30 seconds to answer (system parameter `base_geolocalize_gridpin.timeout`); the first query on a
large sheet, or a busy machine, can take longer, and the message then says so rather than "cannot reach".

A `weak` answer (the engine itself says check it) is not used: the partner keeps no coordinates rather than wrong
ones. Odoo 16 and 17 (the provider interface of `base_geolocalize` is the same).

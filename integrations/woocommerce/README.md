# GridPin Address for WooCommerce

A WordPress plugin: address suggestions at checkout and a check of the delivery address after each order, answered by
`gridpin serve` on the shop's own server. The plugin is in [`gridpin-address/`](gridpin-address/); its
[`readme.txt`](gridpin-address/readme.txt) is the user documentation (the format of the WordPress plugin directory).

Licence: GPL-2.0-or-later (the plugin only), as plugins of the WordPress directory require. GridPin itself stays
Apache-2.0; the plugin talks to it over HTTP.

Tests: `php gridpin-address/tests/test-client.php` (the client without WordPress: suggestions, fields of a chosen
address, the verdict and the note, a server that is down). The WordPress side was checked in a real WooCommerce 11.3
shop: block and classic checkout, three outcomes of the check, the rate limit.

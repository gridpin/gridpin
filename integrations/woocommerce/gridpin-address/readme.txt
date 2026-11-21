=== GridPin Address for WooCommerce ===
Contributors: gridpin
Tags: address autocomplete, address validation, checkout, woocommerce, geocoding
Requires at least: 6.5
Tested up to: 6.9
Requires PHP: 7.4
WC requires at least: 9.9
WC tested up to: 11.3
Stable tag: 0.1.0
License: GPLv2 or later
License URI: https://www.gnu.org/licenses/gpl-2.0.html

Address suggestions at checkout and a check of every delivery address, answered by GridPin on your own server.

== Description ==

**You need a server where you can run a program.** This plugin talks to `gridpin serve`, the GridPin geocoder
running on your own machine with a map file of one country. Shared hosting that only runs PHP is not enough.

What it does:

* **Suggestions at checkout.** It is a provider for WooCommerce's own address autocomplete (WooCommerce 9.9 and
  later), in the block checkout and the classic one. The customer types the street; a list of real addresses from the
  map file appears; choosing one fills street, town and postcode.
* **A CEP in the address field (Brazil).** When the server has the CEP layer (`gridpin serve br.bin
  --postcode-layer br_cep.tsv.gz`), typing an eight-digit CEP into the street field lists the streets of that code;
  choosing one fills street, town, state and CEP, and the customer adds the house number.
* **A check after every order.** The delivery address (shipping, or billing when there is no shipping address) is
  checked against the map file. The order gets a field `_gridpin_address_check` (`yes`, `maybe`, `no` or `unknown`
  with the reason), and when the address is not found or only partly, a private note for you: "Check it with the
  customer before shipping". The order is never blocked: a new house can be missing from the file.

What it does not do:

* No address leaves your server. The checkout script calls this shop's own REST routes; they ask `gridpin serve` at
  the address you set. No third-party service, no key, no per-request fee.
* Suggestions are offered only when the checkout country is the country of the map file. Other countries are typed by
  hand as before.
* When the server does not answer, checkout works as before, without suggestions, and orders are marked `unknown`.
* Fields the map file lacks stay empty for the customer to fill: some houses have no postcode in the file.

== Installation ==

1. Run GridPin on the server: `gridpin sheets fetch de` downloads the map file of Germany and checks it,
   `gridpin sheets where de` prints its path, and `gridpin serve <that path> --port 8080` answers on
   `http://127.0.0.1:8080`. See https://gridpin.dev/docs.
2. Install and activate this plugin.
3. WooCommerce → Settings → General → turn on "Enable address autocomplete".
4. WooCommerce → GridPin Address: check that the server answers and which country its file covers.

== Frequently Asked Questions ==

= Does the customer's address go to Google or another service? =

No. It goes from the browser to your shop, and from your shop to `gridpin serve` on the address you set.

= What happens to an order whose address is not found? =

Nothing automatic. The order is placed as usual and gets a private note asking you to check the address.

= How often can one visitor ask? =

120 calls a minute from one IP address to the suggestion routes; typing one address takes 20 to 30.

== Changelog ==

= 0.1.0 =
* First version: suggestions through WooCommerce's address autocomplete, the check after the order, the settings page.

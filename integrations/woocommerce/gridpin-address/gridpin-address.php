<?php
/**
 * Plugin Name:       GridPin Address for WooCommerce
 * Description:       Address suggestions at checkout and a check of every delivery address after the order, from GridPin running on your own server. No address leaves your server.
 * Version:           0.1.0
 * Requires at least: 6.5
 * Requires PHP:      7.4
 * Requires Plugins:  woocommerce
 * WC requires at least: 9.9
 * License:           GPL-2.0-or-later
 * License URI:       https://www.gnu.org/licenses/gpl-2.0.html
 * Text Domain:       gridpin-address
 *
 * @package GridPin_Address
 */

defined( 'ABSPATH' ) || exit;

define( 'GRIDPIN_ADDRESS_VERSION', '0.1.0' );
define( 'GRIDPIN_ADDRESS_FILE', __FILE__ );

require_once __DIR__ . '/includes/class-gridpin-client.php';
require_once __DIR__ . '/includes/class-gridpin-address.php';

add_action( 'plugins_loaded', array( 'GridPin_Address', 'boot' ), 20 );

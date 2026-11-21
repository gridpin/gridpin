<?php
/**
 * The provider WooCommerce lists under its address autocomplete.
 *
 * @package GridPin_Address
 */

defined( 'ABSPATH' ) || exit;

/** GridPin as a WooCommerce address provider. */
class GridPin_Address_Provider extends WC_Address_Provider {
	/** Id and name. */
	public function __construct() {
		$this->id            = 'gridpin';
		$this->name          = 'GridPin';
		$this->branding_html = '';
	}
}

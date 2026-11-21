<?php
/**
 * WordPress side: the address provider, the REST routes the checkout calls, the check after an order, the settings.
 *
 * @package GridPin_Address
 */

defined( 'ABSPATH' ) || exit;

/**
 * Wires GridPin_Client into WooCommerce. Nothing here blocks an order: a server that is down means no suggestions and
 * an "unknown" check, and the customer types the address by hand as before.
 */
class GridPin_Address {
	const OPTION_SERVER = 'gridpin_address_server';
	const OPTION_CHECK  = 'gridpin_address_check_orders';
	const META_VERDICT  = '_gridpin_address_check';
	const REST_NS       = 'gridpin/v1';
	/**
	 * Calls per visitor (IP) per minute to the suggest and details routes. The block checkout asks once per key from the
	 * third character: one typed address is 20-30 calls, so 120 is about four addresses a minute from one IP.
	 */
	const RATE_PER_MINUTE = 120;

	/** Hooks. */
	public static function boot() {
		if ( ! class_exists( 'WooCommerce' ) || ! class_exists( 'WC_Address_Provider' ) ) {
			return;
		}
		require_once __DIR__ . '/class-gridpin-address-provider.php';
		add_filter( 'woocommerce_address_providers', array( __CLASS__, 'add_provider' ) );
		add_action( 'rest_api_init', array( __CLASS__, 'routes' ) );
		add_action( 'wp_enqueue_scripts', array( __CLASS__, 'scripts' ), 20 );
		add_action( 'woocommerce_checkout_order_processed', array( __CLASS__, 'check_classic' ), 20, 3 );
		add_action( 'woocommerce_store_api_checkout_order_processed', array( __CLASS__, 'check_order' ), 20, 1 );
		add_action( 'admin_menu', array( __CLASS__, 'menu' ) );
		add_action( 'admin_init', array( __CLASS__, 'settings' ) );
	}

	/** The client against the configured server. */
	public static function client() {
		$server = get_option( self::OPTION_SERVER, 'http://127.0.0.1:8080' );
		return new GridPin_Client(
			$server,
			static function ( $url, $timeout ) {
				$r = wp_remote_get( $url, array( 'timeout' => $timeout, 'redirection' => 0 ) );
				if ( is_wp_error( $r ) ) {
					return null;
				}
				return array( 'status' => (int) wp_remote_retrieve_response_code( $r ), 'body' => (string) wp_remote_retrieve_body( $r ) );
			}
		);
	}

	/** The country of the server's sheet, upper case, '' when down; asked once in five minutes. */
	public static function sheet_country() {
		$cached = get_transient( 'gridpin_address_sheet' );
		if ( false !== $cached ) {
			return (string) $cached;
		}
		$health  = self::client()->health();
		$country = $health['ok'] ? strtoupper( $health['sheet'] ) : '';
		set_transient( 'gridpin_address_sheet', $country, $health['ok'] ? 5 * MINUTE_IN_SECONDS : MINUTE_IN_SECONDS );
		return $country;
	}

	/**
	 * @param array $providers Providers so far.
	 * @return array
	 */
	public static function add_provider( $providers ) {
		$providers[] = new GridPin_Address_Provider();
		return $providers;
	}

	/** The two routes the checkout script calls. */
	public static function routes() {
		$common = array(
			'methods'             => 'POST',
			'permission_callback' => array( __CLASS__, 'allowed' ),
		);
		register_rest_route(
			self::REST_NS,
			'/suggest',
			$common + array(
				'callback' => static function ( WP_REST_Request $r ) {
					return rest_ensure_response( self::client()->suggest( $r->get_param( 'q' ) ) );
				},
			)
		);
		register_rest_route(
			self::REST_NS,
			'/details',
			$common + array(
				'callback' => static function ( WP_REST_Request $r ) {
					$fields = self::client()->details( $r->get_param( 'id' ), self::sheet_country() );
					return null === $fields ? new WP_Error( 'gridpin_not_found', 'not found', array( 'status' => 404 ) ) : rest_ensure_response( $fields );
				},
			)
		);
	}

	/**
	 * A checkout page asked (its nonce), and not too often from one address.
	 *
	 * @param WP_REST_Request $r Request.
	 * @return bool|WP_Error
	 */
	public static function allowed( WP_REST_Request $r ) {
		// WordPress asks this twice for one request (the second time for the Allow header): count the request once.
		static $decided = null;
		if ( null !== $decided ) {
			return $decided;
		}
		$decided = self::decide( $r );
		return $decided;
	}

	/**
	 * @param WP_REST_Request $r Request.
	 * @return bool|WP_Error
	 */
	private static function decide( WP_REST_Request $r ) {
		if ( ! wp_verify_nonce( (string) $r->get_header( 'X-WP-Nonce' ), 'wp_rest' ) ) {
			return new WP_Error( 'gridpin_nonce', 'bad nonce', array( 'status' => 403 ) );
		}
		$ip  = isset( $_SERVER['REMOTE_ADDR'] ) ? sanitize_text_field( wp_unslash( $_SERVER['REMOTE_ADDR'] ) ) : '';
		$key = 'gridpin_rate_' . md5( $ip . '|' . gmdate( 'YmdHi' ) );
		$n   = (int) get_transient( $key );
		if ( $n >= self::RATE_PER_MINUTE ) {
			return new WP_Error( 'gridpin_rate', 'too many requests', array( 'status' => 429 ) );
		}
		set_transient( $key, $n + 1, 2 * MINUTE_IN_SECONDS );
		return true;
	}

	/** The provider script on checkout pages, when WooCommerce's address autocomplete is on. */
	public static function scripts() {
		if ( ! function_exists( 'is_checkout' ) || ! is_checkout() || ! wp_script_is( 'wc-address-autocomplete-common', 'registered' ) ) {
			return;
		}
		wp_enqueue_script(
			'gridpin-address',
			plugins_url( 'assets/gridpin-address.js', GRIDPIN_ADDRESS_FILE ),
			array_merge(
				array( 'wc-address-autocomplete-common' ),
				wp_script_is( 'wc-blocks-data-store', 'registered' ) ? array( 'wc-blocks-data-store' ) : array()
			),
			GRIDPIN_ADDRESS_VERSION,
			true
		);
		wp_localize_script(
			'gridpin-address',
			'gridpinAddress',
			array(
				'root'    => esc_url_raw( rest_url( self::REST_NS ) ),
				'nonce'   => wp_create_nonce( 'wp_rest' ),
				'country' => self::sheet_country(),
			)
		);
	}

	/**
	 * Classic checkout.
	 *
	 * @param int      $order_id Order id.
	 * @param array    $posted   Posted data.
	 * @param WC_Order $order    Order.
	 */
	public static function check_classic( $order_id, $posted, $order ) {
		self::check_order( $order );
	}

	/**
	 * The check after an order: a meta field and, when the address is not found or only partly, a note for the seller.
	 * Never blocks: the order is already placed.
	 *
	 * @param WC_Order $order Order.
	 */
	public static function check_order( $order ) {
		if ( ! $order instanceof WC_Order || 'yes' !== get_option( self::OPTION_CHECK, 'yes' ) ) {
			return;
		}
		$verdict = self::client()->verdict( self::delivery_line( $order ) );
		$order->update_meta_data( self::META_VERDICT, $verdict['exists'] . ':' . $verdict['reason'] );
		$note = GridPin_Client::note( $verdict );
		if ( null !== $note ) {
			$order->add_order_note( $note );
		}
		$order->save();
	}

	/**
	 * The address the parcel goes to, as one line: shipping when the order has it, billing otherwise.
	 *
	 * @param WC_Order $order Order.
	 * @return string
	 */
	public static function delivery_line( $order ) {
		$kind  = $order->has_shipping_address() ? 'shipping' : 'billing';
		$get   = static function ( $field ) use ( $order, $kind ) {
			$m = "get_{$kind}_{$field}";
			return trim( (string) $order->$m() );
		};
		$place = trim( $get( 'postcode' ) . ' ' . $get( 'city' ) );
		return implode( ', ', array_filter( array( $get( 'address_1' ), $place ) ) );
	}

	/** Settings page under WooCommerce. */
	public static function menu() {
		add_submenu_page( 'woocommerce', 'GridPin Address', 'GridPin Address', 'manage_woocommerce', 'gridpin-address', array( __CLASS__, 'page' ) );
	}

	/** Registered options. */
	public static function settings() {
		register_setting(
			'gridpin_address',
			self::OPTION_SERVER,
			array(
				'type'              => 'string',
				'sanitize_callback' => static function ( $v ) {
					delete_transient( 'gridpin_address_sheet' );
					return esc_url_raw( trim( (string) $v ), array( 'http', 'https' ) );
				},
				'default'           => 'http://127.0.0.1:8080',
			)
		);
		register_setting(
			'gridpin_address',
			self::OPTION_CHECK,
			array(
				'type'              => 'string',
				'sanitize_callback' => static function ( $v ) {
					return 'yes' === $v ? 'yes' : 'no';
				},
				'default'           => 'yes',
			)
		);
	}

	/** The settings page: server address, the check switch, the server's state. */
	public static function page() {
		$health = self::client()->health();
		$auto   = get_option( 'woocommerce_address_autocomplete_enabled', 'no' );
		?>
		<div class="wrap">
			<h1>GridPin Address</h1>
			<p>GridPin answers from a map file on your own server (<code>gridpin serve</code>). Addresses typed at checkout are sent only there.</p>
			<?php if ( $health['ok'] ) : ?>
				<div class="notice notice-success inline"><p>Server answers. Map file: <strong><?php echo esc_html( strtoupper( $health['sheet'] ) ); ?></strong>. Suggestions are offered when the checkout country is this one.</p></div>
			<?php else : ?>
				<div class="notice notice-warning inline"><p>The server does not answer. Checkout works as before, without suggestions; orders are not checked.</p></div>
			<?php endif; ?>
			<?php if ( 'yes' !== $auto ) : ?>
				<div class="notice notice-info inline"><p>Suggestions need WooCommerce → Settings → General → “Enable address autocomplete”.</p></div>
			<?php endif; ?>
			<form method="post" action="options.php">
				<?php settings_fields( 'gridpin_address' ); ?>
				<table class="form-table" role="presentation">
					<tr><th scope="row"><label for="gridpin_server">Server address</label></th>
						<td><input id="gridpin_server" name="<?php echo esc_attr( self::OPTION_SERVER ); ?>" type="url" class="regular-text" value="<?php echo esc_attr( get_option( self::OPTION_SERVER, 'http://127.0.0.1:8080' ) ); ?>">
						<p class="description">Where <code>gridpin serve</code> listens, as seen from this server.</p></td></tr>
					<tr><th scope="row">Check orders</th>
						<td><label><input name="<?php echo esc_attr( self::OPTION_CHECK ); ?>" type="checkbox" value="yes" <?php checked( 'yes', get_option( self::OPTION_CHECK, 'yes' ) ); ?>> After each order, check the delivery address and leave a note when it is not found</label></td></tr>
				</table>
				<?php submit_button(); ?>
			</form>
		</div>
		<?php
	}
}

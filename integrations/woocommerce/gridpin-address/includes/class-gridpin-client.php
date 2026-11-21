<?php
/**
 * The talk with the local GridPin server: suggestions, the fields of a chosen address, the check after an order.
 * Pure PHP apart from the HTTP call, which is injected, so the logic runs in tests without WordPress.
 *
 * @package GridPin_Address
 */

defined( 'ABSPATH' ) || defined( 'GRIDPIN_TESTS' ) || exit;

/**
 * Client of `gridpin serve` on this machine. Every failure is an empty answer: the checkout never waits on it.
 */
class GridPin_Client {
	/** Longest text sent to the server. */
	const MAX_CHARS = 200;

	/** @var string Server address, e.g. http://127.0.0.1:8080 */
	private $server;

	/** @var callable (string $url, int $timeout) => array{status:int, body:string}|null */
	private $get;

	/**
	 * @param string   $server Server address.
	 * @param callable $get    HTTP GET; null on a network error.
	 */
	public function __construct( $server, $get ) {
		$this->server = rtrim( (string) $server, '/' );
		$this->get    = $get;
	}

	/**
	 * Clean a text a visitor typed: trimmed, control characters out, at most MAX_CHARS characters.
	 *
	 * @param mixed $text Raw input.
	 * @return string
	 */
	public static function clean( $text ) {
		$text = is_string( $text ) ? $text : '';
		$text = preg_replace( '/[\x00-\x1F\x7F]+/u', ' ', $text );
		$text = trim( (string) $text );
		return function_exists( 'mb_substr' ) ? mb_substr( $text, 0, self::MAX_CHARS ) : substr( $text, 0, self::MAX_CHARS );
	}

	/**
	 * JSON of one GET to the server, or null.
	 *
	 * @param string $path  Path and query.
	 * @param int    $timeout Seconds.
	 * @return array|null
	 */
	private function json( $path, $timeout = 3 ) {
		$response = call_user_func( $this->get, $this->server . $path, $timeout );
		if ( ! is_array( $response ) || 200 !== (int) $response['status'] ) {
			return null;
		}
		$data = json_decode( (string) $response['body'], true );
		return is_array( $data ) ? $data : null;
	}

	/** Prefix of the id of a suggestion read from the server's postcode layer: "cep|<code>|<street>". */
	const CODE_ID = 'cep|';

	/**
	 * Is the typed text a whole postcode of eight digits (Brazil's CEP, with or without the dash): the digits, or null.
	 *
	 * @param string $text Typed text.
	 * @return string|null
	 */
	public static function code_of( $text ) {
		$text = trim( (string) $text );
		return preg_match( '/^(\d{5})-?(\d{3})$/', $text, $m ) ? $m[1] . $m[2] : null;
	}

	/**
	 * Suggestions for the typed text, in the shape WooCommerce's address autocomplete wants: [{id, label}].
	 * The id is the label itself: choosing it asks for its fields. A text that is a whole CEP asks the server's
	 * postcode layer (`gridpin serve --postcode-layer`) for the streets of that code instead.
	 *
	 * @param string $query Typed text.
	 * @return array
	 */
	public function suggest( $query ) {
		$query = self::clean( $query );
		if ( strlen( $query ) < 3 ) {
			return array();
		}
		$code = self::code_of( $query );
		if ( null !== $code ) {
			return $this->code_suggestions( $code );
		}
		$data = $this->json( '/v1/suggest?' . http_build_query( array( 'q' => $query, 'k' => 8 ) ) );
		$out  = array();
		foreach ( (array) ( $data['suggestions'] ?? array() ) as $s ) {
			if ( isset( $s['label'] ) && '' !== $s['label'] ) {
				$out[] = array( 'id' => (string) $s['label'], 'label' => (string) $s['label'] );
			}
		}
		return $out;
	}

	/**
	 * The streets of a CEP from the server's postcode layer, as suggestions; empty when the server has no layer.
	 *
	 * @param string $code Eight digits.
	 * @return array
	 */
	private function code_suggestions( $code ) {
		$out = array();
		foreach ( $this->code_hits( $code ) as $h ) {
			$street = (string) ( $h['street'] ?? '' );
			$place  = trim( ( $h['commune'] ?? '' ) . ' - ' . ( $h['region'] ?? '' ), ' -' );
			$out[]  = array(
				'id'    => self::CODE_ID . $code . '|' . $street,
				'label' => implode( ', ', array_filter( array( $street, $place, (string) ( $h['postcode'] ?? '' ) ) ) ),
			);
		}
		return $out;
	}

	/**
	 * The answers of the postcode layer for a code (flag postcode_layer), at most five.
	 *
	 * @param string $code Eight digits.
	 * @return array
	 */
	private function code_hits( $code ) {
		$data = $this->json( '/v1/geocode?' . http_build_query( array( 'q' => $code, 'k' => 5 ) ) );
		$hits = array();
		foreach ( (array) ( $data['results'] ?? array() ) as $h ) {
			if ( is_array( $h ) && in_array( 'postcode_layer', (array) ( $h['flags'] ?? array() ), true ) ) {
				$hits[] = $h;
			}
		}
		return $hits;
	}

	/**
	 * The fields of a chosen address, as WooCommerce fills them, or null. The first part of the label is the street
	 * and number in the order of the country ("Albrechtstraße 5", "1600 Pennsylvania Avenue"); the postcode comes from
	 * the server's answer for the label.
	 *
	 * @param string $label   The chosen suggestion.
	 * @param string $country Two-letter country of the form.
	 * @return array|null
	 */
	public function details( $label, $country ) {
		$label = self::clean( $label );
		if ( '' === $label ) {
			return null;
		}
		if ( 0 === strpos( $label, self::CODE_ID ) ) {
			return $this->code_details( $label, $country );
		}
		$data = $this->json( '/v1/geocode?' . http_build_query( array( 'q' => $label, 'k' => 1 ) ) );
		$hit  = $data['results'][0] ?? null;
		if ( ! is_array( $hit ) ) {
			return null;
		}
		$parts = explode( ', ', $label );
		// An official commune name carries its kind after a comma ("Bernburg (Saale), Stadt"); the town field takes the
		// name people write, and a comma in it would split the line the check after the order reads.
		$town = explode( ',', (string) ( $hit['commune'] ?? '' ) )[0];
		return array(
			'address_1' => $parts[0],
			'address_2' => '',
			'city'      => trim( $town ),
			'state'     => '',
			'postcode'  => (string) ( $hit['postcode'] ?? '' ),
			'country'   => strtoupper( (string) $country ),
		);
	}

	/**
	 * The fields of a street chosen from a CEP: street (the customer adds the number), town, state and the code.
	 *
	 * @param string $id      "cep|<code>|<street>".
	 * @param string $country Two-letter country of the form.
	 * @return array|null
	 */
	private function code_details( $id, $country ) {
		$parts = explode( '|', $id, 3 );
		if ( 3 !== count( $parts ) || null === self::code_of( $parts[1] ) ) {
			return null;
		}
		foreach ( $this->code_hits( $parts[1] ) as $h ) {
			if ( (string) ( $h['street'] ?? '' ) === $parts[2] ) {
				return array(
					'address_1' => $parts[2],
					'address_2' => '',
					'city'      => (string) ( $h['commune'] ?? '' ),
					'state'     => (string) ( $h['region'] ?? '' ),
					'postcode'  => (string) ( $h['postcode'] ?? '' ),
					'country'   => strtoupper( (string) $country ),
				);
			}
		}
		return null;
	}

	/**
	 * The verdict on an address after an order: ['exists' => yes|maybe|no|unknown, 'reason' => ...]. Unknown when the
	 * server cannot be asked: the order goes through either way.
	 *
	 * @param string $line One line: street and number, postcode, town.
	 * @return array
	 */
	public function verdict( $line ) {
		$line = self::clean( $line );
		if ( '' === $line ) {
			return array( 'exists' => 'unknown', 'reason' => 'no_address' );
		}
		$data = $this->json( '/v1/exists?' . http_build_query( array( 'q' => $line ) ), 5 );
		if ( ! is_array( $data ) || ! isset( $data['exists'] ) ) {
			return array( 'exists' => 'unknown', 'reason' => 'server_unavailable' );
		}
		return array( 'exists' => (string) $data['exists'], 'reason' => (string) ( $data['reason'] ?? '' ) );
	}

	/**
	 * The note for the seller, or null when the address needs nothing. It asks to check, it does not call the
	 * address wrong: a sheet can lack a new house.
	 *
	 * @param array $verdict From verdict().
	 * @return string|null
	 */
	public static function note( $verdict ) {
		switch ( $verdict['exists'] ?? 'unknown' ) {
			case 'no':
				return 'GridPin: this delivery address was not found on the map file (' . $verdict['reason'] . '). Check it with the customer before shipping.';
			case 'maybe':
				return 'GridPin: this delivery address is only partly confirmed (' . $verdict['reason'] . '). Worth a check before shipping.';
			default:
				return null;
		}
	}

	/**
	 * Is the server up, and with which sheet: ['ok' => bool, 'sheet' => string].
	 *
	 * @return array
	 */
	public function health() {
		$data = $this->json( '/health', 2 );
		if ( ! is_array( $data ) ) {
			return array( 'ok' => false, 'sheet' => '' );
		}
		$sheet = $data['sheet']['country'] ?? '';
		return array( 'ok' => true, 'sheet' => (string) $sheet );
	}
}

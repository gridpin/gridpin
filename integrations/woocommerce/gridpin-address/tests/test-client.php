<?php
/**
 * Tests of GridPin_Client without WordPress: `php tests/test-client.php`. Exit code 0 when all pass.
 *
 * @package GridPin_Address
 */

define( 'GRIDPIN_TESTS', true );
require __DIR__ . '/../includes/class-gridpin-client.php';

$failed = 0;
$passed = 0;
function check( $name, $ok ) {
	global $failed, $passed;
	if ( $ok ) {
		$passed++;
	} else {
		$failed++;
		fwrite( STDERR, "FAIL: $name\n" );
	}
}

/** A fake server: path prefix => [status, body]; records the URLs asked. */
function fake( array $routes, array &$asked ) {
	return static function ( $url, $timeout ) use ( $routes, &$asked ) {
		$asked[] = $url;
		$path    = substr( $url, strlen( 'http://gp' ) );
		foreach ( $routes as $prefix => $answer ) {
			if ( 0 === strpos( $path, $prefix ) ) {
				return null === $answer ? null : array( 'status' => $answer[0], 'body' => $answer[1] );
			}
		}
		return array( 'status' => 404, 'body' => '' );
	};
}

// clean: control characters out, length capped.
check( 'clean strips control characters', 'a b' === GridPin_Client::clean( "a\x00\nb" ) );
check( 'clean caps the length', GridPin_Client::MAX_CHARS === mb_strlen( GridPin_Client::clean( str_repeat( 'ä', 500 ) ) ) );
check( 'clean of a non-string is empty', '' === GridPin_Client::clean( array( 'x' ) ) );

// suggest: shape, the id is the label, short input asks nothing.
$asked = array();
$c     = new GridPin_Client( 'http://gp/', fake( array( '/v1/suggest' => array( 200, '{"suggestions":[{"label":"Albrechtstraße 5, Berlin","precision":"house"},{"label":""}]}' ) ), $asked ) );
$s     = $c->suggest( 'Albrechtstr 5' );
check( 'suggest returns id and label', array( array( 'id' => 'Albrechtstraße 5, Berlin', 'label' => 'Albrechtstraße 5, Berlin' ) ) === $s );
check( 'suggest asks k=8 with the query', 1 === count( $asked ) && false !== strpos( $asked[0], 'q=Albrechtstr+5' ) && 1 === preg_match( '/[?&]k=8(&|$)/', $asked[0] ) );
$asked = array();
check( 'suggest under 3 characters asks nothing', array() === $c->suggest( 'Al' ) && array() === $asked );

// suggest: server down or garbage → empty, never an error.
$asked = array();
$down  = new GridPin_Client( 'http://gp', fake( array( '/' => null ), $asked ) );
check( 'suggest with the server down is empty', array() === $down->suggest( 'Albrechtstraße' ) );
$bad = new GridPin_Client( 'http://gp', fake( array( '/' => array( 200, 'not json' ) ), $asked ) );
check( 'suggest with garbage is empty', array() === $bad->suggest( 'Albrechtstraße' ) );
$err = new GridPin_Client( 'http://gp', fake( array( '/' => array( 500, '{"suggestions":[{"label":"x"}]}' ) ), $asked ) );
check( 'suggest with an error status is empty', array() === $err->suggest( 'Albrechtstraße' ) );

// details: street and number from the label, postcode and town from the answer.
$asked = array();
$c     = new GridPin_Client( 'http://gp', fake( array( '/v1/geocode' => array( 200, '{"results":[{"commune":"Berlin","postcode":"10117","lat":52.5,"lon":13.3}]}' ) ), $asked ) );
$d     = $c->details( 'Albrechtstraße 5, Berlin', 'de' );
check(
	'details fill the WooCommerce fields',
	array(
		'address_1' => 'Albrechtstraße 5',
		'address_2' => '',
		'city'      => 'Berlin',
		'state'     => '',
		'postcode'  => '10117',
		'country'   => 'DE',
	) === $d
);
check( 'details ask one answer', 1 === preg_match( '/[?&]k=1(&|$)/', $asked[0] ) );
$c = new GridPin_Client( 'http://gp', fake( array( '/v1/geocode' => array( 200, '{"results":[]}' ) ), $asked ) );
check( 'details without an answer are null', null === $c->details( 'Nowhere 1', 'de' ) );
check( 'details of an empty label are null', null === $c->details( '  ', 'de' ) );

$c = new GridPin_Client( 'http://gp', fake( array( '/v1/geocode' => array( 200, '{"results":[{"commune":"Bernburg (Saale), Stadt","postcode":""}]}' ) ), $asked ) );
$d = $c->details( 'Albrechtstraße 5, Bernburg (Saale), Stadt', 'de' );
check( 'details give the town without the kind after the comma', 'Bernburg (Saale)' === $d['city'] );
check( 'details leave a missing postcode empty', '' === $d['postcode'] );

// a CEP typed into the address field: the streets of the code from the server's postcode layer.
$layer = '{"results":[{"street":"Avenida Paulista","commune":"São Paulo","region":"SP","postcode":"01310-100","flags":["postcode_layer"]},'
	. '{"street":"Rua Stray","commune":"São Paulo","region":"SP","postcode":"01310-100","flags":["postcode_layer"]}]}';
$asked = array();
$c     = new GridPin_Client( 'http://gp', fake( array( '/v1/geocode' => array( 200, $layer ) ), $asked ) );
check( 'code_of reads both forms', '01310100' === GridPin_Client::code_of( '01310-100' ) && '01310100' === GridPin_Client::code_of( '01310100' ) );
check( 'code_of refuses an address', null === GridPin_Client::code_of( 'Rua 01310-100' ) && null === GridPin_Client::code_of( '0131010' ) );
$s = $c->suggest( '01310-100' );
check( 'a CEP suggests the streets of the code', 2 === count( $s ) && 'Avenida Paulista, São Paulo - SP, 01310-100' === $s[0]['label'] );
check( 'a CEP asks the server by its digits', 1 === preg_match( '/[?&]q=01310100(&|$)/', end( $asked ) ) && 1 === preg_match( '/[?&]k=5(&|$)/', end( $asked ) ) );
$d = $c->details( $s[1]['id'], 'br' );
check(
	'a chosen street of a CEP fills street, town, state and code',
	array(
		'address_1' => 'Rua Stray',
		'address_2' => '',
		'city'      => 'São Paulo',
		'state'     => 'SP',
		'postcode'  => '01310-100',
		'country'   => 'BR',
	) === $d
);
check( 'a forged code id is refused', null === $c->details( 'cep|0131|Avenida Paulista', 'br' ) && null === $c->details( 'cep|01310100|Nowhere', 'br' ) );
$sheet = new GridPin_Client( 'http://gp', fake( array( '/v1/geocode' => array( 200, '{"results":[{"street":"Rua 1310","commune":"X","postcode":"01310","flags":["street_exact"]}]}' ) ), $asked ) );
check( 'without a layer a CEP suggests nothing', array() === $sheet->suggest( '01310-100' ) );

// verdict and note.
$c = new GridPin_Client( 'http://gp', fake( array( '/v1/exists' => array( 200, '{"exists":"no","reason":"number_not_on_street"}' ) ), $asked ) );
$v = $c->verdict( 'Albrechtstraße 999, 10117 Berlin' );
check( 'verdict reads exists and reason', array( 'exists' => 'no', 'reason' => 'number_not_on_street' ) === $v );
check( 'note for no names the reason', false !== strpos( (string) GridPin_Client::note( $v ), 'number_not_on_street' ) );
check( 'note for maybe exists', null !== GridPin_Client::note( array( 'exists' => 'maybe', 'reason' => 'street_corrected' ) ) );
check( 'no note for yes', null === GridPin_Client::note( array( 'exists' => 'yes', 'reason' => 'house_found' ) ) );
check( 'no note for unknown', null === GridPin_Client::note( array( 'exists' => 'unknown', 'reason' => 'server_unavailable' ) ) );
check( 'verdict with the server down is unknown', array( 'exists' => 'unknown', 'reason' => 'server_unavailable' ) === $down->verdict( 'Albrechtstraße 5, Berlin' ) );
check( 'verdict of an empty line is unknown', 'unknown' === $down->verdict( '' )['exists'] );

// health.
$c = new GridPin_Client( 'http://gp', fake( array( '/health' => array( 200, '{"status":"ok","sheet":{"country":"de"}}' ) ), $asked ) );
check( 'health reads the sheet country', array( 'ok' => true, 'sheet' => 'de' ) === $c->health() );
check( 'health of a down server', array( 'ok' => false, 'sheet' => '' ) === $down->health() );

echo "$passed passed, $failed failed\n";
exit( $failed ? 1 : 0 );

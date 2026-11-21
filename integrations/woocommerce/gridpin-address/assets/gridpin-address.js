/* GridPin as a provider of WooCommerce's address autocomplete (classic and block checkout). The calls go to this
 * shop's own REST routes, which ask gridpin serve on the shop's server; nothing goes to a third party. */
( function () {
	var cfg = window.gridpinAddress || {};
	var api = window.wc && window.wc.addressAutocomplete;
	if ( ! api || typeof api.registerAddressAutocompleteProvider !== 'function' ) {
		return;
	}
	var latest = null;
	function post( path, body ) {
		return fetch( cfg.root + path, {
			method: 'POST',
			credentials: 'same-origin',
			headers: { 'Content-Type': 'application/json', 'X-WP-Nonce': cfg.nonce },
			body: JSON.stringify( body ),
		} ).then( function ( r ) {
			return r.ok ? r.json() : null;
		} );
	}
	api.registerAddressAutocompleteProvider( {
		id: 'gridpin',
		canSearch: function ( country ) {
			return !! cfg.country && String( country || '' ).toUpperCase() === cfg.country;
		},
		search: function ( query ) {
			// The classic checkout shows whichever answer comes last; an answer to older typing that arrives after a
			// newer one would hide the newer list. So every call answers with the list of the latest call.
			var mine = post( '/suggest', { q: query } )
				.then( function ( list ) {
					return Array.isArray( list ) ? list : [];
				} )
				.catch( function () {
					return [];
				} );
			latest = mine;
			return mine.then( function ( list ) {
				return latest === mine ? list : latest;
			} );
		},
		select: function ( id ) {
			return post( '/details', { id: id } ).catch( function () {
				return null;
			} );
		},
	} );
} )();

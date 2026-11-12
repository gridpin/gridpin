-- An example model: customers with their point and how far the answer reaches. Each address is geocoded once
-- (`as materialized`: an inlined CTE would call the geocoder again for every field read from it); the fields are read
-- from that answer. Rows with weak = 'true' are answers the engine says need checking.
with geocoded as materialized (
  select id, address, {{ gridpin.gridpin_geocode('address') }} as answer
  from {{ ref('customers') }}
)
select
  id,
  address,
  cast({{ gridpin.gridpin_field('answer', 'lat') }} as double precision) as lat,
  cast({{ gridpin.gridpin_field('answer', 'lon') }} as double precision) as lon,
  {{ gridpin.gridpin_field('answer', 'match_level') }} as match_level,
  {{ gridpin.gridpin_field('answer', 'weak') }} = 'true' as weak
from geocoded

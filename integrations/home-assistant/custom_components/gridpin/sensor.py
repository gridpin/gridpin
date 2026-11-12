"""One sensor per device tracker: its address, updated when its coordinates change (no cloud, the local server)."""
from homeassistant.components.sensor import SensorEntity
from homeassistant.helpers.aiohttp_client import async_get_clientsession
from homeassistant.helpers.event import async_track_state_change_event

from .format import reverse_url, state_of


async def async_setup_entry(hass, entry, async_add_entities):
    trackers = [s.entity_id for s in hass.states.async_all("device_tracker")]
    async_add_entities([GridPinAddress(hass, entry.data["server"], t) for t in trackers])


class GridPinAddress(SensorEntity):
    _attr_should_poll = False

    def __init__(self, hass, server, tracker):
        self.hass, self._server, self._tracker = hass, server, tracker
        self._attr_name = f"GridPin {tracker.split('.', 1)[1]}"
        self._attr_unique_id = f"gridpin_{tracker}"

    async def async_added_to_hass(self):
        self.async_on_remove(async_track_state_change_event(self.hass, [self._tracker], self._changed))
        await self._update()

    async def _changed(self, _event):
        await self._update()

    async def _update(self):
        st = self.hass.states.get(self._tracker)
        lat, lon = (st.attributes.get("latitude"), st.attributes.get("longitude")) if st else (None, None)
        if lat is None or lon is None:
            return
        async with async_get_clientsession(self.hass).get(reverse_url(self._server, lat, lon)) as r:
            self._attr_native_value, self._attr_extra_state_attributes = state_of(await r.json())
        self.async_write_ha_state()

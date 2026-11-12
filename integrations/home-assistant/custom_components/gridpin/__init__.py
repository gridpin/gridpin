"""GridPin reverse geocoding for Home Assistant: the address of each device tracker, from your local `gridpin serve`."""
DOMAIN = "gridpin"
PLATFORMS = ["sensor"]


async def async_setup_entry(hass, entry):
    await hass.config_entries.async_forward_entry_setups(entry, PLATFORMS)
    return True


async def async_unload_entry(hass, entry):
    return await hass.config_entries.async_unload_platforms(entry, PLATFORMS)

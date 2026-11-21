# GridPin on the internet, behind one key

Your own geocoding server at `https://your-domain/v1/geocode`, on a machine you rent: the server and the
addresses it sees are yours, nothing passes through GridPin.

```sh
./setup.sh us geo.example.com     # country of the sheet, a domain that points at this machine
```

`setup.sh` makes a random key (43 characters, in `gridpin.key`, readable by you only), checks that the machine
has the memory the sheet wants (`sheet-memory.txt`), writes `.env` and starts two containers:

- `gridpin` — the country image, started with `--require-key`: without the key file it does not start;
- `caddy` — gets the certificate for your domain from Let's Encrypt and passes requests on. It keeps no access
  log and refuses bodies over 1 MB. Only it listens outside (80 and 443).

Ask it with the key in a header; send addresses in a `POST` body, so they stay out of every log on the way:

```sh
curl -H "Authorization: Bearer $(cat gridpin.key)" -H 'Content-Type: application/json' \
  -d '{"queries": ["1600 Pennsylvania Ave NW, Washington, DC"]}' https://geo.example.com/v1/geocode
```

`/health` answers without the key (for your host's checks). The key is one for the whole server and counts
nothing: several keys, daily limits and a log of requests are `gridpin-pro serve` (https://gridpin.dev/pro).

The key needs the images of the GridPin release that brings it (`--require-key`); an older image stops at start.

## On a host, with a button

The public repository carries two templates (sources in `hosts/`):

- **Render** — `render.yaml` at the root: Render makes the key itself; the Serbia sheet on the free plan.
  [Deploy to Render](https://render.com/deploy?repo=https://github.com/gridpin/gridpin). The key is in the service's
  Environment tab.
- **DigitalOcean App Platform** — `.do/deploy.template.yaml`: paste a key of your own (`openssl rand -base64 32`)
  when the form asks; without one the server does not start.

Another country: fork the repository, change the image tag and the plan (`sheet-memory.txt` says how much memory
each sheet wants), and use the button with your fork.

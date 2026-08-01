# Deployment — `hcr.pmine.org` and `hcrapi.pmine.org`

Public, open to anyone, as an academy experiment. That decision shapes everything
below: there is no sign-in, and the guards that matter are the ones that hold
when strangers can reach the API.

## 0. What "no sign-in" means, precisely

`hcr-server` takes the caller's identity from the `X-HCR-Player` header and does
not check it. The header exists because the architecture reserves a place for an
authenticating proxy ([`01-CONTRACT.md`](01-CONTRACT.md) §9); on this deployment
nothing fills that place. So:

- anyone can submit as any player id, into any round whose code they have;
- a leaderboard says what *some browser claimed*, not who did the work;
- room codes are the only gate on a round — six unambiguous base32 characters,
  ~10⁹, unguessable in practice but not a permission.

For a public "have a go" experiment that is a reasonable trade, and it is the one
being made deliberately. The UI says so: the menu carries a notice, and the lobby
lists "Names are not verified" alongside the rules. Do not later present a
standing from this deployment as a graded result.

What is **not** left to trust:

| | |
| --- | --- |
| Scores | Server-side replay of the submitted IR. A client cannot report its own score. |
| Deadlines | Server receive time. A tampered client clock buys nothing. |
| Item references | HMAC-signed with `HCR_SIGNING_KEY`. Forging one requires the key. |
| Replay cost | `ReplayPool` bounds concurrency and returns 429 when saturated. |
| Memory | Sessions, rounds and the idempotency store are all bounded and swept. |

## 1. Frontend → Cloudflare Pages

Pages is the right fit: `vite build` emits `dist/` and nothing runs server-side.
No SSR, no API routes, and screens are React state rather than URLs, so every
visit lands on `/`.

| Pages setting | Value |
| --- | --- |
| Build command | `npm run build` |
| Build output directory | `dist` |
| Root directory | `HCR_Simulator_Frontend` |
| Node version | from `.nvmrc` (22) |

Environment variable, on **production and preview both**:

```
VITE_HCR_API_BASE_URL = https://hcrapi.pmine.org
```

> Vite inlines `import.meta.env.*` at **build** time. Changing it means a rebuild.
> A preview built without it silently falls back to offline practice mode
> (`resolveServices.ts`) — which looks like a working app, so it is the confusing
> failure rather than the loud one.

`public/_headers` ships a CSP whose `connect-src` allows `'self'` and
`https://hcrapi.pmine.org` only. If the API hostname changes, that file changes
with it or the browser blocks every request.

Add `hcr.pmine.org` as a custom domain; Cloudflare issues the certificate.

**Favicon:** `index.html` references `/favicon.png`. Save the pmine mark to
`HCR_Simulator_Frontend/public/favicon.png` — `public/` is copied to `dist/`
verbatim.

## 2. Backend → the VPS

Not Cloudflare: `hcr-server` is a native binary (multi-threaded tokio, the
`arona` CAT engine, hotaru's own TCP listener). Workers is a V8/WASM isolate with
no threads and no sockets, and `ReplayPool` — the thing that stops a pathological
program becoming a denial of service — has no equivalent there.

It is a modest process. Replay is CPU-bound and sized to the core count;
everything else is a few maps. Two shared cores and 512 MB is ample.

### Build

```sh
cargo build --release -p hcr_service --features hotaru --bin hcr-server
sudo install -m0755 target/release/hcr-server /usr/local/bin/
```

### Install

```sh
sudo useradd --system --no-create-home --shell /usr/sbin/nologin hcr
sudo install -d -o hcr -g hcr /var/lib/hcr

sudo install -m0644 deploy/hcr-server.service   /etc/systemd/system/
sudo install -m0644 deploy/hcr-usage.logrotate  /etc/logrotate.d/hcr-usage

sudo cp deploy/hcr-server.env.example /etc/hcr-server.env
sudo chown root:hcr /etc/hcr-server.env && sudo chmod 0640 /etc/hcr-server.env
sudo sed -i "s|replace-me-openssl-rand-hex-32|$(openssl rand -hex 32)|" /etc/hcr-server.env

sudo systemctl daemon-reload
sudo systemctl enable --now hcr-server
```

Then put a reverse proxy in front — see §2.1 (nginx/aaPanel) or §2.2 (Caddy).

### 2.1 Reverse proxy — nginx via aaPanel

`deploy/nginx-hcrapi.conf` is the site body. Create the site in aaPanel, let the
panel issue and renew the certificate, then replace the generated `location /`
with that file's contents.

Two pieces live in the `http {}` context instead (App Store → Nginx → Settings →
Configuration), because nginx will not accept them anywhere else:

```nginx
limit_req_zone $binary_remote_addr zone=hcrapi:10m rate=2r/s;
include /www/server/nginx/conf/cloudflare-realip.conf;
```

Generate that include once, and again whenever Cloudflare changes its ranges:

```sh
sudo sh deploy/cloudflare-realip.sh > /www/server/nginx/conf/cloudflare-realip.conf
sudo nginx -t && sudo nginx -s reload
```

Three things to get right, in descending order of how easily they are missed:

1. **Real client IP.** Behind Cloudflare, `$remote_addr` is a Cloudflare edge
   address. Without `set_real_ip_from` + `real_ip_header CF-Connecting-IP` the
   rate limit keys every visitor to the same few addresses — so it throttles
   your users collectively, and the access log records nothing useful. Trusting
   `CF-Connecting-IP` *without* the ranges is worse still: then anyone can claim
   to be anyone by sending the header themselves.
2. **Do not add CORS headers in nginx.** `hcr-server` emits them itself, preflight
   included. Adding them in nginx too sends each header twice, and a browser
   rejects a duplicated CORS header outright — so the well-meant fix is what
   breaks the site. If preflight fails, the thing to check is `HCR_CORS_ORIGIN`.
3. **`client_max_body_size`.** aaPanel defaults to 50m. Program IR is a few kB;
   the config sets 256k so a hostile body is rejected before it is buffered.

If aaPanel's free WAF is enabled, confirm it passes `POST` with a JSON body and
does not touch `OPTIONS` — it inspects request bodies and this API is all JSON.

Before relying on the real-IP configuration, check the module is compiled in.
nginx refuses to start on an unknown directive, so this takes the site down
rather than degrading it:

```sh
nginx -V 2>&1 | tr ' ' '\n' | grep realip     # expect --with-http_realip_module
nginx -t                                       # after every change, before reload
```

If it is absent, drop both the `include` and the `limit_req` line and use
Cloudflare's own rate limiting — limiting on Cloudflare's addresses is worse than
not limiting at all.

> Not machine-validated: these files were reviewed against nginx's directive
> contexts by hand, not run through `nginx -t` here. Run `nginx -t` before
> reloading.

### 2.2 Reverse proxy — Caddy

`deploy/Caddyfile`, if you would rather not run a panel. Functionally the same;
it obtains and renews the certificate itself, and its rate limiting needs a
third-party module (`caddy add-package github.com/mholt/caddy-ratelimit`) where
nginx has `limit_req` built in.

### Configuration

| Variable | Default | Notes |
| --- | --- | --- |
| `HCR_SIGNING_KEY` | **required** | HMAC key for item references, ≥32 bytes. The server refuses to start without it. |
| `HCR_BIND` | `127.0.0.1:18623` | Loopback; Caddy fronts it. |
| `HCR_CORS_ORIGIN` | *absent* | `https://hcr.pmine.org`. Absent sends no CORS headers, which breaks the frontend. |
| `HCR_USAGE_LOG` | *absent* | Path to append usage events to. Omit to collect nothing. |

`HCR_SIGNING_KEY` must be **stable across restarts** — it signs the `itemRef`
tokens binding a session's item to its response, so rotating it invalidates every
reference in flight.

`cargo run --example serve` is the development server and is not deployable: it
binds loopback, allows the Vite dev origin, and its key is a constant printed in
its own source.

### DNS

Point `hcrapi.pmine.org` at the VPS, proxied through Cloudflare (orange cloud).
Then restrict the VPS firewall to Cloudflare's ranges, or the proxy — and the
rate limiting in front of it — can be walked around by hitting the IP directly.

## 3. Usage collection

`HCR_USAGE_LOG` appends one JSON object per line. It is two things at once:

1. **The response data item calibration needs.** [`07-CALIBRATION.md`](07-CALIBRATION.md)
   describes refitting difficulty from real responses; until now nothing
   persisted any. A `submission` row is exactly an IRT datum — one person, one
   item, one outcome.
2. **Whether the thing gets used** — rounds played, programs written, where
   people stop.

Three event kinds: `submission`, `sessionResponse`, `matchResults`. The full
schema, and the reasoning, is in `crates/hcr_service/src/usage.rs`.

**Not recorded**, deliberately:

- **Display names.** Free text a player typed, which is where a real name or an
  email would end up. Grouping by `playerId` answers the same questions.
- **IP addresses.** Never, in this log.
- **Program source.** A learner's work; the shape metrics (`blocks`,
  `commands`, `durationMs`) carry the analysis value without archiving it.

`playerId` is a random identifier the browser generates and keeps in
`localStorage`. It is not an account and the server never checks it — treat it as
"probably the same browser", nothing stronger. Say so in any write-up that uses
this data.

Rotation is logrotate's job (`deploy/hcr-usage.logrotate`, `copytruncate` because
the server holds the file open). A write failure is reported once to the journal
and then swallowed: losing telemetry must never take the service down.

Quick look at what has been collected:

```sh
jq -r 'select(.kind=="submission") | [.challengeId, .completionScore] | @tsv' \
  /var/lib/hcr/usage.jsonl | sort | uniq -c | sort -rn | head
```

## 4. Operational notes

- **A restart drops every live round.** All state is in memory; there is no
  database. Rounds are minutes long, so pick a quiet moment.
- **The sweeper runs every 60 s** and evicts idle sessions, finished rounds
  (15 min after anyone last looked) and abandoned lobbies (30 min). Without it
  the process would grow for as long as it ran; it is started by the binary, and
  logs when it reclaims anything.
- **The catalog is fixed at boot** from `hcr_service::seed`. There is no
  authoring endpoint — adding a challenge means a redeploy.
- **Rate limiting lives in the reverse proxy**, not in the service: the only
  caller identity the service has is one the client chooses, so a limit there is
  bypassed by picking a new id. The proxy sees the address — provided the
  real-IP configuration in §2.1 is in place.

## 5. Checking a deployment

```sh
curl -s https://hcrapi.pmine.org/api/v1/time
curl -s https://hcrapi.pmine.org/api/v1/challenges | jq -r '.[].id'

curl -si -X OPTIONS https://hcrapi.pmine.org/api/v1/challenges \
  -H 'Origin: https://hcr.pmine.org' | grep -i access-control-allow-origin
```

The frontend's own live suite is the better check — it drives the real client
classes through a whole competitive round (lobby, reveal, submit, deadline,
standings):

```sh
cd HCR_Simulator_Frontend
HCR_API_BASE_URL=https://hcrapi.pmine.org npm test -- tests/integration
```

It self-skips when the API is unreachable, so a green run with 6 skips means it
never connected.

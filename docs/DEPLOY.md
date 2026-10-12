# Deployment — `web.hcr.rs` and `api.hcr.rs`

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
VITE_HCR_API_BASE_URL = https://api.hcr.rs
```

> Vite inlines `import.meta.env.*` at **build** time. Changing it means a rebuild.
> A preview built without it silently falls back to offline practice mode
> (`resolveServices.ts`) — which looks like a working app, so it is the confusing
> failure rather than the loud one.

`public/_headers` ships a CSP whose `connect-src` allows `'self'` and
`https://api.hcr.rs` only. If the API hostname changes, that file changes
with it or the browser blocks every request.

Add `web.hcr.rs` as a custom domain; Cloudflare issues the certificate.

**Favicon:** `index.html` references `/favicon.png`. Save the site mark to
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
cargo build --locked --release -p hcr --features hotaru --bin hcr-server
sudo install -m0755 target/release/hcr-server /usr/local/bin/
```

### Install

```sh
sudo useradd --system --no-create-home --shell /usr/sbin/nologin hcr
sudo install -d -m0750 -o hcr -g hcr /var/lib/hcr

sudo install -m0644 deploy/hcr-server.service   /etc/systemd/system/
sudo install -m0644 deploy/hcr-usage.logrotate  /etc/logrotate.d/hcr-usage

sudo install -m0640 -o root -g hcr deploy/hcr-server.env.example /etc/hcr-server.env
sudo sed -i '/^HCR_SIGNING_KEY=/d' /etc/hcr-server.env
# printf is a shell builtin, so the key is on no process's command line.
printf 'HCR_SIGNING_KEY=%s\n' "$(openssl rand -hex 32)" | sudo tee -a /etc/hcr-server.env >/dev/null

sudo systemctl daemon-reload
sudo systemctl enable --now hcr-server

sudo systemctl restart hcr-server
```

Then put a reverse proxy in front — see §2.1 (nginx/aaPanel) or §2.2 (Caddy).

### Re-install

``````bash
cargo build --locked --release -p hcr --features hotaru --bin hcr-server
sudo install -m0755 target/release/hcr-server /usr/local/bin/
sudo systemctl restart hcr-server
sudo cp /etc/hcr-server.env /etc/hcr-server.env.bak

sudo install -m0755 target/release/hcr-server /usr/local/bin/

sudo install -m0644 deploy/hcr-usage.logrotate /etc/logrotate.d/hcr-usage

sudo sed -i 's|^HCR_CORS_ORIGIN=.*|HCR_CORS_ORIGIN=https://web.hcr.rs,hcr://app|' /etc/hcr-server.env
grep -q '^HCR_USAGE_LOG=' /etc/hcr-server.env || \
echo 'HCR_USAGE_LOG=/var/lib/hcr/usage.jsonl' | sudo tee -a /etc/hcr-server.env

sudo grep -c '^HCR_SIGNING_KEY=replace-me' /etc/hcr-server.env   # must print 0

sudo systemctl restart hcr-server
``````



### 2.1 Reverse proxy — nginx via aaPanel

`deploy/nginx-hcrapi.conf` is the site body. Create the site in aaPanel, let the
panel issue and renew the certificate, then replace the generated `location /`
with that file's contents.

Two pieces live in the `http {}` context instead (App Store → Nginx → Settings →
Configuration), because nginx will not accept them anywhere else. On a server
set up earlier, replace the old `limit_req_zone $binary_remote_addr
zone=hcrapi…` line rather than adding these beside it (nginx refuses a zone
bound to two keys), and update `http {}` before installing the new site body,
which needs `hcrpoll`; then run `sudo nginx -t`:

```nginx
# IPv4: the address. IPv6: the /64 (first 8 bytes).
# shortcut: a routed /48 still gets 65k buckets; move to a Cloudflare rate-limiting rule if the access log shows it.
map $binary_remote_addr $hcr_client { "~(?s)^(?<hcr_v6net>.{8}).{8}$" $hcr_v6net; default $binary_remote_addr; }
map $request_method $hcr_read  { GET $hcr_client; OPTIONS $hcr_client; default ""; }
map $request_method $hcr_write { GET ""; OPTIONS ""; default $hcr_client; }
limit_req_zone $hcr_read  zone=hcrpoll:10m rate=30r/s;
limit_req_zone $hcr_write zone=hcrapi:10m  rate=4r/s;
include /www/server/nginx/conf/cloudflare-realip.conf;
```

Generate that include once, and again whenever Cloudflare changes its ranges:

```sh
conf=/www/server/nginx/conf/cloudflare-realip.conf
tmp=$(mktemp) && sh deploy/cloudflare-realip.sh > "$tmp" \
  && { [ ! -e "$conf" ] || sudo cp -p "$conf" "$conf.bak"; } \
  && sudo install -m0644 "$tmp" "$conf" && sudo nginx -t && sudo nginx -s reload
```

If a fetch fails or a line is not a plain CIDR range, the script exits non-zero
and the chain stops before the live file is touched. If `nginx -t` fails, put
the previous file back with `sudo mv "$conf.bak" "$conf"` before anything
reloads nginx.

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

If it is absent, drop both the `include` and the `limit_req` lines and use
Cloudflare's own rate limiting — limiting on Cloudflare's addresses is worse than
not limiting at all.

> Checked on aaPanel's nginx 1.30.4 (PCRE 8.43) with `nginx -t` and a live run:
> addresses in one IPv6 /64 share a bucket, and GET and POST use separate zones.
> Another build can differ, so run `nginx -t` before reloading; if it rejects
> the first `map`, drop it and use `$binary_remote_addr` in place of
> `$hcr_client` in the other two.

### 2.2 Reverse proxy — Caddy

`deploy/Caddyfile`, if you would rather not run a panel. Functionally the same;
it obtains and renews the certificate itself. It needs two third-party modules
where nginx has the features built in: rate limiting, and Cloudflare's ranges for
trusting `CF-Connecting-IP`. Adding a module replaces the binary, so restart:

```sh
sudo caddy add-package github.com/mholt/caddy-ratelimit
sudo caddy add-package github.com/WeidiDeng/caddy-cloudflare-ip
caddy validate --config deploy/Caddyfile
sudo install -m0644 deploy/Caddyfile /etc/caddy/Caddyfile && sudo systemctl restart caddy
```

### Configuration

| Variable | Default | Notes |
| --- | --- | --- |
| `HCR_SIGNING_KEY` | **required** | HMAC key for item references, ≥32 bytes. The server refuses to start without it. |
| `HCR_BIND` | `127.0.0.1:18623` | Loopback; Caddy fronts it. |
| `HCR_CORS_ORIGIN` | *absent* | Comma-separated allowlist, e.g. `https://web.hcr.rs,hcr://app`. Only a listed origin is echoed back. Absent sends no CORS headers, which breaks the frontend. |
| `HCR_USAGE_LOG` | *absent* | Path to append usage events to. Omit to collect nothing. |

`HCR_SIGNING_KEY` must be **stable across restarts** — it signs the `itemRef`
tokens binding a session's item to its response, so rotating it invalidates every
reference in flight.

`cargo run --example serve` is the development server and is not deployable: it
binds loopback, allows the Vite dev origin, and its key is a constant printed in
its own source.

### DNS and TLS

Point `api.hcr.rs` at the VPS, proxied through Cloudflare (orange cloud), and set
the zone's SSL/TLS mode to **Full (strict)**. "Flexible" sends the
Cloudflare→origin hop in cleartext, and "Full" accepts any certificate there, so
anyone on that path can read player ids and item references. The panel's (or
Caddy's) own certificate satisfies strict mode.

Restrict the VPS firewall to Cloudflare's ranges. A direct hit on the IP is
still rate-limited, since the real-IP ranges mean it is keyed on the caller's own
address, but it skips Cloudflare's WAF, DDoS protection and rate-limiting rules.

The firewall admits every Cloudflare zone, not just yours: anyone can point
their own zone at this IP. To admit only yours, use zone-level Authenticated
Origin Pulls with a client certificate issued by your own CA. Cloudflare's
shared AOP certificate proves only "some Cloudflare zone", so it does not help.
Upload the certificate as the zone's AOP certificate and enable AOP in Cloudflare
(their "Authenticated Origin Pulls" docs) **before** adding these lines to the
site's `server {}` block, or every request is refused:

```nginx
ssl_client_certificate /www/server/nginx/conf/hcr-aop-ca.pem;
ssl_verify_client on;
```

A request to the origin IP without that certificate then gets 400.

## 3. Usage collection

`HCR_USAGE_LOG` appends one JSON object per line. It is two things at once:

1. **The response data item calibration needs.** [`07-CALIBRATION.md`](07-CALIBRATION.md)
   describes refitting difficulty from real responses; until now nothing
   persisted any. A `submission` row is exactly an IRT datum — one person, one
   item, one outcome.
2. **Whether the thing gets used** — rounds played, programs written, where
   people stop.

Four event kinds: `submission`, `sessionResponse`, `matchResults` and `lesson`.
The full schema, and the reasoning, is in `crates/hcr/src/usage.rs`.

Run the commands in this section from a root shell (`sudo -s`): the log, its
archives and the backups are readable only by root and hcr.

The first three are the server's own account of work it did. **`lesson` is not:**
the lessons run and score entirely in the browser, so those rows carry what a
client reported and the service can verify none of it. They exist because the
course was otherwise invisible here — a lesson never submits, so a log that only
saw submissions saw nothing of the thing most people actually use. Read them for
purpose 2 above and filter them out for purpose 1:

```sh
# Usage: where do people stop?
jq -r 'select(.kind=="lesson") | [.lessonId, .section, .outcome] | @tsv' \
  /var/lib/hcr/usage.jsonl | sort | uniq -c | sort -rn | head

# Calibration input: server-scored responses only, never lesson rows.
jq -c 'select(.kind=="submission")' /var/lib/hcr/usage.jsonl
```

### Backing it up, and deploying over it

A deploy replaces `/usr/local/bin/hcr-server` and never opens `/var/lib/hcr`.
The new process opens the same path in append mode and carries on: nothing is
rewritten, there is no migration step, and there is nothing to do to the log
itself. `an_upgrade_appends_to_the_log_the_previous_build_left` in
`crates/hcr/tests/service.rs` holds that to be true of the file, not just of the
code — it seeds a log with rows an older build wrote, runs this build against it,
and checks the old bytes are still a byte-for-byte prefix.

**Back up the directory, not the file.** Right after a rotation `usage.jsonl` is
legitimately absent or zero bytes and the day's rows are in `usage.jsonl.1`, so a
backup of `usage.jsonl` alone captures nothing — and, worse, succeeds while doing
it:

```sh
(umask 077 && tar czf /var/backups/hcr-usage-$(date +%F-%H%M).tar.gz -C /var/lib/hcr .)
tar tzvf /var/backups/hcr-usage-*.tar.gz    # non-empty, or the backup is a shell
```

A verification against an empty baseline passes vacuously — `cmp` of nothing
against nothing is equal — so check the baseline has bytes before believing the
comparison:

```sh
BAK=/var/backups/usage-BEFORE.jsonl
SIZE=$(wc -c < "$BAK" 2>/dev/null || echo 0)
if [ "${SIZE:-0}" -lt 1 ]; then
  echo "baseline absent or empty: this check proves nothing"
else
  head -c "$SIZE" /var/lib/hcr/usage.jsonl | cmp -s - "$BAK" \
    && echo "the old bytes are untouched" || echo "the old region changed"
fi
```

After the restart, confirm collection actually resumed. The service **starts
anyway** when the log cannot be opened — losing telemetry must not take the
service down — so a `chown` that went wrong during a deploy is silent except for
one line in the journal:

```sh
sudo journalctl -u hcr-server -b | grep -E "recording usage to|could not open usage log"
```

`recording usage to /var/lib/hcr/usage.jsonl` is the line that says it worked. If
instead it could not open the file, the usual cause is a deploy step run as root
leaving the file root-owned; `sudo chown hcr:hcr /var/lib/hcr/usage.jsonl` and
restart.

### Reading rows written by more than one build

The file is append-only and nothing ever rewrites a line, so a log that has been
collecting for a while holds rows from every build that has run against it.
Three things about such a file are easy to read wrongly:

**`matchResults` used to be written once per reader.** The row is recorded when
the standings are published, but every client in the room polls for them the
moment the round closes, and builds before this one recorded on every call — a
seventeen-player round left fifteen identical rows seconds apart. Those rows are
still there and no fix reaches them. Deduplicate, which reads both eras
correctly:

```sh
# Rounds actually played. A room keeps its matchId across a rematch, so the
# round is (matchId, challengeId), not matchId.
jq -r 'select(.kind=="matchResults") | [.matchId, .challengeId] | @tsv' \
  /var/lib/hcr/usage.jsonl | sort -u | wc -l
```

**`scoringDigest` names the weighting a `submission` was graded against.**
`challengeVersion` was supposed to answer that and does not: nothing forces the
version up when only the scoring block beneath it changes, and that has already
happened — `neat-short-cap` moved from `referenceProgramCost` 6.25 /
`referenceTimeMs` 5645 to 1.25 / 1000 on 2026-08-27 and stayed version 1. Rows
from either side of that date claim the same version and mean different things.

| Digest | `neat-short-cap` scoring | Written |
| --- | --- | --- |
| *(absent)* | not recorded — resolve from `ts` | before the field existed |
| `2081283affc7cc7d` | 6.25 / 5645 | until 2026-08-27 |
| `448b870304930bb7` | 1.25 / 1000 | after 2026-08-27 |

An absent digest means **not recorded**, never "the current one".

What the reweighting spoils is narrower than it looks:

- `completionScore` is **unaffected** — it compares voxels against the target and
  never consults the scoring block. The calibration datum this section exists to
  collect survived, which is why [`07-CALIBRATION.md`](07-CALIBRATION.md) fits
  on it.
- `finalScore` is **not comparable** across a change of digest, and neither is
  the adaptive `rawScore`, which is `finalScore / 100`. Ability estimates either
  side of one are in different units. Group by `scoringDigest` before comparing
  either, and treat absent as its own group.

**A `submission` row is an attempt, not a person's answer to an item.**
Resubmission is unlimited by design — the best attempt counts — so one learner
can leave dozens of rows for one item, and the log has several runs of identical
rows seconds apart. That is fine for usage and wrong for calibration, where a
repeated response breaks the local independence an IRT fit assumes. Take one row
per (player, item, round) before a refit:

```sh
# First attempt per person per item per round. `.playerId // .sessionId`
# because a submission made inside an adaptive session carries no playerId —
# only the match path passes one — so the session id is the person key there.
jq -r -s 'map(select(.kind=="submission"))
  | group_by([.playerId // .sessionId, .challengeId, .matchId])
  | map(min_by(.ts))
  | .[] | [.playerId // .sessionId, .challengeId, .completionScore] | @tsv' \
  /var/lib/hcr/usage.jsonl
```

**Not recorded**, deliberately:

- **Display names.** Free text a player typed, which is where a real name or an
  email would end up. Grouping by `playerId` answers the same questions.
- **IP addresses.** Never, in this log.
- **Program source.** A learner's work; the shape metrics (`blocks`,
  `commands`, `durationMs`) carry the analysis value without archiving it.
- **Anything without explicit research consent.** A `submission`,
  `sessionResponse` or `lesson` row is written only when the request carries
  `X-HCR-Research-Program-And-Scores: granted` (for a `sessionResponse`, the
  submission it answers). `matchResults` names no player and is always written.

`playerId` is a random identifier the browser generates and keeps in
`localStorage`. It is not an account and the server never checks it — treat it as
"probably the same browser", nothing stronger. Say so in any write-up that uses
this data.

Rotation is logrotate's job (`deploy/hcr-usage.logrotate`), and it renames the
live file. The server holds the file open for the life of the process, so it
checks its descriptor against the path before every write and reopens when they
have parted company: the rename is survived without a restart, the next row
recreates `usage.jsonl` (0640, under the unit's `UMask=0027`), and one
`file was rotated, reopened it` line goes to the journal. Until that row the file
is absent, which is normal. Install this logrotate file only with a build that
has the descriptor check: an older one keeps appending to the renamed
`usage.jsonl.1` until it is restarted, and nothing rotates in the meantime.

A write failure is reported once to the journal and then swallowed: losing
telemetry must never take the service down.

**If you find `usage.jsonl` missing or at zero bytes** while a numbered archive
keeps growing, that is the pre-fix failure: an older build is still appending to
the renamed inode. Check with `sudo ls -l /proc/$(pidof hcr-server)/fd | grep usage`.
The data is in whichever archive the descriptor points at — keep it — and a
restart is the only way to move a descriptor. A telltale sign is an archive with
a *newer* mtime than `.1`, which normal rotation can never produce.

Quick look at what has been collected:

```sh
jq -r 'select(.kind=="submission") | [.challengeId, .completionScore] | @tsv' \
  /var/lib/hcr/usage.jsonl | sort | uniq -c | sort -rn | head
```

## 4. Operational notes

- **A restart drops every live round.** All state is in memory; there is no
  database. Rounds are minutes long, so pick a quiet moment.
- **The sweeper runs every 60 s** and evicts idle sessions, closed rounds
  (15 min after the close or the last look, whichever is later) and abandoned
  lobbies (30 min). Without it the process would grow for as long as it ran; it
  is started by the binary, and logs when it reclaims anything.
- **The catalog is fixed at boot** from `hcr::seed`. There is no
  authoring endpoint — adding a challenge means a redeploy.
- **Rate limiting lives in the reverse proxy**, not in the service: the only
  caller identity the service has is one the client chooses, so a limit there is
  bypassed by picking a new id. The proxy sees the address — provided the
  real-IP configuration in §2.1 is in place.

## 5. Checking a deployment

```sh
curl -s https://api.hcr.rs/api/v1/time
curl -s https://api.hcr.rs/api/v1/challenges | jq -r '.[].id'

curl -si -X OPTIONS https://api.hcr.rs/api/v1/challenges \
  -H 'Origin: https://web.hcr.rs' | grep -i access-control-allow-origin
```

The frontend's own live suite is the better check — it drives the real client
classes through a whole competitive round (lobby, reveal, submit, deadline,
standings):

```sh
cd HCR_Simulator_Frontend
HCR_API_BASE_URL=https://api.hcr.rs npm test -- tests/integration
```

It self-skips when the API is unreachable, so a green run with 6 skips means it
never connected.

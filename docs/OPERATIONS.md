# Operations runbook — Tuvalu Court Register

One binary (`tuvalu-court`), one data directory, no external services. Pick one
of the three install shapes below.

## 1. Install

### Option A — binary + systemd + nginx

```sh
# Build (frontend first, it is embedded into the binary)
cd web && npm ci && npm run build && cd ..
cargo build --release --locked

# Install
sudo install -Dm755 target/release/tuvalu-court /opt/tuvalu-court/tuvalu-court
sudo useradd --system --home /var/lib/tuvalu-court --shell /usr/sbin/nologin tuvalu-court
sudo install -d -o tuvalu-court -g tuvalu-court -m 0700 /var/lib/tuvalu-court

# Config: copy .env.example to /etc/tuvalu-court.env and set
#   TCR_MODE=production  TCR_DATA_DIR=/var/lib/tuvalu-court
#   TCR_BIND=127.0.0.1:8088  TCR_COOKIE_SECURE=true
sudo cp .env.example /etc/tuvalu-court.env && sudo chmod 640 /etc/tuvalu-court.env

# Service
sudo cp deploy/tuvalu-court.service /etc/systemd/system/
sudo systemctl daemon-reload && sudo systemctl enable --now tuvalu-court
```

The unit runs the service as `tuvalu-court`, sandboxed, capped at 200 MB RAM /
50% CPU. Data lives in `/var/lib/tuvalu-court` (created via `StateDirectory`).

Reverse proxy: copy `deploy/nginx-tuvalu.conf` to
`/etc/nginx/sites-available/tuvalu`, adjust the server name, issue a certificate
(`certbot --nginx -d <host>`), enable and reload nginx. The app trusts
`X-Forwarded-For`; the shipped config overwrites it — keep it that way.

### Option B — Docker Compose

```sh
cp .env.example .env          # set TCR_MODE, TCR_COOKIE_SECURE, etc.
docker compose up -d          # http://127.0.0.1:8088, loopback only
docker compose logs -f
```

Data is in the `court-data` named volume; the container is read-only with a
256 MB / 0.5 CPU limit. Terminate TLS at a reverse proxy in front.

### Option C — Kamal (used for the public demo)

`config/deploy.yml` deploys the demo at `tuvalu.shelfcompass.com`: image built
locally (arm64), pushed to ghcr.io, served via kamal-proxy with Let's Encrypt
TLS, data in the `tuvalu_data` volume.

```sh
export KAMAL_REGISTRY_PASSWORD=$(gh auth token)   # needs write:packages
kamal setup    # first time
kamal deploy   # subsequent releases
```

Build the image on a workstation, not on the app host — a Rust release build is
too heavy for the small server.

## 2. First production setup

With `TCR_MODE=production`, on the server:

```sh
# 1. Create the first administrator (password ≥ 12 chars, prompted on stdin)
tuvalu-court create-user admin "Registry Administrator" --perm admin.users --perm admin.settings

# 2. Create staff accounts the same way. Court/judicial powers cannot be granted
#    from the admin UI — grant them from the CLI as the court's authority:
tuvalu-court grant elena case.view_all
tuvalu-court grant elena case.assign_judge
tuvalu-court grant viktor decision.finalise
tuvalu-court revoke <user> <permission>    # to take one back
```

Full permission list: `ADMIN_GRANTABLE`/`perm::ALL` in `src/policy.rs` and the
table in `SECURITY.md`.

**First login:** sign in with the password → the app answers
`enroll_required` → scan the `otpauth://` URI (or enter the secret) in an
authenticator → confirm with a code. From then on login = password + TOTP.
An admin can reset a user's enrolment via `reset-password`/user management, and
the user re-enrols at next login.

Then, in **Settings** (as an `admin.settings` user): set the court name,
registries/series, rooms, reference lists and message templates for the real
court. The seeded values are examples — replace them.

## 3. Upgrade

```sh
# Always back up first (see §4)
tuvalu-court backup /safe/place/pre-upgrade.tcrb /secure/backup.key

# Binary install:
sudo install -Dm755 target/release/tuvalu-court /opt/tuvalu-court/tuvalu-court
sudo systemctl restart tuvalu-court

# Compose: docker compose pull|build && docker compose up -d
# Kamal: kamal deploy
```

Schema migrations run automatically at startup (`PRAGMA user_version`), both
for the production database and for any existing demo sandboxes on first touch.

Migration 0007 preserves dispatch ids, items, attempts, confirmations and mailbox history while
adding terminal `superseded` and reviewed hearing/material bindings. Pending legacy working material
is labelled DRAFT / working material and its old review is cleared. Legacy invitations without a
provable hearing binding are superseded when processed; prepare a new notice and preview it.
Sent history is unchanged. A superseded item cannot be retried: prepare and review a fresh message.

## 4. Backup and restore

```sh
# One-time key — 32 random bytes as hex. KEEP IT OFF THE SERVER AND OUT OF GIT.
tuvalu-court gen-key /secure/backup.key

tuvalu-court backup /backups/court-2026-10-08.tcrb /secure/backup.key
# → "Backup created: N tables, M files, K audit events, B bytes."
```

The archive (`TCRB1`) contains the DB snapshot, every stored file and a manifest
with row counts + SHA-256 of each file + the audit chain head, all encrypted
with XChaCha20-Poly1305. Safe to run on a live server (`VACUUM INTO` snapshot).
Schedule with cron or a systemd timer.

```sh
# Restore — target MUST be a new or empty directory; everything is verified
# (authentication, manifest, row counts, FK integrity, file checksums, audit
# head) before anything is published:
tuvalu-court restore /backups/court-2026-10-08.tcrb /secure/backup.key /var/lib/tuvalu-court
# → "Backup restored: N tables, M files, K audit events."
```

The user-facing case export zip is **not** a backup — it cannot restore
restricted data.

## 5. Audit verification

```sh
tuvalu-court verify-audit
# "Audit chain intact: N events."  (exit 0)
# "Audit chain BROKEN at event #id" (exit 2 — investigate before trusting the DB)
```

Run it after restores and periodically via cron.

## 6. Logs

The binary logs to stderr (`tracing`), level via `RUST_LOG` (default `info`).

```sh
journalctl -u tuvalu-court -f        # systemd
docker compose logs -f               # compose
kamal app logs -f                    # kamal
```

Health endpoint: `GET /api/health`.

## 7. Demo housekeeping

- Visitors can reset only their **own** sandbox (Demo banner → Reset). There is
  no global reset — by design.
- Expired sandboxes are swept automatically; at capacity only sandboxes idle
  >2 h are evicted, else new visitors get `503`.
- To remove a demo installation: `kamal remove` (or `docker compose down -v`,
  or stop the unit and delete the data dir). Deleting the data directory
  removes all sandboxes — that is the only "global reset".

## 8. Resource limits

- systemd unit: `MemoryMax=200M`, `CPUQuota=50%`, `TasksMax=64`, strict
  filesystem sandboxing (writes only to `/var/lib/tuvalu-court`).
- compose: `mem_limit: 256m`, `cpus: 0.5`, read-only root fs.
- The app uses a fixed 2-worker tokio runtime; SQLite WAL; no external deps.
  Expect tens of MB RSS in normal use.
- nginx `client_max_body_size` must exceed `TCR_UPLOAD_MAX_MB` plus multipart
  overhead (shipped config: 52 m for a 50 MB import package).

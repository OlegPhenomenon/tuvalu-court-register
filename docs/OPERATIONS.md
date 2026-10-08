# Operations runbook — Tuvalu Court Register

One binary (`tuvalu-court`), one private data directory, SQLite. Production email
uses SMTP; file antivirus uses a court-operated ClamAV `clamd` service. Demo
always uses the local mailbox and format checks only, regardless of SMTP settings.

## Install with systemd

Build the frontend, then the binary:

```sh
cd web && npm ci && npm run build && cd ..
cargo build --release --locked
sudo install -Dm755 target/release/tuvalu-court /opt/tuvalu-court/tuvalu-court
sudo useradd --system --home /var/lib/tuvalu-court --shell /usr/sbin/nologin tuvalu
sudo install -d -o tuvalu -g tuvalu -m 0700 /var/lib/tuvalu-court
sudo install -o root -g tuvalu -m 0640 .env.example /etc/tuvalu-court.env
```

Edit `/etc/tuvalu-court.env`: set `TCR_MODE=production`,
`TCR_DATA_DIR=/var/lib/tuvalu-court`, `TCR_BIND=127.0.0.1:8088`,
`TCR_COOKIE_SECURE=true`, and `TCR_CLAMD=tcp://127.0.0.1:3310` (or
`unix:/run/clamav/clamd.ctl`). Give the service user access to the socket. Keep
clamd private and update its signatures. If the court explicitly chooses to
operate without antivirus, set `TCR_AV=off`; Settings and every uploaded file
then say **format checks only, no antivirus**. Otherwise production refuses to
start without a scanner endpoint. A configured scanner always takes precedence
and fails closed, even if `TCR_AV=off` is also present.

For outbound mail set `TCR_SMTP_URL=smtp://user:password@host:587` (STARTTLS
required) or `smtps://user:password@host:465` (implicit TLS), plus
`TCR_MAIL_FROM=Registry <registry@example.invalid>` using the court's configured
address. URL-encode credentials. TLS uses rustls, ring and bundled webpki roots;
no OpenSSL or aws-lc/cmake is needed. URL TLS overrides are refused.
`TCR_SMTP_TIMEOUT_SECS` defaults to 20; `TCR_SCAN_TIMEOUT_MS` defaults to 10000.
Never commit this env file or credentials.

```sh
sudo cp deploy/tuvalu-court.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now tuvalu-court
```

The unit's `EnvironmentFile` is **`/etc/tuvalu-court.env`**. Its executable also
loads that exact file via `--env-file`. Commands below use the same absolute
binary path, environment file and user. Literal `KEY=value` entries, comments,
`export KEY=value` and single/double quoted values are supported; shell
interpolation/commands are never executed. Explicit file values override shell
environment values. `TCR_ENV_FILE=/etc/tuvalu-court.env` is an alternative to the
CLI option. Do not mix a different shell data path with the service's env file.

Use `deploy/nginx-tuvalu.conf` behind HTTPS, with a certificate for the chosen
host. The proxy must overwrite forwarded IP headers. The systemd unit limits
memory to 200 MB, CPU to 50%, and writes to `/var/lib/tuvalu-court`.

## First accounts and normal operation

Start the server once before maintenance commands: startup initialises the
schema and creates a random **installation id**, visible in admin Settings.
`create-user`, `grant`, `revoke`, `backup` and `verify-audit` require an existing
initialised production database. They print its resolved data directory and
installation id before acting; they never silently create `./data`.

```sh
sudo -u tuvalu /opt/tuvalu-court/tuvalu-court --env-file /etc/tuvalu-court.env create-user demoadmin "DEMO Registry Administrator" --perm admin.users --perm admin.settings
sudo -u tuvalu /opt/tuvalu-court/tuvalu-court --env-file /etc/tuvalu-court.env create-user demojudge "DEMO Judge" --judge --perm decision.draft --perm decision.finalise
sudo -u tuvalu /opt/tuvalu-court/tuvalu-court --env-file /etc/tuvalu-court.env grant democlerk case.view_all
sudo -u tuvalu /opt/tuvalu-court/tuvalu-court --env-file /etc/tuvalu-court.env revoke democlerk case.view_all
```

Names above are fictional examples; create the installation's accounts using
its approved staff list. Passwords (at least 12 characters) are read from stdin,
never command arguments. First login: password → TOTP enrolment → forced change
of temporary password, with a fresh TOTP code. After MFA, records APIs remain
blocked until the change. Mode discovery, login, logout, me, TOTP and password
change stay available, including after a reload before TOTP. Settings includes **Change my password** for every
production user. Password changes revoke other sessions and preserve TOTP.
Technical administrators cannot reset judges or protected accounts.

Set court name, number series, reference lists, rooms and templates in Settings.
The seeded reference values are examples requiring court approval.

Production without SMTP leaves email queued, showing **Mail transport not
configured** in Settings and the dispatch. A configured transport failure is
recorded as `failed`; automatic retries use 60-second exponential backoff, capped
at 64 minutes, with a new delivery attempt each time. Only transport failures
retry automatically; changed permissions/files require review. Restarting after
SMTP configuration recovers queued items. Successful SMTP deliveries also appear
in the mailbox sent log marked **sent via SMTP**, with the exact queued versions.
Technical delivery is separate from human handover and legal service assessment.
SMTP acknowledgement loss or a crash after server acceptance can make delivery
ambiguous; a stable Message-ID helps receivers recognise repeats but cannot
provide universal exactly-once email delivery.

The worker commits an `in_flight` attempt with its claim, stable Message-ID and
exact attachment inventory before preparing or sending mail. SMTP runs outside
SQLite write transactions; other writes remain available during a mail outage.
It rechecks claim ownership, permissions and hearing/decision currency immediately
before sending. On startup or the next worker tick, an attempt older than twice
`TCR_SMTP_TIMEOUT_SECS` becomes failed with an ambiguous-delivery reason and
enters the normal backoff retry using the same Message-ID. A missing, unreadable
or corrupt attachment fails only its dispatch, with no automatic retry; review
the file and preview the dispatch before deliberately retrying.

Uploads appear as `pending_scan` while clamd runs. Infection, scanner error,
timeout or interrupted scan produces `quarantined`. Pending/quarantined files
cannot be downloaded, previewed, exported or attached to outgoing mail. A server
restart quarantines interrupted scans; submit a new version when the scanner is
healthy. File imports commit pending versions, then use the same scanner path
outside the import transaction. Format checks and
antivirus reduce risk; neither proves a file absolutely safe.

## Backup, restore, upgrade and audit

The service filesystem sandbox permits writing only under its data directory.
Choose backup/key paths accessible to `tuvalu`, copy encrypted archives off-host,
and keep the key separately with restricted permissions.

```sh
sudo -u tuvalu /opt/tuvalu-court/tuvalu-court --env-file /etc/tuvalu-court.env gen-key /var/lib/tuvalu-court/backup.key
sudo -u tuvalu /opt/tuvalu-court/tuvalu-court --env-file /etc/tuvalu-court.env backup /var/lib/tuvalu-court/pre-upgrade.tcrb /var/lib/tuvalu-court/backup.key
sudo -u tuvalu /opt/tuvalu-court/tuvalu-court --env-file /etc/tuvalu-court.env verify-audit
```

`TCRB1` contains the consistent SQLite snapshot, all immutable document versions
and blobs, grants and audit chain, encrypted with chunked XChaCha20-Poly1305.
User-facing case exports are not backups and cannot restore a court installation.
Full backups intentionally include quarantined records; restoring them preserves
the verdict and does not make their bytes available through application channels.

Restore reads the existing initialised database **inside the authenticated
backup**, so disaster recovery does not require a surviving source installation.
The destination must be an explicitly supplied new or empty directory. Before
writing it, the CLI prints the destination and backup installation id and requires
interactive entry of that id or `--yes`. It refuses noninteractive execution
without `--yes`. Authentication, counts, foreign keys, blob checksums and audit
head are verified before publication. The restored installation keeps its id.
For backups predating installation ids, confirmation uses a `legacy-…` fingerprint
of the authenticated database; startup migration then creates its persistent
random installation id. No older backup format is discarded.

Confirmation and restore stage plaintext only under the destination's private
`.restore-tmp`, removed on success or error; they do not decrypt archives into
the container's `/tmp` tmpfs. Allow disk space there for the decrypted archive
and verified database/files. If the process is killed, remove the destination's
unpublished `.restore-tmp` before retrying into an empty destination.

```sh
sudo -u tuvalu /opt/tuvalu-court/tuvalu-court --env-file /etc/tuvalu-court.env restore /var/lib/tuvalu-court/pre-upgrade.tcrb /var/lib/tuvalu-court/backup.key /var/lib/tuvalu-court/restored --yes
# Set TCR_DATA_DIR=/var/lib/tuvalu-court/restored in /etc/tuvalu-court.env before switching the service.
sudo -u tuvalu /opt/tuvalu-court/tuvalu-court --env-file /etc/tuvalu-court.env verify-audit
sudo systemctl restart tuvalu-court
```

For upgrades, take a backup, install the new binary and restart the service.
Migrations apply on startup to production and to demo sandboxes on first touch.
`verify-audit` exits 0 for an intact chain and 2 for a broken chain. It detects
changes within the documented threat model; it cannot prevent the server owner
from rewriting both database and chain.

Migration 0008 preserves dispatch ids, items, attempts, confirmations and mailbox history while
adding terminal `superseded` and reviewed hearing/material bindings. Pending legacy working material
is labelled DRAFT / working material and its old review is cleared. Legacy invitations without a
provable hearing binding are superseded when processed; prepare a new notice and preview it.
Sent history is unchanged. A superseded item cannot be retried: prepare and review a fresh message.

## Docker Compose

```sh
cp .env.example .env
# Edit the same production, SMTP and scanner settings described above.
docker compose up -d
docker compose exec -it court /usr/local/bin/tuvalu-court create-user demoadmin "DEMO Registry Administrator" --perm admin.users --perm admin.settings
docker compose exec court /usr/local/bin/tuvalu-court grant democlerk case.view_all
docker compose exec court /usr/local/bin/tuvalu-court revoke democlerk case.view_all
docker compose exec court /usr/local/bin/tuvalu-court gen-key /data/backup.key
docker compose exec court /usr/local/bin/tuvalu-court backup /data/court.tcrb /data/backup.key
docker compose exec court /usr/local/bin/tuvalu-court verify-audit
docker compose stop court
docker compose run --rm court restore /data/court.tcrb /data/backup.key /data/restored --yes
# Change TCR_DATA_DIR=/data/restored in .env, then start the service.
docker compose up -d
docker compose exec court /usr/local/bin/tuvalu-court verify-audit
```

`exec` inherits the running service's env; `run` loads the same `.env`. Files live
in the `court-data` named volume. For off-host archives, `docker compose cp` copies
them out; keep encryption keys separately. The root filesystem is read-only,
`/tmp` is tmpfs, memory is capped at 256 MB and CPU at 0.5. Clamd must be reachable
from inside the container; `127.0.0.1` refers to that container, not its host.

## Kamal

`config/deploy.yml` remains a public **DEMO** deployment: mailbox local, AV off by
design. For production, configure `TCR_MODE=production`, `TCR_CLAMD` (or explicit
`TCR_AV=off`) and `TCR_MAIL_FROM` in the deployment env and `TCR_SMTP_URL` as a Kamal
secret. Never store SMTP credentials in `env.clear`. Build on the workstation.

```sh
kamal setup
kamal deploy
kamal app exec -i 'create-user demoadmin "DEMO Registry Administrator" --perm admin.users --perm admin.settings'
kamal app exec -i 'grant democlerk case.view_all'
kamal app exec -i 'revoke democlerk case.view_all'
kamal app exec -i 'gen-key /data/backup.key'
kamal app exec -i 'backup /data/court.tcrb /data/backup.key'
kamal app exec -i 'restore /data/court.tcrb /data/backup.key /data/restored --yes'
# Set TCR_DATA_DIR=/data/restored in the deployment env and redeploy.
kamal deploy
kamal app exec -i 'verify-audit'
```

These commands start a new container. Its image ENTRYPOINT is already
`/usr/local/bin/tuvalu-court`, so pass only the subcommand. With `--reuse`,
Kamal uses `docker exec` instead and requires the full binary path. See
[Kamal's execution implementation](https://github.com/basecamp/kamal/blob/main/lib/kamal/commands/app/execution.rb).

Kamal exec inherits deployed env and runs as the image's unprivileged `court`
user, with the same `/data` volume. These maintenance commands require production;
they intentionally refuse to operate on demo sandboxes.

## Logs and demo housekeeping

```sh
journalctl -u tuvalu-court -f
docker compose logs -f
kamal app logs -f
```

Logs go to stderr at `RUST_LOG` level. Health endpoint: `GET /api/health`.
Demo visitors reset only their own sandbox. Expired sandboxes are swept; at
capacity only sandboxes idle for more than two hours are evicted. To remove an
installation, stop it and explicitly delete its data; no maintenance command does
that automatically. The proxy upload limit must exceed `TCR_UPLOAD_MAX_MB` plus
multipart overhead (the nginx example accommodates the 50 MB import cap).

## Responsible officers and contact corrections

Use the case summary's **Change responsible officer** action with staff-assignment permission and a reason.
Replacement ends the previous clerk assignment immediately; the result lists any other assignments that remain.
General case-view permissions can also preserve access. Registration defaults to the registering clerk;
choosing another officer needs staff-assignment permission and a reason. Judges use the judge-assignment action.

When a shared contact's editor says to ask the registry head, the head can open **Participants → Edit contact**
for records they can see and correct the contact without case-edit permission. A head must also have visibility
of every linked record; restricted cases still require an assignment or restricted-case permission. The demo
registry head is Elena. Hidden record names are not displayed in the explanation.

Migration 0011 is applied at startup and adds the five party-link lookup indexes. No data rewrite is needed.
Global audit redaction is computed for each viewer; stored audit events and chain verification remain intact.

## Workflow checks after upgrade

Legacy CSV preview rejects future closure dates using Pacific/Funafuti (UTC+12). Commit revalidates older
previews and returns `import_changed` if a previously accepted row now fails; preview the source again.
After reopening, close on or after the latest effective status date and the chosen evidence date.
“Open cases with no next step” excludes pending hearing outcomes/confirmations, handover or service
assessments, draft decisions and reopened cases awaiting a status decision. Hidden draft documents
still count as recorded work on visible cases, with neutral prompts. Retrying a service-assessment form
retains its command key and records only one assessment; open a new form for a deliberate new assessment.

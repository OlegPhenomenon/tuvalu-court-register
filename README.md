# Tuvalu Court Register

An independent, self-hosted prototype of a court registry case management system,
built for the **Office of the Judiciary, Tuvalu**. It covers the full
record-keeping cycle: receiving filings, registering cases, assigning staff and
the judge, scheduling hearings, recording outcomes, issuing decisions and
delivering permitted copies — with a tamper-evident audit trail.

**Live demo:** <https://tuvalu.shelfcompass.com> — every visitor gets an isolated
sandbox pre-loaded with fictional `DEMO` data and a persona switcher.

## What this is — and is not

- An internal registry application. It records what happened and what is due
  next. It does **not** determine guilt, the correctness of a judgment,
  jurisdiction or the consequences of a missed deadline — those decisions remain
  with an authorised person.
- Not an AI judge, not an e-library, not a public case catalogue. There is no
  public endpoint that accepts real filings.
- All demonstration names, case categories, numbers and documents are fictional
  and marked `DEMO`. No real e-mail is sent; outgoing messages land in a local
  mailbox viewer.

### Budget context

The Tuvalu National Budget 2026–2027 allocates **A$50 000** to the Office of the
Judiciary for a *Case Management System* (section 4.1.14, printed p. 60 /
PDF p. 61) [T1]. The National Budget 2024–2025 already listed the same item
among external proposals seeking funding (A$71 500, Schedule 2, printed p. 86 /
PDF p. 92) [T2].

Boundary: the budget confirms the need and the expenditure line — it does **not**
confirm a detailed technical specification, the absence of a contractor, or the
court's current records procedures. Everything in this repository is a design
proposal built on fictional data. See `ASSUMPTIONS.md` (last section) for what
must be confirmed with the court before real use. Section and feature numbers
(§N, C01–C18) refer to the project brief, which is not published in this repository.

## Features (spec C01–C18)

| Ref | Feature |
|---|---|
| C01 | Sign-in with password + TOTP code; court permissions are separate from administration; deactivation ends sessions and file access |
| C02 | Intake records with sender, channel, dates, attachments and paper location; supplements, duplicates, linking and return — never deleted |
| C03 | Case registration with unique per-series/year numbering, enforced in the database and idempotent under retries |
| C04 | Parties and organisations with a per-case role, representative and service contact; same names are never merged |
| C05 | Judge and staff assignments made by an authorised person with a mandatory reason; ending an assignment ends access, including old file URLs |
| C06 | Documents and versions with type, source, checksum, visibility and original location; uploads validated server-side |
| C07 | Multiple hearings per case with time, room, judge and status; double-booking prevented by database constraints |
| C08 | Adjournment/cancellation creates a linked new record, keeps reason and authoriser, frees the slot, re-lists who must be notified |
| C09 | Template-based notices and copies, review before sending, technical delivery vs. human confirmation vs. legal service assessment as separate records; real e-mail disabled — local mailbox only |
| C10 | Hearing outcome (held/not held), attendance, result, follow-up tasks with their own state |
| C11 | Decisions: draft → finalised → superseded; a finalised version can only be corrected by a linked amendment, never replaced |
| C12 | Copy packages per recipient with exact document versions, mandatory preview, recorded transfer |
| C13 | Closure requires a basis and resolution of open items; reopen with reason; links to related cases without merging history |
| C14 | Search and reports (open/closed cases, open tasks, upcoming hearings, workload) filtered to what the user may see; CSV export and print |
| C15 | Append-only, hash-chained audit history; viewing restricted documents and judicial notes is itself audited |
| C16 | Legacy import: preview, conflict report, original registration dates preserved, idempotent commit |
| C17 | Case export: zip of only permitted materials + chronology; encrypted full backup is a separate CLI tool |
| C18 | Self-operated: users, rooms, registries, reference lists, templates and settings are managed in Settings; install/upgrade/backup/restore documented in `docs/OPERATIONS.md` |

## Screenshots and video

- [Main scenario walkthrough (video)](docs/demo/main-scenario.mp4) — intake →
  registration → judge assignment → adjourned hearing → outcome → decision →
  copies → closure.
- Screen-by-screen walkthrough images: [`docs/demo/`](docs/demo) (`*.png`).

## Quick start

### Local (Rust + Node)

Requires a Rust toolchain (edition 2024) and Node 22.

```sh
cd web && npm ci && npm run build && cd ..   # build the SPA into web/dist
cargo run                                   # serves http://127.0.0.1:8088 (demo mode)
```

For frontend development run `npm run dev` inside `web/` — Vite serves on :5173
and proxies `/api` to the backend.

### Docker Compose

```sh
cp .env.example .env
docker compose up -d                        # http://127.0.0.1:8088
```

Data lives in the `court-data` volume and survives restarts. Put a TLS reverse
proxy in front for anything beyond localhost (see `deploy/nginx-tuvalu.conf`).

## Modes

| Mode | Behaviour |
|---|---|
| `demo` (default) | Each visitor gets an isolated sandbox (own SQLite + files) seeded with fictional data; persona switcher enabled; sandboxes expire after `TCR_SANDBOX_TTL_HOURS` |
| `production` | One database, password + TOTP sign-in, no persona switch, no expiry |

## Configuration

All configuration is via environment variables (see `.env.example`).

| Variable | Default | Meaning |
|---|---|---|
| `TCR_MODE` | `demo` | `demo` or `production` |
| `TCR_DATA_DIR` | `./data` | Database, files and sandboxes directory |
| `TCR_BIND` | `127.0.0.1:8088` | Listen address |
| `TCR_COOKIE_SECURE` | `false` | Set `true` when served over HTTPS |
| `TCR_SESSION_HOURS` | `12` | Session lifetime |
| `TCR_UPLOAD_MAX_MB` | `15` | Maximum uploaded file size |
| `TCR_SANDBOX_TTL_HOURS` | `72` | Demo sandbox idle lifetime |
| `TCR_MAX_SANDBOXES` | `200` | Maximum live demo sandboxes |
| `TCR_SANDBOX_QUOTA_MB` | `25` | Storage quota per demo sandbox |
| `RUST_LOG` | `info` | Log level (`tracing` env filter) |

## Demo personas

Persona switching exists **only in demo mode**. Real installations create
accounts with `tuvalu-court create-user` and assign permissions individually.

| Persona | Role | Can do |
|---|---|---|
| Olga Marsh | Registry clerk | Intake, register cases, schedule hearings, tasks, documents, dispatch, close cases, reports, case export |
| Elena Brooks | Head of registry | Assign staff and the judge, view all non-restricted cases, grant restricted-document access, override hearing conflicts, reopen cases, import, audit view |
| Viktor Hale | Judge | Sees assigned cases; records hearing outcomes; drafts/finalises decisions; private judicial notes; records service assessment |
| Sergei Novak | Hearings & service officer | Schedules hearings, manages dispatches and tasks; no restricted files or judicial notes |
| Pavel Stone | Technical administrator | Manages users and settings; **no** access to cases, files or judicial notes |

## Project layout

```
src/            Rust backend (axum + rusqlite/SQLite)
src/api/        HTTP route modules (one per domain)
src/migrations/ Ordered SQL migrations (applied on start)
src/seed.rs     Reference lists, templates, personas
src/seed_demo.rs Fictional DEMO dataset
web/            React + TypeScript SPA (Vite), embedded into the binary
tests/          Integration tests over the real HTTP router
deploy/         systemd unit + nginx site example
config/deploy.yml  Kamal deploy config (used for the public demo)
docs/           Architecture contract, spec, demo media
docs/OPERATIONS.md  Install / upgrade / backup / restore runbook
```

## Tests

```sh
cargo test          # unit + integration tests; temporary data dirs, no external services
cd web && npm run typecheck
```

See `ACCEPTANCE.md` for the requirement-by-requirement checklist and the tests
that cover each item.

## Resource footprint

Designed for a small server: a single static binary, embedded frontend, one
SQLite database, a private file directory, no external services or database
server. The shipped systemd unit caps the service at 200 MB RAM / 50% CPU, and
docker-compose at 256 MB / 0.5 CPU — the application is designed to stay well
under those limits.

## No timers, no lock-in

The code contains no activation timer, no licence check, no hidden accounts and
no obligation of paid support. Whoever receives this repository can run, modify
and extend it independently (spec §12).

## Licence

MIT — see `LICENSE`. Third-party components are listed in `THIRD_PARTY.md`.

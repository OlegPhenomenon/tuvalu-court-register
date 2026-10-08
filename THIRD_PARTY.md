# Third-party components

Direct dependencies of the Tuvalu Court Register, with the versions pinned in
`Cargo.lock` / `web/package-lock.json` and their declared licences. Transitive
dependencies are resolved by the lock files.

## Rust (Cargo.toml)

| Crate | Version | Licence |
|---|---|---|
| axum | 0.8.9 | MIT |
| tokio | 1.53.2 | MIT |
| tower | 0.5.3 | MIT |
| rusqlite | 0.40.2 | MIT |
| serde | 1.0.229 | MIT OR Apache-2.0 |
| serde_json | 1.0.151 | MIT OR Apache-2.0 |
| time | 0.3.55 | MIT OR Apache-2.0 |
| argon2 | 0.6.0 | MIT OR Apache-2.0 |
| sha2 | 0.11.0 | MIT OR Apache-2.0 |
| sha1 | 0.11.0 | MIT OR Apache-2.0 |
| hmac | 0.13.0 | MIT OR Apache-2.0 |
| getrandom | 0.4.3 | MIT OR Apache-2.0 |
| hex | 0.4.3 | MIT OR Apache-2.0 |
| rust-embed | 8.13.0 | MIT |
| zip | 8.6.0 | MIT |
| chacha20poly1305 | 0.11.0 | Apache-2.0 OR MIT |
| csv | 1.4.0 | Unlicense/MIT |
| parking_lot | 0.12.5 | MIT OR Apache-2.0 |
| tracing | 0.1.44 | MIT |
| tracing-subscriber | 0.3.23 | MIT |
| http-body-util (dev, tests) | 0.1.5 | MIT |

## Web frontend (web/package.json)

| Package | Version | Licence |
|---|---|---|
| react | 19.3.0 | MIT |
| react-dom | 19.3.0 | MIT |
| react-router-dom | 7.18.4 | MIT |
| @types/react (dev) | 19.3.0 | MIT |
| @types/react-dom (dev) | 19.3.0 | MIT |
| @vitejs/plugin-react (dev) | 6.1.2 | MIT |
| typescript (dev) | 5.9.3 | Apache-2.0 |
| vite (dev) | 8.3.3 | MIT |

## Base images (Dockerfile)

| Image | Purpose |
|---|---|
| `node:22.23.3-alpine` | frontend build stage only |
| `rust:1.98.1-alpine` | backend build stage only |
| `alpine:3.22` | runtime image (binary + SQLite data volume) |

No code was copied from other projects. Open-source examples consulted during
design (Mayan EDMS, OpenProject) are listed in `docs/spec/SPEC_RU.txt` §16.

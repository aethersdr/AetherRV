# Third-party licences

AetherRV is GPL-3.0. Everything below is linked into the binaries or shipped in the UI bundle, and every
licence is GPL-3.0-compatible (permissive). Regenerate with `harness/crates.sh`; dev-only tools
(test frameworks, the bundler) are not listed.

## Rust: 310 crates

| Licence | Crates |
|---|---|
| MIT OR Apache-2.0 | 158 |
| MIT | 58 |
| Apache-2.0 OR MIT | 21 |
| Unicode-3.0 | 18 |
| MIT/Apache-2.0 | 6 |
| Apache-2.0/MIT | 5 |
| Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT | 5 |
| Apache-2.0 | 5 |
| Apache-2.0 OR ISC OR MIT | 4 |
| Unlicense OR MIT | 3 |
| ISC | 3 |
| Zlib | 2 |
| MIT OR Zlib OR Apache-2.0 | 2 |
| MIT OR Apache-2.0 OR LGPL-2.1-or-later | 2 |
| Unlicense/MIT | 2 |
| BSD-2-Clause OR Apache-2.0 OR MIT | 2 |
| 0BSD OR MIT OR Apache-2.0 | 1 |
| ISC AND (Apache-2.0 OR ISC) | 1 |
| ISC AND (Apache-2.0 OR ISC) AND Apache-2.0 AND MIT AND BSD-3-Clause AND (Apache-2.0 OR ISC OR MIT) AND (Apache-2.0 OR ISC OR MIT-0) | 1 |
| BSD-2-Clause | 1 |
| CC0-1.0 OR MIT-0 OR Apache-2.0 | 1 |
| Apache-2.0 / MIT | 1 |
| MIT OR Apache-2.0 OR Zlib | 1 |
| MIT AND BSD-3-Clause | 1 |
| Apache-2.0 AND ISC | 1 |
| Apache-2.0 OR BSL-1.0 | 1 |
| BSD-3-Clause | 1 |
| Zlib OR Apache-2.0 OR MIT | 1 |
| (MIT OR Apache-2.0) AND Unicode-3.0 | 1 |
| CDLA-Permissive-2.0 | 1 |

Not on the permissive list: none.

### Direct dependencies

| Crate | Version | Licence |
|---|---|---|
| anyhow | 1.0.105 | MIT OR Apache-2.0 |
| axum | 0.8.9 | MIT |
| bluer | 0.17.4 | BSD-2-Clause |
| bytes | 1.12.2 | MIT |
| futures | 0.3.34 | MIT OR Apache-2.0 |
| http-body-util | 0.1.5 | MIT |
| hyper | 1.13.0 | MIT |
| hyper-util | 0.1.21 | MIT |
| libc | 0.2.191 | MIT OR Apache-2.0 |
| pbkdf2 | 0.13.0 | MIT OR Apache-2.0 |
| png | 0.18.1 | MIT OR Apache-2.0 |
| prost | 0.14.6 | Apache-2.0 |
| prost-reflect | 0.16.5 | MIT OR Apache-2.0 |
| prost-types | 0.14.6 | Apache-2.0 |
| reqwest | 0.13.5 | MIT OR Apache-2.0 |
| rumqttc | 0.25.1 | Apache-2.0 |
| rusqlite | 0.43.0 | MIT |
| sd-notify | 0.5.0 | MIT OR Apache-2.0 |
| serde | 1.0.229 | MIT OR Apache-2.0 |
| serde_json | 1.0.154 | MIT OR Apache-2.0 |
| sha1 | 0.10.7 | MIT OR Apache-2.0 |
| sha1 | 0.11.0 | MIT OR Apache-2.0 |
| socket2 | 0.6.5 | MIT OR Apache-2.0 |
| tempfile | 3.27.0 | MIT OR Apache-2.0 |
| thiserror | 2.0.21 | MIT OR Apache-2.0 |
| tokio | 1.54.1 | MIT |
| tokio-rustls | 0.26.6 | MIT OR Apache-2.0 |
| tokio-tungstenite | 0.29.0 | MIT |
| toml | 1.2.2+spec-1.1.0 | MIT OR Apache-2.0 |
| tonic | 0.14.9 | MIT |
| tonic-prost | 0.14.9 | MIT |
| tonic-reflection | 0.14.9 | MIT |
| tower | 0.5.3 | MIT |
| tracing | 0.1.44 | MIT |
| tracing-journald | 0.3.2 | MIT |
| tracing-subscriber | 0.3.23 | MIT |
| uuid | 1.29.3 | Apache-2.0 OR MIT |

## UI (npm, shipped in the bundle)

| Package | Version | Licence |
|---|---|---|
| leaflet | 1.9.4 | BSD-2-Clause |
| qrcode-generator | 1.4.4 | MIT |
| react | 19.3.1 | MIT |
| react-dom | 19.3.1 | MIT |
| three | 0.170.0 | MIT |

## Not vendored

Run-time system packages from Arch Linux ARM (Mosquitto EPL-2.0/EDL-1.0, the UI runtime, BlueZ GPL-2.0) are
separate programs the system talks to, not linked or redistributed by this repository.

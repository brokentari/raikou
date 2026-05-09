# Raikou

Raikou is a Redis-compatible server written in Rust. It speaks RESP2 and RESP3.

## Load

Raikou targets one node, hundreds of concurrent clients, and data that fits in memory.

## Check

Run `scripts/check.sh` before each commit. It needs a nightly `rustfmt` and, from the compatibility
suite on, Docker.

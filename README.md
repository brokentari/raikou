# Raikou

Raikou is a Redis-compatible server written in Rust. It speaks RESP2 and, in a reduced form, RESP3.

![Raikou serving redis-cli commands](https://raw.githubusercontent.com/brokentari/raikou/main/docs/demo.gif)

## Load

Raikou targets one node, hundreds of concurrent clients, and data that fits in memory.

## Run

```sh
cargo run --release -- --bind 127.0.0.1 --port 6379
```

On start it prints its version, process id, address, and the number of databases and commands.
SIGINT or SIGTERM stops it: the first signal stops accepting clients and exits with code 0, and a
second signal exits at once.

Raikou has no passwords: `AUTH` accepts any password for the `default` user. Keep it on a loopback
address, which is the default. Raikou prints a warning when you bind anything else.

## Embed

Add `raikou` to a Rust project to start a server from your code. The server needs a Tokio runtime.

```rust
# #[tokio::main(flavor = "current_thread")]
# async fn main() -> std::io::Result<()> {
// Port 0 asks the system for a free port.
let listener = raikou::server::bind("127.0.0.1", 0).await?;
println!("listening on {}", listener.local_addr()?);
let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
let server = tokio::spawn(raikou::server::serve_until(listener, async {
    let _ = stopped.await;
}));
// Connect any Redis client to the address, then stop the server.
stop.send(()).ok();
server.await??;
# Ok(())
# }
```

`serve` runs until the process ends. `serve_until` also stops when the future you pass completes.

## Docker

Images are published to GitHub Container Registry: `latest` and a `sha-…` tag for each push to `main`,
and `0.1.0`-style tags for releases.

```sh
docker run --rm -p 127.0.0.1:6379:6379 ghcr.io/brokentari/raikou:latest
```

Publish the port on loopback only, as above, because Raikou has no passwords.

## Supported commands

- Connection: `PING` `ECHO` `HELLO` `AUTH` `SELECT` `QUIT` `RESET` `CLIENT ID|GETNAME|SETNAME|SETINFO|INFO`
- Server: `INFO` `COMMAND` `COMMAND COUNT|DOCS` `CONFIG GET` `DBSIZE` `FLUSHDB` `FLUSHALL`
- Keys: `DEL` `UNLINK` `EXISTS` `TYPE` `TOUCH` `RENAME` `RENAMENX` `KEYS` `SCAN` `RANDOMKEY`
- Expiry: `EXPIRE` `PEXPIRE` `EXPIREAT` `PEXPIREAT` `TTL` `PTTL` `PERSIST` `EXPIRETIME` `PEXPIRETIME`
- Strings: `GET` `SET` (`NX` `XX` `GET` `EX` `PX` `EXAT` `PXAT` `KEEPTTL` `IFEQ` `IFNE`) `SETNX` `SETEX` `PSETEX`
  `GETSET` `GETDEL` `GETEX` `MGET` `MSET` `MSETNX` `APPEND` `STRLEN` `GETRANGE` `SETRANGE` `INCR` `DECR`
  `INCRBY` `DECRBY` `INCRBYFLOAT`
- Lists: `LPUSH` `RPUSH` `LPUSHX` `RPUSHX` `LPOP` `RPOP` `LLEN` `LRANGE` `LINDEX` `LSET` `LREM` `LTRIM`
  `LINSERT` `LMOVE` `RPOPLPUSH`
- Hashes: `HSET` `HGET` `HMSET` `HMGET` `HGETALL` `HDEL` `HEXISTS` `HLEN` `HKEYS` `HVALS` `HSETNX`
  `HSTRLEN` `HINCRBY` `HINCRBYFLOAT`
- Sets: `SADD` `SREM` `SMEMBERS` `SISMEMBER` `SMISMEMBER` `SCARD` `SPOP` `SRANDMEMBER` `SMOVE` `SUNION`
  `SINTER` `SDIFF` `SUNIONSTORE` `SINTERSTORE` `SDIFFSTORE`

Sorted sets, transactions, pub/sub and clean shutdown are not built yet. See the open issues.

## Known divergences

- `INFO`, `COMMAND`, `CLIENT INFO` and the `HELLO` fields `version`, `id` and `modules` differ from Redis.
- `SET ... IFDEQ|IFDNE` is a syntax error; it needs the XXH3 digest.
- Redis 8.10.3 stays silent when a bad argument header arrives in a later read than its `*N` header,
  and it never limits an oversized bulk-count line. Raikou reports both as protocol errors.
- `SCAN` cursors are hash positions, not Redis cursors. A full scan returns the same keys.
- `INCRBYFLOAT` and `HINCRBYFLOAT` emulate the x87 `long double` of Redis. Values beyond 38 digits or
  `1e38` use double precision, and subnormal extended values are not modeled.
- Hash, set and `KEYS` replies come in hash order. Redis returns small ones in insertion order.

## Performance

`cargo bench --bench perf` starts Raikou and drives it with pipelined clients over TCP. It prints
operations per second and batch latency (p50 and p99) for `PING`, `SET`, `GET`, `INCR`, `LPUSH`,
`LRANGE`, `HSET`, `SADD` and `MSET`. It also opens 500 connections at once and moves 1 MiB values.

| Variable | Default | Meaning |
|---|---|---|
| `RAIKOU_PERF_OPS` | 200000 | operations per workload |
| `RAIKOU_PERF_CLIENTS` | 16 | client threads |
| `RAIKOU_PERF_PIPELINE` | 16 | requests per batch |
| `RAIKOU_PERF_MIN_OPS` | 0 | fail if any workload is slower (CI sets 20000) |

## Check

`scripts/check.sh` runs `rustfmt` (nightly), `clippy`, a comment check, the tests and the docs.

The `compat` test needs Docker. It starts `redis:8.10.3` as the oracle and compares replies from
`redis-cli` (cases in `tests/cases/cli`) and raw bytes (cases in `tests/cases/wire`) on both servers.
`RAIKOU_COMPAT_SELF=1` compares the oracle with itself, and `RAIKOU_COMPAT_FILTER=name` runs matching
case files only.

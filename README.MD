# profiling-lab

A small three-stage pipeline (producers → workers → stats) for learning to profile.
Producers create fake transactions, workers hash each account with blake3, and the
stats thread prints throughput and latency once per second.

```bash
cargo run --release -- --seconds 10
```

Profile it with samply (opens the Firefox Profiler in your browser):

```bash
cargo build --release
samply record ./target/release/profiling-lab --seconds 10
```

Options: `--seconds`, `--producers`, `--workers`, `--accounts`, `--account-size`,
`--rate` (tx/s per producer, `0` = unlimited), `--queue` (channel capacity).
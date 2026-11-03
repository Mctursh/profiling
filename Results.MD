# Results log

- **Machine:** Apple Silicon Mac, macOS 15.5
- **Build:** `cargo build --release` with `debug = true`, blake3 1.8.7
- **Defaults:** producers 2, workers 2, accounts 8, account size 256, 10s
- **GB/s** = tx/s × 8 × (32 + account size)

## Runs

| #    | Date  | Change                                   | tx/s    | GB/s | p50     | p99     |
|------|-------|------------------------------------------|---------|------|---------|---------|
| 001  | 10-29 | baseline: rate 20k, queue 10k            | 39,994  | 0.09 | 0.50ms  | 1.3ms   |
| 002  | 10-30 | `--rate 0`                               | 548,415 | 1.26 | 18.0ms  | 21.7ms  |
| 003  | 10-30 | `--queue 1000`                           | 562,901 | 1.30 | 1.75ms  | 2.4ms   |
| 004  | 10-30 | 003, profiled                            | 546,502 | 1.26 | 1.81ms  | 2.4ms   |
| 005  | 11-01 | `--account-size 16384` (NEON default)    | 24,034  | 3.16 | 41.2ms  | 61.3ms  |
| 006  | 11-01 | 005 + blake3 `no_neon`                   | 16,682  | 2.19 | 60.2ms  | 62.8ms  |
| 007  | 11-03 | one data buffer per tx                   | 583,000 | 1.34 | 1.70ms  | 2.05ms  |
| 008  | 11-03 | 007 + hashes as iterator                 | 599,000 | 1.38 | 1.66ms  | 2.04ms  |
| 009a | 11-03 | `--batch 1 --queue 1000`                 | 589,000 | 1.36 | 1.68ms  | 2.12ms  |
| 009b | 11-03 | `--batch 8 --queue 125`                  | 783,000 | 1.80 | 1.30ms  | 1.49ms  |
| 009c | 11-03 | `--batch 32 --queue 31`                  | 832,000 | 1.92 | 1.28ms  | 1.63ms  |
| 009d | 11-03 | `--batch 128 --queue 8`                  | 940,000 | 2.17 | 1.48ms  | 1.86ms  |
| 010  | 11-03 | 009d, profiled                           | 897,000 | 2.07 | —       | —       |

Runs 003 onward use `--rate 0`. 007 onward are averages of 3 runs unless profiled.

## Takeaways

- **001:** throughput just matches the configured rate; latency comes from producer bursts.
- **002:** queue always full, so p50 ≈ 10,000 / 548k (Little's law).
- **003:** 10× smaller queue, same throughput, 10× less waiting.
- **004:** worker: hashing 72%, channel 15%, freeing memory 12%. Hash is `portable` because 288B is one chunk.
- **005:** hashing 96% of worker; NEON `hash_many` in use.
- **006:** NEON ≈ 1.44× faster, only for large accounts.
- **007:** 9 → 2 allocations; freeing 12% → 4.9%, +3.5% (predicted up to 1.12×). Refactor added a `Vec<Hash>` alloc.
- **008:** freeing 2.9%; +6% vs 003. Channel receive (17%) is now the biggest overhead.
- **009a:** same as 008, so the batching refactor costs nothing.
- **009b:** 1.33×; best p99.
- **009c:** 1.41×; lowest p50, p99 starts rising.
- **009d:** 1.60×, beats the 1.2× Amdahl ceiling; batch-fill delay now raises latency.
- **010:** workers 95% hashing, channel 0.4%. Producers ~97% blocked in `send` (queue full): spare capacity, workers are the bottleneck.
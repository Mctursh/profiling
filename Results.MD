# Results log

Apple Silicon Mac, macOS 15.5, release + debug symbols, blake3 1.8.7. Defaults: producers 2, workers 2, accounts 8, 10s.

| # | Date | Change | tx/s | GB/s | p50 | p99 | Takeaway |
|---|---|---|---|---|---|---|---|
| 001 | 10-29 | baseline (rate 20k, queue 10k, 256B) | 39,994 | 0.09 | 0.50ms | 1.3ms | Throughput just matches the configured rate; latency comes from producer bursts. |
| 002 | 10-30 | `--rate 0` | 548,415 | 1.26 | 18.0ms | 21.7ms | Queue is always full, so p50 ≈ 10k / 548k (Little's law). |
| 003 | 10-30 | `--queue 1000` | 562,901 | 1.30 | 1.75ms | 2.4ms | 10× smaller queue: same throughput, 10× less waiting. |
| 004 | 10-30 | profiled 003 | 546,502 | 1.26 | 1.81ms | 2.4ms | Worker: hashing 72%, channel 15%, freeing memory 12%; portable hash because 288B is one chunk. |
| 005 | 11-01 | `--account-size 16384` (NEON on by default) | 24,034 | 3.16 | 41.2ms | 61.3ms | Hashing 96% of worker; NEON `hash_many` in use. |
| 006 | 11-01 | 005 + blake3 `no_neon` | 16,682 | 2.19 | 60.2ms | 62.8ms | NEON ≈ 1.44× faster, only for large accounts. |

GB/s = tx/s × 8 × (32 + account size). Next: 007, allocation lesson, baseline 004.
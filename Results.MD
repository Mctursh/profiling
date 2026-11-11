# Results log

## Summary

A three-stage pipeline (producers → workers → stats) profiled from 40k to 3.6M tx/s, then used to measure how the OS and CPU layout shape throughput and tail latency.

**What each phase taught**

| Phase | Runs | Lesson |
|---|---|---|
| Queueing | 001–003 | Latency ≈ items waiting ÷ throughput (Little's law). A 10× smaller queue gave the same throughput with 10× less waiting. |
| Hot path | 004–008 | Profile first. Fewer allocations helped a little; SIMD only helps large inputs, and the gain is capped by the share of time it touches (Amdahl's law). |
| Overheads | 009 | Batching spreads the channel's fixed cost over many items (1.6×), at the price of batch-fill latency. |
| Scaling | 011–019 | Find the bottleneck by asking who waits on whom. Gains flatten past the number of fast cores. At 10 workers, freeing memory waited on the allocator's lock; mimalloc fixed it (1.24–1.29×). |
| Jitter on macOS | 020–025 | Rule out one suspect at a time: stats thread, samply, allocator purging. The spikes were macOS scheduling. Rebuild before measuring (020 used a stale binary). |
| Environments | 026–027 | A VM still inherits the host's jitter. On bare-metal Linux the spikes disappeared without tuning. |
| CPU layout | 028–037 | Pinning +16%. Producer and consumer in different L3 groups: −4% throughput, IPC −16%, 64% of data fetched from outside the group. Hyperthread twins: each core did 1.71× more work, but each thread was slower. Isolation removes the tail spikes, not the average. The performance governor removes the slow start. CPU 0's group is noisy. |

**How to measure (habits that held up)**

- Change one thing per run; run each setup 3+ times; check the ± spread before trusting a difference.
- Rerun the baseline on every new machine; compare within a session, not across machines.
- Predict before running; a result that disagrees with the prediction is the interesting one.
- Go from symptom to mechanism to proof: throughput → IPC → where the data came from (hardware counters).
- Measuring changes what you measure: samply −2.5%, `perf sched record` raised p50 ~70%.

**For real systems (validators included)**

- Pin threads; keep each producer and its consumer in the same L3 group.
- Give latency-critical threads a whole core (twin left idle); use twins for throughput work.
- Isolate cores for critical threads (`isolcpus`, `nohz_full`, `rcu_nocbs`), set the performance governor, and stay off CPU 0's group.

**Still open**

- Why the EPYC is 55% slower than the Ryzen per worker (predicted 25–35%).
- The 5s burst to 2.05M tx/s in 029 run 3.
- The exact job that runs every ~5s on CPUs 0–5.
- Why split L3 groups pulled 42% of fills from RAM rather than from the other group's cache.

## Setup

- **Machines:**
  - 001–025: Apple Silicon Mac (8 performance + 4 efficiency cores), macOS 15.5
  - 026: OrbStack VM on that Mac, Ubuntu 24.04 arm64
  - 027: AMD Ryzen 9 7900 bare metal (12 cores / 24 threads, 2 L3 groups), Ubuntu 24.04
  - 028–029: AMD EPYC 7443P bare metal (24 cores / 48 threads, 1 NUMA node), Ubuntu 24.04. CPU *n* + 24 is the hyperthread twin of CPU *n*; L3 groups are CPUs 0–5, 6–11, 12–17, 18–23
  - 030–037: another EPYC 7443P, same CPU map
- **Build:** `cargo build --release` with `debug = true`, blake3 1.8.7
- **Defaults:** producers 2, workers 2, accounts 8, account size 256, 10s
- **GB/s** = tx/s × 8 × (32 + account size)

Runs 003 onward use `--rate 0`. 007 onward are averages of 3 runs unless marked otherwise.

## Runs

| # | Date | Change | tx/s | GB/s | p50 | p99 |
|---|---|---|---|---|---|---|
| 001 | 10-29 | baseline: rate 20k, queue 10k | 39,994 | 0.09 | 0.50ms | 1.3ms |
| 002 | 10-30 | `--rate 0` | 548,415 | 1.26 | 18.0ms | 21.7ms |
| 003 | 10-30 | `--queue 1000` | 562,901 | 1.30 | 1.75ms | 2.4ms |
| 004 | 10-30 | 003, profiled | 546,502 | 1.26 | 1.81ms | 2.4ms |
| 005 | 11-01 | `--account-size 16384` (NEON default) | 24,034 | 3.16 | 41.2ms | 61.3ms |
| 006 | 11-01 | 005 + blake3 `no_neon` | 16,682 | 2.19 | 60.2ms | 62.8ms |
| 007 | 11-03 | one data buffer per tx | 583,000 | 1.34 | 1.70ms | 2.05ms |
| 008 | 11-03 | 007 + hashes as iterator | 599,000 | 1.38 | 1.66ms | 2.04ms |
| 009a | 11-03 | `--batch 1 --queue 1000` | 589,000 | 1.36 | 1.68ms | 2.12ms |
| 009b | 11-03 | `--batch 8 --queue 125` | 783,000 | 1.80 | 1.30ms | 1.49ms |
| 009c | 11-03 | `--batch 32 --queue 31` | 832,000 | 1.92 | 1.28ms | 1.63ms |
| 009d | 11-03 | `--batch 128 --queue 8` | 940,000 | 2.17 | 1.48ms | 1.86ms |
| 010 | 11-03 | 009d, profiled | 897,000 | 2.07 | — | — |
| 011 | 11-04 | 009d + `--producers 4` | 943,000 | 2.17 | 1.75ms | 2.34ms |
| 012 | 11-04 | 009d + `--workers 4` | 1,629,000 | 3.75 | 0.92ms | 1.20ms |
| 013 | 11-04 | 009d + `--workers 6` | 2,158,000 | 4.97 | 0.74ms | 1.01ms |
| 014 | 11-04 | 009d + `--workers 8` | 2,535,000 | 5.84 | 0.57ms | 0.92ms |
| 015 | 11-04 | 009d + `--workers 10` | 2,661,000 | 6.13 | 0.58ms | 1.18ms |
| 016 | 11-05 | 014 + mimalloc (`--workers 8`) | 3,142,000 | 7.24 | 0.45ms | 0.74ms |
| 017 | 11-05 | 015 + mimalloc (`--workers 10`) | 3,445,000 | 7.94 | 0.41ms | 0.92ms |
| 018 | 11-05 | 016 + `--producers 3` | 3,210,000 | 7.40 | 0.54ms | 0.83ms |
| 019 | 11-05 | 016 + `--producers 4` | 3,215,000 | 7.41 | 0.57ms | 1.13ms |
| 020 | 11-05 | 016 profiled (stale binary, no histogram) | 3,086,000 | 7.11 | 0.51ms | 0.86ms |
| 021 | 11-05 | 020, not profiled (5 runs) | 3,164,000 | 7.29 | 0.46ms | 0.73ms |
| 022 | 11-05 | 021 + `MIMALLOC_PURGE_DELAY=-1` (5 runs) | 3,133,000 | 7.22 | 0.45ms | 0.75ms |
| 023 | 11-05 | 021 + `--workers 4` (5 runs) | 1,820,000 | 4.19 | 0.83ms | 1.05ms |
| 024 | 11-05 | 023 + workers `QOS_CLASS_BACKGROUND` (4 runs) | 377,000 | 0.87 | 3.93ms | 7.85ms |
| 025 | 11-05 | 021 + HdrHistogram stats, rebuilt (5 runs) | 3,145,000 | 7.25 | 0.44ms | 0.72ms |
| 026 | 11-05 | 025 on OrbStack VM (Ubuntu arm64) | 2,956,000 | 6.81 | 0.42ms | 0.75ms |
| 027 | 11-06 | 025 on bare metal (Ryzen 9 7900) | 3,608,000 | 8.31 | 0.23ms | 0.51ms |
| 028 | 11-11 | 027 on EPYC 7443P, unpinned | 1,624,000 | 3.74 | 0.34ms | 0.53ms |
| 029 | 11-11 | 028 + `--worker-cpus 0-7 --producer-cpus 8-9` | 1,890,000 | 4.36 | 0.32ms | 0.50ms |
| 030 | 11-11 | D1: `--workers 4 --worker-cpus 0-3 --producer-cpus 4-5` (same L3) | 1,092,000 | 2.52 | 1.37ms | 1.77ms |
| 031 | 11-11 | D2: 030 but `--producer-cpus 6-7` (other L3) | 1,047,000 | 2.41 | 1.40ms | 1.69ms |
| 032 | 11-11 | C: 030 but `--worker-cpus 0-1,24-25` (4 workers on 2 cores) | 934,000 | 2.15 | 1.61ms | 2.09ms |
| 033 | 11-11 | E1: 030 after reboot with `isolcpus=12-17,36-41` (0–5 not isolated) | 1,093,000 | 2.52 | 1.37ms | 1.77ms |
| 034 | 11-11 | E2: same on isolated CPUs (`--worker-cpus 12-15 --producer-cpus 16-17`) | 1,004,000 | 2.31 | 1.45ms | 2.37ms |
| 035 | 11-11 | 034 + performance governor | 1,102,000 | 2.54 | 1.36ms | 1.77ms |
| 036 | 11-11 | 033 + performance governor | 1,097,000 | 2.53 | 1.37ms | 1.77ms |
| 037 | 11-11 | 036 layout on L3 group 3 (`--worker-cpus 18-21 --producer-cpus 22-23`), 30s | 1,097,000 | 2.53 | 1.37ms | 1.78ms |

## Takeaways

### Queueing and hot path

- **001:** throughput just matches the configured rate; latency comes from producer bursts.
- **002:** queue always full, so p50 ≈ 10,000 / 548k (Little's law).
- **003:** 10× smaller queue, same throughput, 10× less waiting.
- **004:** worker: hashing 72%, channel 15%, freeing memory 12%. Hash is `portable` because 288B is one chunk.
- **005:** hashing 96% of worker; NEON `hash_many` in use.
- **006:** NEON ≈ 1.44× faster, only for large accounts.
- **007:** 9 → 2 allocations; freeing 12% → 4.9%, +3.5% (predicted up to 1.12×). Refactor added a `Vec<Hash>` alloc.
- **008:** freeing 2.9%; +6% vs 003. Channel receive (17%) is now the biggest overhead.

### Batching and scaling

- **009a:** same as 008, so the batching refactor costs nothing.
- **009b:** 1.33×; best p99.
- **009c:** 1.41×; lowest p50, p99 starts rising.
- **009d:** 1.60×, beats the 1.2× Amdahl ceiling; batch-fill delay now raises latency.
- **010:** workers 95% hashing, channel 0.4%. Producers ~97% blocked in `send` (queue full): workers are the bottleneck.
- **011:** more producers: same throughput, as the profile predicted, but p50 +0.27ms (each producer fills its batch half as fast).
- **012:** 2× workers → 1.73× throughput; workers fully busy, producers still ~91% blocked in `send`.
- **013–015:** per-worker throughput falls 470k → 266k; nearly flat past 8 workers (= performance cores). At 10, p99 rises and max hits 10–21ms.
- **015 profiled:** workers 27–35% freeing memory (was 3.4% at 2 workers), mostly waiting on the allocator's lock (`_os_unfair_lock_lock_slow`).
- **016:** mimalloc: 1.24× at 8 workers; freeing back to 3.5%.
- **017:** mimalloc: 1.29× at 10 workers, but p99 +24%; producers close to becoming the bottleneck.
- **018–019:** more producers: +2% (noise), p50 worse, p99 much worse at 4 (13 threads, 12 cores). Best: 2 producers / 8 workers.

### Jitter on macOS

- **020:** invalid as a histogram test: binary not rebuilt. Dip samples: worker in `mi_free_block_mt` (cross-thread free), producer parked in `send`.
- **021:** dips persist without samply (max 7–11ms), so samply is ruled out; it costs ~2.5% throughput and ~10% p50.
- **022:** no purging: max still 6.5–10.7ms, so allocator purging is ruled out.
- **023:** spare cores: run max 4–7ms (was 6.5–11ms), wobble ±2% (was ±4%). Core competition explains about half; the rest is OS jitter macOS won't let you isolate.
- **024:** background QoS: 4.8× slower (~94k tx/s per worker vs ~455k), one 134ms stall: E-cores, low priority, throttled.
- **025:** real histogram test: same numbers as 021, max still 7.9–12.4ms, so the stats thread is ruled out.

### Environments

- **026:** VM: −6% throughput, max 4.5–5.6ms (macOS 7.9–12.4ms). Guest CPUs are still host threads, so host jitter remains.
- **027:** bare metal: +15% throughput, half the p50, max 0.42–1.05ms after the first second. The macOS spikes were the OS, not the code.
- **027 perf sched:** recording raised p50 to 0.40ms. Longest wait for a CPU: workers/producers 1.0–1.34ms (matches max); `kworker/N-mm` up to 3ms, starved on our CPUs' twins.

### CPU layout (EPYC)

- **028:** baseline 55% below the Ryzen (predicted 25–35%; cause not checked). 350 migrations, 8.55 CPUs used.
- **029:** pinning: +16%, per-second max 0.55–0.67ms (was 0.8–1.1ms), migrations 350 → 17 (only unpinned main and stats).
- **028 rerun with `ps`:** no two busy threads ever on twin CPUs (the scheduler avoids it), but spread over all 4 L3 groups; pinned, 2 groups.
- **030–031:** producers in another L3 group: −4.1%, and 5.08 CPUs used vs 4.48 for less work.
- **030–031 counters:** instructions per tx identical (~24.2k), cycles per tx +19%, IPC 2.13 → 1.78. `cache-misses` count flat: misses not more frequent, each one slower.
- **030–031 fill sources** (`ls_dmnd_fills_from_sys.*`): D1: own L2 24%, same group 76%, other group 0.05%, RAM 0.2%. D2: own L2 22%, same group 14%, other group 22%, RAM 42%.
- **032:** hyperthread twins: −14.5% vs 4 full cores, IPC per thread 1.86. Each core did 1.71× more work with 2 threads (I'd predicted 0–30%): blake3's single-chunk path is a dependency chain, leaving units idle for the twin. p50 +18%, as Little's law predicts.
- **033–034:** isolated CPUs start slow (570k–970k for 2–4s), then match E1 with no 3.9–4.3ms spikes. `kworker` switches on worker CPUs: ~1,115 each in 3s (E1) vs 16–29 (E2): 40–70× fewer interruptions.
- **035:** performance governor: slow start gone, every second 1.09–1.11M, max ≤ 2.17ms. The slow start was CPU frequency ramp-up on `nohz_full` CPUs.
- **036 vs 035:** same throughput and p50/p99. Non-isolated CPUs still spike to 3.7–4.0ms about every 5s; isolated never above 2.17ms. Isolation fixes the tail, not the average.
- **Writeback test:** spikes stayed ~5s apart with writeback at 5s and at 15s, so writeback is ruled out.
- **Hunt:** no foreign thread ran >0.5ms on CPUs 0–5; interrupt counts show no 5s bursts.
- **037:** same layout on group 3 (not isolated): no spike above 2.7ms in 30s. The cause is specific to CPUs 0–5, most likely CPU 0's machine-wide duties; exact job not identified.
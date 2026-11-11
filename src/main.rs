//! profiling-lab: a tiny pipeline shaped like a slice of a validator.
//!
//!   producers ──(channel)──► workers ──(channel)──► stats reporter
//!   make "transactions"       hash each account      prints throughput
//!   with a few accounts        (blake3)              and latency each second
//!
//! This is the clean baseline. Each lesson changes one thing on purpose,
//! then we measure and profile the difference.
//!
//! Run (always profile release builds):
//!   cargo run --release -- --seconds 10
//!
//! Options (all optional):
//!   --seconds N          how long to run                    (default 10)
//!   --producers N        producer threads                   (default 2)
//!   --workers N          worker threads                     (default 2)
//!   --accounts N         accounts per transaction           (default 8)
//!   --account-size N     bytes of data per account          (default 256)
//!   --rate N             transactions/sec per producer,
//!                        0 = as fast as possible            (default 20000)
//!   --queue N            channel capacity between stages    (default 10000)

use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, bounded};
use hdrhistogram::Histogram;
// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[derive(Debug, Clone)]
struct Config {
    seconds: u64,
    producers: usize,
    workers: usize,
    accounts: usize,
    account_size: usize,
    rate: u64,
    queue: usize,
    batch: usize,
    worker_cpus: Option<Vec<usize>>,
    producer_cpus: Option<Vec<usize>>,
}

impl Config {
    fn default() -> Self {
        Config {
            seconds: 10,
            producers: 2,
            workers: 2,
            accounts: 8,
            account_size: 256,
            rate: 20_000,
            queue: 10_000,
            batch: 1,
            worker_cpus: None,
            producer_cpus: None,
        }
    }

    fn from_args() -> Self {
        let mut cfg = Config::default();

        let args: Vec<String> = std::env::args().skip(1).collect();
        let mut i = 0;
        while i < args.len() {
            let value = || -> u64 {
                args.get(i + 1)
                    .and_then(|v| v.parse().ok())
                    .unwrap_or_else(|| panic!("{} needs a number", args[i]))
            };

            let range = || -> Option<Vec<usize>> {
                Some(
                    args.get(i + 1)
                        .and_then(|v| {
                            let val = v.split("-").collect::<Vec<&str>>();
                            if val.len() != 2 {
                                panic!("{} needs to be a range eg. 0-5", args[i])
                            }
                            let val = val
                                .into_iter()
                                .map(|v| v.parse::<usize>().unwrap())
                                .collect::<Vec<usize>>();
                            let start = val[0];
                            let end = val[1];
                            assert!(start <= end, "invalid range, start is higher than end");
                            Some((start..=end).collect::<Vec<usize>>())
                        })
                        .unwrap_or_else(|| panic!("{} needs a number", args[i])),
                )
            };

            match args[i].as_str() {
                "--seconds" => cfg.seconds = value(),
                "--producers" => cfg.producers = value() as usize,
                "--workers" => cfg.workers = value() as usize,
                "--accounts" => cfg.accounts = value() as usize,
                "--account-size" => cfg.account_size = value() as usize,
                "--rate" => cfg.rate = value(),
                "--queue" => cfg.queue = value() as usize,
                "--batch" => cfg.batch = value() as usize,
                "--worker-cpus" => cfg.worker_cpus = range(),
                "--producer-cpus" => cfg.producer_cpus = range(),
                other => panic!("unknown option: {other}"),
            }
            i += 2;
        }
        cfg
    }
}

// ---------------------------------------------------------------------------
// Data
// ---------------------------------------------------------------------------

struct Account {
    key: [u8; 32],
}

struct Transaction {
    created_at: Instant,
    accounts: Vec<Account>,
    data: Vec<u8>,
}

/// What a worker reports back: how long each transaction took, from the
/// moment it was created to the moment its accounts were all hashed.
struct Report {
    latencies_us: Vec<u64>,
}

// ---------------------------------------------------------------------------
// Stage 1: producers
// ---------------------------------------------------------------------------

fn make_transaction(cfg: &Config, counter: u64) -> Transaction {
    let mut txs_data: Vec<u8> = Vec::with_capacity(cfg.account_size * cfg.accounts);
    let accounts = (0..cfg.accounts)
        .map(|i| {
            let mut key = [0u8; 32];
            key[..8].copy_from_slice(&counter.to_le_bytes());
            key[8..16].copy_from_slice(&(i as u64).to_le_bytes());

            txs_data.resize(
                txs_data.len() + cfg.account_size,
                (counter as u8).wrapping_add(i as u8),
            );
            Account { key }
        })
        .collect();

    Transaction {
        created_at: Instant::now(),
        accounts,
        data: txs_data,
    }
}

fn producer(cfg: Config, tx: Sender<Vec<Transaction>>, stop_at: Instant) {
    // fn producer(cfg: Config, tx: Sender<Transaction>, stop_at: Instant) {
    let mut counter = 0u64;

    // Pacing: produce in small batches, then sleep until the next batch is due.
    // (Sleeping per transaction would be too coarse at high rates.)
    // let batch = 100u64;
    let batch_per_send = cfg.batch;
    let batch_interval = if cfg.rate == 0 {
        None
    } else {
        Some(Duration::from_secs_f64(
            batch_per_send as f64 / cfg.rate as f64,
        ))
    };
    let mut next_batch = Instant::now();

    while Instant::now() < stop_at {
        let mut txs: Vec<Transaction> = Vec::with_capacity(batch_per_send as usize);
        for _ in 0..batch_per_send {
            txs.push(make_transaction(&cfg, counter));

            counter += 1;
        }

        if tx.send(txs).is_err() {
            return;
        }

        if let Some(interval) = batch_interval {
            next_batch += interval;
            let now = Instant::now();
            if next_batch > now {
                thread::sleep(next_batch - now);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Stage 2: workers
// ---------------------------------------------------------------------------

fn hash_account(key: &[u8], data: &[u8]) -> blake3::Hash {
    let mut hasher = blake3::Hasher::new();
    hasher.update(key);
    hasher.update(data);
    hasher.finalize()
}

fn worker(rx: Receiver<Vec<Transaction>>, reports: Sender<Report>) {
    let mut latencies = Vec::with_capacity(4096);
    let mut last_flush = Instant::now();
    let mut checksum = 0u8; // keeps the compiler from optimizing hashing away

    for txns in rx {
        for txn in txns {
            for hash in hash_transaction(&txn) {
                checksum ^= hash.as_bytes()[0]
            }

            latencies.push(txn.created_at.elapsed().as_micros() as u64);
        }

        // Send latencies to the reporter in batches, not one by one.
        if latencies.len() >= 4096 || last_flush.elapsed() >= Duration::from_millis(100) {
            let _ = reports.send(Report {
                latencies_us: std::mem::take(&mut latencies),
            });
            last_flush = Instant::now();
        }
    }

    let _ = reports.send(Report {
        latencies_us: latencies,
    });
    std::hint::black_box(checksum);
}

fn hash_transaction(txn: &Transaction) -> impl Iterator<Item = blake3::Hash> {
    let account_size = txn.data.len() / txn.accounts.len();
    (0..txn.accounts.len()).map(move |i| {
        let start = (i * account_size) as usize;
        let stop = (start + account_size) as usize;
        let data = &txn.data[start..stop];
        hash_account(&txn.accounts[i].key, data)
    })
}

// ---------------------------------------------------------------------------
// Stage 3: stats
// ---------------------------------------------------------------------------

fn percentile(sorted: &[u64], p: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = ((p / 100.0) * (sorted.len() - 1) as f64).round() as usize;
    sorted[idx]
}

fn print_line(label: &str, latencies: &mut Vec<u64>, elapsed: Duration) {
    latencies.sort_unstable();
    let count = latencies.len();
    let per_sec = count as f64 / elapsed.as_secs_f64();
    println!(
        "{label:>6}  {per_sec:>10.0} tx/s   latency p50 {:>6}µs   p99 {:>6}µs   max {:>7}µs",
        percentile(latencies, 50.0),
        percentile(latencies, 99.0),
        latencies.last().copied().unwrap_or(0),
    );
}

fn print_hist(label: &str, h: &Histogram<u64>, elapsed: Duration) {
    let per_sec = h.len() as f64 / elapsed.as_secs_f64();
    println!(
        "{label:>6}  {per_sec:>10.0} tx/s   latency p50 {:>6}µs   p99 {:>6}µs   max {:>7}µs",
        h.value_at_quantile(0.50),
        h.value_at_quantile(0.99),
        h.max(),
    );
}

fn stats(reports: Receiver<Report>) {
    let start = Instant::now();
    let mut window = Histogram::<u64>::new_with_bounds(1, 60_000_000, 3).unwrap();
    let mut all = Histogram::<u64>::new_with_bounds(1, 60_000_000, 3).unwrap();
    let mut window_start = Instant::now();
    let mut second = 1;

    loop {
        match reports.recv_timeout(Duration::from_millis(50)) {
            Ok(report) => {
                for v in report.latencies_us {
                    window.saturating_record(v);
                }
            }
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        }

        if window_start.elapsed() >= Duration::from_secs(1) {
            print_hist(&format!("{second}s"), &window, window_start.elapsed());
            all.add(&window).unwrap();
            window.reset();
            window_start = Instant::now();
            second += 1;
        }
    }

    all.add(&window).unwrap();
    println!("{}", "-".repeat(78));
    print_hist("total", &all, start.elapsed());
}

#[cfg(target_os = "macos")]
fn set_qos(class: libc::qos_class_t) {
    let rc = unsafe { libc::pthread_set_qos_class_self_np(class, 0) };
    assert_eq!(rc, 0, "failed to set QoS");
}

fn pin(cpus: Option<Vec<usize>>, thread_index: usize) -> Option<bool> {
    if let Some(worker_cpus) = cpus {
        let cpu_list_index = thread_index % worker_cpus.len();
        Some(core_affinity::set_for_current(core_affinity::CoreId {
            id: worker_cpus[cpu_list_index],
        }))
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Wiring
// ---------------------------------------------------------------------------

fn main() {
    let cfg = Config::from_args();
    println!("{cfg:?}");

    let (txn_tx, txn_rx) = bounded::<Vec<Transaction>>(cfg.queue);
    let (report_tx, report_rx) = bounded::<Report>(1024);
    let stop_at = Instant::now() + Duration::from_secs(cfg.seconds);

    let stats_thread = thread::Builder::new()
        .name("stats".into())
        .spawn(move || stats(report_rx))
        .unwrap();

    let workers: Vec<_> = (0..cfg.workers)
        .map(|i| {
            let cfg = cfg.clone();
            let rx = txn_rx.clone();
            let reports = report_tx.clone();
            thread::Builder::new()
                .name(format!("worker-{i}"))
                .spawn(move || {
                    if let Some(v) = pin(cfg.worker_cpus.clone(), i) {
                        if v {
                            println!("Successfully pinned worker-{i} to a core")
                        } else {
                            println!("Failed to pin worker-{i} to a core")
                        }
                    }
                    // #[cfg(target_os = "macos")]
                    // set_qos(libc::qos_class_t::QOS_CLASS_BACKGROUND); // should puhs it to the efficiency cores
                    worker(rx, reports)
                })
                .unwrap()
        })
        .collect();
    drop(txn_rx);
    drop(report_tx);

    let producers: Vec<_> = (0..cfg.producers)
        .map(|i| {
            let tx = txn_tx.clone();
            let producer_cpus = cfg.producer_cpus.clone();
            let cfg = cfg.clone();
            thread::Builder::new()
                .name(format!("producer-{i}"))
                .spawn(move || {
                    if let Some(v) = pin(producer_cpus, i) {
                        if v {
                            println!("Successfully pinned producer-{i} to a core")
                        } else {
                            println!("Failed to pin producer-{i} to a core")
                        }
                    }
                    // #[cfg(target_os = "macos")]
                    // set_qos(libc::qos_class_t::QOS_CLASS_BACKGROUND); // should puhs it to the efficiency cores
                    producer(cfg, tx, stop_at)
                })
                .unwrap()
        })
        .collect();
    drop(txn_tx);

    // Shutdown order: producers stop → channel closes → workers drain and
    // exit → report channel closes → stats prints the total.
    for p in producers {
        p.join().unwrap();
    }
    for w in workers {
        w.join().unwrap();
    }
    stats_thread.join().unwrap();
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use crate::{Config, Transaction, hash_account, hash_transaction, make_transaction};

    struct Account {
        key: [u8; 32],
        data: Vec<u8>,
    }

    struct Txn {
        created_at: Instant,
        accounts: Vec<Account>,
    }

    #[test]
    fn hash_account_match() {
        let instant = Instant::now();
        let counter = 1;
        let txns = make_transaction(&Config::default(), counter);
        let tx = Transaction {
            created_at: instant,
            accounts: txns.accounts,
            data: txns.data,
        };

        let first_hashes = hash_transaction(&tx);

        let accounts = (0..Config::default().accounts)
            .map(|i| {
                let mut key = [0u8; 32];
                key[..8].copy_from_slice(&counter.to_le_bytes());
                key[8..16].copy_from_slice(&(i as u64).to_le_bytes());

                Account {
                    key,
                    data: vec![
                        (counter as u8).wrapping_add(i as u8);
                        Config::default().account_size
                    ],
                }
            })
            .collect();

        let old_tx = Txn {
            created_at: instant,
            accounts,
        };

        let mut second_hashes: Vec<blake3::Hash> = Vec::with_capacity(old_tx.accounts.len());
        for account in old_tx.accounts {
            second_hashes.push(hash_account(&account.key, &account.data));
        }

        assert_eq!(first_hashes.collect::<Vec<blake3::Hash>>(), second_hashes)
    }
}

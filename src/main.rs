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

use crossbeam_channel::{bounded, Receiver, Sender};
// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Config {
    seconds: u64,
    producers: usize,
    workers: usize,
    accounts: usize,
    account_size: usize,
    rate: u64,
    queue: usize,
}

impl Config {
    fn from_args() -> Self {
        let mut cfg = Config {
            seconds: 10,
            producers: 2,
            workers: 2,
            accounts: 8,
            account_size: 256,
            rate: 20_000,
            queue: 10_000,
        };

        let args: Vec<String> = std::env::args().skip(1).collect();
        let mut i = 0;
        while i < args.len() {
            let value = || -> u64 {
                args.get(i + 1)
                    .and_then(|v| v.parse().ok())
                    .unwrap_or_else(|| panic!("{} needs a number", args[i]))
            };
            match args[i].as_str() {
                "--seconds" => cfg.seconds = value(),
                "--producers" => cfg.producers = value() as usize,
                "--workers" => cfg.workers = value() as usize,
                "--accounts" => cfg.accounts = value() as usize,
                "--account-size" => cfg.account_size = value() as usize,
                "--rate" => cfg.rate = value(),
                "--queue" => cfg.queue = value() as usize,
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
    data: Vec<u8>,
}

struct Transaction {
    created_at: Instant,
    accounts: Vec<Account>,
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
    let accounts = (0..cfg.accounts)
        .map(|i| {
            let mut key = [0u8; 32];
            key[..8].copy_from_slice(&counter.to_le_bytes());
            key[8..16].copy_from_slice(&(i as u64).to_le_bytes());
            Account {
                key,
                data: vec![(counter as u8).wrapping_add(i as u8); cfg.account_size],
            }
        })
        .collect();

    Transaction {
        created_at: Instant::now(),
        accounts,
    }
}

fn producer(cfg: Config, tx: Sender<Transaction>, stop_at: Instant) {
    let mut counter = 0u64;

    // Pacing: produce in small batches, then sleep until the next batch is due.
    // (Sleeping per transaction would be too coarse at high rates.)
    let batch = 100u64;
    let batch_interval = if cfg.rate == 0 {
        None
    } else {
        Some(Duration::from_secs_f64(batch as f64 / cfg.rate as f64))
    };
    let mut next_batch = Instant::now();

    while Instant::now() < stop_at {
        for _ in 0..batch {
            if tx.send(make_transaction(&cfg, counter)).is_err() {
                return;
            }
            counter += 1;
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

fn hash_account(account: &Account) -> blake3::Hash {
    let mut hasher = blake3::Hasher::new();
    hasher.update(&account.key);
    hasher.update(&account.data);
    hasher.finalize()
}

fn worker(rx: Receiver<Transaction>, reports: Sender<Report>) {
    let mut latencies = Vec::with_capacity(4096);
    let mut last_flush = Instant::now();
    let mut checksum = 0u8; // keeps the compiler from optimizing hashing away

    for txn in rx.iter() {
        for account in &txn.accounts {
            checksum ^= hash_account(account).as_bytes()[0];
        }
        latencies.push(txn.created_at.elapsed().as_micros() as u64);

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

fn stats(reports: Receiver<Report>) {
    let start = Instant::now();
    let mut window = Vec::new();
    let mut all = Vec::new();
    let mut window_start = Instant::now();
    let mut second = 1;

    loop {
        match reports.recv_timeout(Duration::from_millis(50)) {
            Ok(report) => window.extend(report.latencies_us),
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        }

        if window_start.elapsed() >= Duration::from_secs(1) {
            print_line(&format!("{second}s"), &mut window, window_start.elapsed());
            all.append(&mut window);
            window_start = Instant::now();
            second += 1;
        }
    }

    all.append(&mut window);
    println!("{}", "-".repeat(78));
    print_line("total", &mut all, start.elapsed());
}

// ---------------------------------------------------------------------------
// Wiring
// ---------------------------------------------------------------------------

fn main() {
    let cfg = Config::from_args();
    println!("{cfg:?}");

    let (txn_tx, txn_rx) = bounded::<Transaction>(cfg.queue);
    let (report_tx, report_rx) = bounded::<Report>(1024);
    let stop_at = Instant::now() + Duration::from_secs(cfg.seconds);

    let stats_thread = thread::Builder::new()
        .name("stats".into())
        .spawn(move || stats(report_rx))
        .unwrap();

    let workers: Vec<_> = (0..cfg.workers)
        .map(|i| {
            let rx = txn_rx.clone();
            let reports = report_tx.clone();
            thread::Builder::new()
                .name(format!("worker-{i}"))
                .spawn(move || worker(rx, reports))
                .unwrap()
        })
        .collect();
    drop(txn_rx);
    drop(report_tx);

    let producers: Vec<_> = (0..cfg.producers)
        .map(|i| {
            let tx = txn_tx.clone();
            let cfg = cfg.clone();
            thread::Builder::new()
                .name(format!("producer-{i}"))
                .spawn(move || producer(cfg, tx, stop_at))
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
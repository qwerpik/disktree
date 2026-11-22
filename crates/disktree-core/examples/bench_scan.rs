//! Time a blocking scan: `bench_scan ROOT [THREADS]`.
use std::path::Path;
use std::time::Instant;

use disktree_core::scan::{ScanOptions, scan};
use disktree_core::scan_threads::ScanThreads;

fn main() {
    let mut args = std::env::args().skip(1);
    let root = args.next().unwrap_or_else(|| "/".into());
    let threads: usize = args.next().map_or(usize::MAX, |t| t.parse().unwrap());
    let options = ScanOptions {
        threads: ScanThreads {
            max_threads: threads,
            adaptive: false,
            system_cpu_limit: None,
            ..ScanThreads::default()
        },
        ..ScanOptions::default()
    };
    let start = Instant::now();
    let node = scan(Path::new(&root), options).expect("scan");
    let took = start.elapsed();
    println!(
        "{root} threads={threads}: {:.3}s files={} dirs={} bytes={:.1}G ({} B)",
        took.as_secs_f64(),
        node.files,
        node.dirs,
        node.bytes as f64 / 1e9,
        node.bytes
    );
}

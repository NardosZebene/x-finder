//! Benchmark helper for the "100k-file rule" (plan §1, §9).
//!
//!   cargo run --release --bin bench -- gen  <dir> [count]   create a test folder
//!   cargo run --release --bin bench -- list <dir>           time listing + sorting

use std::fs;
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::Instant;

use finder_rs::core::format::thousands;
use finder_rs::core::fs_list::{self, Cancel, ListEvent};
use finder_rs::core::sort::{self, SortKey};
use finder_rs::core::sys;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [cmd, dir] if cmd == "list" => list(PathBuf::from(dir)),
        [cmd, dir] if cmd == "gen" => generate(PathBuf::from(dir), 100_000),
        [cmd, dir, n] if cmd == "gen" => generate(PathBuf::from(dir), n.parse().expect("count must be a number")),
        _ => {
            eprintln!("usage:\n  bench gen <dir> [count]\n  bench list <dir>");
            std::process::exit(2);
        }
    }
}

const EXTS: [&str; 10] = ["txt", "png", "jpg", "rs", "pdf", "mp3", "zip", "json", "docx", "bin"];

fn generate(dir: PathBuf, count: usize) {
    fs::create_dir_all(&dir).expect("create dir");
    let start = Instant::now();
    let threads = thread::available_parallelism().map_or(4, |n| n.get()).min(16);
    let handles: Vec<_> = (0..threads)
        .map(|t| {
            let dir = dir.clone();
            thread::spawn(move || {
                for i in (t..count).step_by(threads) {
                    // 1% folders, the rest files with mixed extensions and sizes.
                    if i % 100 == 0 {
                        let _ = fs::create_dir(dir.join(format!("folder {i}")));
                    } else {
                        let name = format!("file {i}.{}", EXTS[i % EXTS.len()]);
                        let _ = fs::write(dir.join(name), vec![b'x'; i % 4096]);
                    }
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    println!("created {} entries in {} in {:.1} s", thousands(count), dir.display(), start.elapsed().as_secs_f64());
}

fn list(dir: PathBuf) {
    let ram_before = sys::memory_usage().unwrap_or(0);
    let (tx, rx) = mpsc::channel();
    let start = Instant::now();
    fs_list::list_dir(dir, Cancel::new(), move |ev| {
        let _ = tx.send((start.elapsed(), ev));
    });

    let mut entries = Vec::new();
    let mut first_batch = None;
    let mut batches = 0;
    for (at, ev) in rx {
        match ev {
            ListEvent::Batch(b) => {
                first_batch.get_or_insert(at);
                batches += 1;
                entries.extend(b);
            }
            ListEvent::Done { total, elapsed, error } => {
                if let Some(e) = error {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                }
                println!("entries        {}", thousands(total));
                println!("batches        {batches}");
                println!("first batch    {:>8.1} ms", first_batch.unwrap_or(at).as_secs_f64() * 1e3);
                println!("full listing   {:>8.1} ms", elapsed.as_secs_f64() * 1e3);
                break;
            }
        }
    }

    for key in [SortKey::Name, SortKey::Modified, SortKey::Size, SortKey::Kind] {
        let mut idx: Vec<u32> = (0..entries.len() as u32).collect();
        let t = Instant::now();
        sort::sort_indices(&mut idx, &entries, key, true);
        println!("sort {:<9} {:>8.1} ms", format!("{key:?}"), t.elapsed().as_secs_f64() * 1e3);
    }

    let t = Instant::now();
    let hits = entries.iter().filter(|e| e.key.contains("99")).count();
    println!("filter \"99\"    {:>8.1} ms  ({} hits)", t.elapsed().as_secs_f64() * 1e3, thousands(hits));

    let ram_after = sys::memory_usage().unwrap_or(0);
    println!(
        "entry memory   {:>8.1} MB  (working set {:.1} MB)",
        (ram_after.saturating_sub(ram_before)) as f64 / 1e6,
        ram_after as f64 / 1e6
    );
}

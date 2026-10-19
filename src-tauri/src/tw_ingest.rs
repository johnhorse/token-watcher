//! tw-ingest：开发验证 CLI（不入安装包）
//!
//! T1：全量解析 ~/.claude/projects → SQLite，打印今日总量（四项 + 总和）。
//! T4：接写者锁（F5）——GUI 在跑时转只读模式（只查询，不扫描不落库）。
//! 用法：
//!   tw-ingest [--path <dir>] [--db <path>]
//! 默认 path = ~/.claude/projects，默认 db = ~/.token-watcher/db.sqlite3。

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use token_watcher_lib::app_lock::{AppLock, LockError};
use token_watcher_lib::core::ingest;
use token_watcher_lib::core::store::Store;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("tw-ingest: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut dir: Option<PathBuf> = None;
    let mut db: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--path" => {
                dir = Some(PathBuf::from(args.next().ok_or("--path 缺少目录参数")?));
            }
            "--db" => {
                db = Some(PathBuf::from(args.next().ok_or("--db 缺少路径参数")?));
            }
            "-h" | "--help" => {
                print_usage();
                return Ok(());
            }
            other => {
                print_usage();
                return Err(format!("未知参数: {other}").into());
            }
        }
    }

    let home = home::home_dir().ok_or("无法定位用户 home 目录")?;
    let dir = dir.unwrap_or_else(|| home.join(".claude").join("projects"));
    let db_path = db.unwrap_or_else(|| home.join(".token-watcher").join("db.sqlite3"));

    // F5：写者锁。GUI（或另一个 tw-ingest）持有锁时转只读，绝不并发写库。
    let writable = match AppLock::acquire_default() {
        Ok(lock) => Some(lock),
        Err(LockError::Busy) => None,
        Err(LockError::Io(e)) => return Err(e.into()),
    };
    if writable.is_none() {
        println!("检测到 token-watcher GUI 正在运行（写者锁被占用）");
        println!("tw-ingest 转入只读模式：只查询聚合，不扫描、不落库");
        println!();
    }

    println!("dir: {}", dir.display());
    println!("db:  {}", db_path.display());

    let started = Instant::now();
    let mut store = Store::open(&db_path)?;
    if writable.is_some() {
        let stats = ingest::scan_dir(&mut store, &dir)?;
        let elapsed = started.elapsed();
        println!(
            "files scanned: {} (failed: {}, dirs failed: {})",
            stats.files_scanned, stats.files_failed, stats.dirs_failed
        );
        println!(
            "records: {} (bad lines: {}, truncated files: {})",
            stats.records, stats.bad_lines, stats.truncated_files
        );
        println!("elapsed: {:.3}s", elapsed.as_secs_f64());
    }

    let today = store.local_today()?;
    let totals = store.today_totals()?;

    println!();
    println!("today ({today}, 本地 00:00 起):");
    println!("  input          {}", totals.input);
    println!("  output         {}", totals.output);
    println!("  cache_read     {}", totals.cache_read);
    println!("  cache_creation {}", totals.cache_creation);
    println!("  total          {}", totals.total());
    Ok(())
}

fn print_usage() {
    println!("用法: tw-ingest [--path <dir>] [--db <db_path>]");
    println!("  --path <dir>   JSONL 根目录（默认 ~/.claude/projects）");
    println!("  --db <path>    SQLite 路径（默认 ~/.token-watcher/db.sqlite3）");
}

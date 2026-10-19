//! T1：增量 ingest
//!
//! - [`scan_dir`]：递归枚举 `*.jsonl`，对每个文件调用 [`ingest_file`]；
//! - [`ingest_file`]：按 checkpoint 的 byte offset 续读；末尾无换行的片段先试解析
//!   JSON——可解析视为完整行，解析失败才当作不完整尾行，不推进 offset
//!   （下次重读拼接）；
//! - 截断/轮转检测：`size < checkpoint.byte_offset` 或 mtime 回退 → 从 0 重扫；
//!   uuid UPSERT 幂等，重扫不会双计（D22）。
//!
//! 顺序保证：先提交记录、后写 checkpoint。若两步之间崩溃，下次从旧 offset 重读，
//! 走 UPSERT 后到者胜，结果仍正确；反过来则会丢记录。

use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use super::parse::{parse_line, ParsedLine};
use super::store::{Checkpoint, Store};
use super::CoreError;

/// 单文件一次 ingest 的结果
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct FileIngestStats {
    /// 本次解析出的有效记录数（含重复 uuid 的重放，落库由 UPSERT 去重）
    pub records: u64,
    /// 坏行数（中间位置 JSON 非法 / 用量字段无效）
    pub bad_lines: u64,
    /// 是否因截断/轮转从 0 重扫
    pub truncated: bool,
}

/// 一次目录扫描的汇总
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ScanStats {
    pub files_scanned: u64,
    /// 单文件读取失败数（D17 的「跳过 N 文件」来源）
    pub files_failed: u64,
    /// 不可读子目录数（根目录不可读会直接报错，不计入这里）
    pub dirs_failed: u64,
    pub records: u64,
    pub bad_lines: u64,
    pub truncated_files: u64,
}

/// checkpoint 的键：文件规范路径（符号链接/相对路径统一成同一把钥匙）
pub fn checkpoint_key(path: &Path) -> String {
    fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

/// 增量读取单个文件并落库
pub fn ingest_file(store: &mut Store, path: &Path) -> Result<FileIngestStats, CoreError> {
    let metadata = fs::metadata(path)?;
    let size = metadata.len().min(i64::MAX as u64) as i64;
    let mtime_ms = metadata
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0);

    let key = checkpoint_key(path);
    let mut truncated = false;
    let mut offset = 0i64;
    if let Some(checkpoint) = store.get_checkpoint(&key)? {
        if size < checkpoint.byte_offset || mtime_ms < checkpoint.mtime_ms {
            // 截断/轮转：从 0 重扫（UPSERT 幂等）
            truncated = true;
        } else {
            offset = checkpoint.byte_offset;
        }
    }

    let mut stats = FileIngestStats {
        truncated,
        ..Default::default()
    };
    let mut file = fs::File::open(path)?;
    if offset > 0 {
        file.seek(SeekFrom::Start(offset as u64))?;
    }
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)?;

    let mut records = Vec::new();
    let mut new_offset = offset;
    let mut line_start = 0usize;
    for i in 0..buf.len() {
        if buf[i] == b'\n' {
            match parse_line(&buf[line_start..i]) {
                ParsedLine::Record(record) => records.push(record),
                ParsedLine::Bad => stats.bad_lines += 1,
                ParsedLine::Ignored => {}
            }
            line_start = i + 1;
            new_offset = offset + line_start as i64;
        }
    }
    // buf[line_start..] 是末尾无换行的片段。契约以 JSON 是否可解析判定完整性：
    // 解析成功 → 是完整行（部分写入工具不补结尾换行），照常处理并推进 offset；
    // 解析失败 → 写入进行中的不完整尾行，不计数、不推进，下次重读拼接。
    let tail = &buf[line_start..];
    if !tail.is_empty() && serde_json::from_slice::<serde_json::Value>(tail).is_ok() {
        match parse_line(tail) {
            ParsedLine::Record(record) => records.push(record),
            ParsedLine::Bad => stats.bad_lines += 1,
            ParsedLine::Ignored => {}
        }
        new_offset = offset + buf.len() as i64;
    }
    stats.records = records.len() as u64;

    store.upsert_records(&records)?;
    store.set_checkpoint(
        &key,
        Checkpoint {
            byte_offset: new_offset,
            mtime_ms,
            size: size.max(new_offset),
        },
    )?;
    Ok(stats)
}

/// 递归扫描目录下所有 `*.jsonl` 并增量落库。
///
/// 根目录不存在按空态容忍；根目录不可读直接报错（D17 的第一档）。
/// 单个文件失败跳过计数，不中断整轮扫描；DB 故障则上抛。
pub fn scan_dir(store: &mut Store, root: &Path) -> Result<ScanStats, CoreError> {
    let mut stats = ScanStats::default();
    if !root.exists() {
        return Ok(stats);
    }
    if !root.is_dir() {
        return Err(CoreError::Other(format!(
            "扫描根不是目录: {}",
            root.display()
        )));
    }
    // 根目录可读性单独判定，区别于「空目录」
    if let Err(e) = fs::read_dir(root) {
        return Err(CoreError::Io(e));
    }

    let mut files = Vec::new();
    collect_jsonl(root, &mut files, &mut stats.dirs_failed);
    // 排序保证跨次运行/跨平台的导入顺序稳定（同 uuid 跨文件时后到者胜）
    files.sort();

    for path in files {
        stats.files_scanned += 1;
        match ingest_file(store, &path) {
            Ok(file_stats) => {
                stats.records += file_stats.records;
                stats.bad_lines += file_stats.bad_lines;
                if file_stats.truncated {
                    stats.truncated_files += 1;
                }
            }
            Err(e @ CoreError::Db(_)) => return Err(e),
            Err(_) => stats.files_failed += 1,
        }
    }
    Ok(stats)
}

fn collect_jsonl(dir: &Path, out: &mut Vec<PathBuf>, dirs_failed: &mut u64) {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => {
            *dirs_failed += 1;
            return;
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(metadata) = fs::metadata(&path) else {
            continue;
        };
        if metadata.is_dir() {
            collect_jsonl(&path, out, dirs_failed);
        } else if metadata.is_file() && path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            out.push(path);
        }
    }
}

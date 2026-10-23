//! T1：增量 ingest
//!
//! - [`scan_dir`]：递归枚举 `*.jsonl`，对每个文件调用 [`ingest_file`]；
//! - [`ingest_file`]：按 checkpoint 的 byte offset 续读；末尾无换行的片段先试解析
//!   JSON——可解析视为完整行，解析失败才当作不完整尾行，不推进 offset
//!   （下次重读拼接）；
//! - 截断/轮转检测：`size < checkpoint.byte_offset` 或 mtime 回退 → 从 0 重扫；
//!   uuid UPSERT 幂等，重扫不会双计（D22）。
//!
//! T3 扩展（保持原 API 不变，测试无感）：
//! - [`scan_dir_with_progress`]：首扫进度回调（done/total），供 scan-progress 事件；
//! - [`ingest_file_with_records`]：返回本次解析出的记录，供 usage-pulse 事件。
//!
//! 顺序保证：先提交记录、后写 checkpoint。若两步之间崩溃，下次从旧 offset 重读，
//! 走 UPSERT 后到者胜，结果仍正确；反过来则会丢记录。

use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use super::parse::{
    extract_cline_model, parse_aider_line, parse_antigravity_db, parse_cline_roo_entries,
    parse_codex_line, parse_continue_session, parse_line, parse_omp_line, parse_opencode_message,
    parse_qoder_line, parse_workbuddy_line, parse_zcode_line, AiderSessionTracker, CodexFileState,
    ParsedLine, UsageRecord,
};
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
    /// UPSERT 实际受影响的行数（v0.7.2 R7：flush 的 emit 判定改由它驱动）
    pub changed: u64,
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

/// 日志文件所属的 AI 引擎类别
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileEngineKind {
    ClaudeCode,
    CodexCli,
    Cline,
    RooCode,
    Aider,
    OpenCode,
    Continue,
    Omp,
    Antigravity,
    ZCode,
    Qoder,
    WorkBuddy,
}

/// 依据文件名及内容抽样自动判定文件所属引擎
pub fn detect_file_kind(path: &Path, sample_bytes: &[u8]) -> FileEngineKind {
    let filename = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    let path_str = path.to_string_lossy();
    if filename.ends_with(".db")
        && (path_str.contains("/.gemini/antigravity/")
            || path_str.contains("\\.gemini\\antigravity\\")
            || path_str.contains("/antigravity/")
            || path_str.contains("\\antigravity\\"))
    {
        return FileEngineKind::Antigravity;
    }
    if filename == "opencode.db"
        || (filename.ends_with(".db")
            && (path_str.contains("/opencode/") || path_str.contains("\\opencode\\")))
    {
        return FileEngineKind::OpenCode;
    }
    if filename.ends_with(".jsonl")
        && (path_str.contains("/.zcode/")
            || path_str.contains("\\.zcode\\")
            || path_str.contains("/zcode/")
            || path_str.contains("\\zcode\\"))
    {
        return FileEngineKind::ZCode;
    }
    if filename.ends_with(".jsonl")
        && (path_str.contains("/.qoder/")
            || path_str.contains("\\.qoder\\")
            || path_str.contains("/qoder/")
            || path_str.contains("\\qoder\\"))
    {
        return FileEngineKind::Qoder;
    }
    if filename.ends_with(".jsonl")
        && (path_str.contains("/.codebuddy/")
            || path_str.contains("\\.codebuddy\\")
            || path_str.contains("/.workbuddy/")
            || path_str.contains("\\.workbuddy\\")
            || path_str.contains("/codebuddy/")
            || path_str.contains("\\codebuddy\\")
            || path_str.contains("/workbuddy/")
            || path_str.contains("\\workbuddy\\"))
    {
        return FileEngineKind::WorkBuddy;
    }
    if filename.ends_with(".json")
        && (path_str.contains(".continue") || path_str.contains("continue"))
    {
        return FileEngineKind::Continue;
    }
    if filename == "ui_messages.json" {
        if path_str.contains("roo-cline")
            || path_str.contains("rooveterinaryinc")
            || path_str.contains("roo_tasks")
            || path_str.contains("/roo/")
            || path_str.contains("\\roo\\")
        {
            return FileEngineKind::RooCode;
        } else {
            return FileEngineKind::Cline;
        }
    }
    if filename.contains("aider") || sample_bytes.windows(14).any(|w| w == b"\"message_send\"") {
        return FileEngineKind::Aider;
    }
    if path_str.contains(".omp")
        || path_str.contains("/omp/")
        || path_str.contains("\\omp\\")
        || path_str.contains("/omp_")
        || path_str.contains("\\omp_")
        || path_str.contains("omp_sessions")
    {
        return FileEngineKind::Omp;
    }
    if filename.starts_with("rollout-") && filename.ends_with(".jsonl") {
        return FileEngineKind::CodexCli;
    }
    if sample_bytes.windows(14).any(|w| w == b"\"session_meta\"")
        || sample_bytes
            .windows(20)
            .any(|w| w == b"\"token_usage_record\"")
    {
        return FileEngineKind::CodexCli;
    }
    FileEngineKind::ClaudeCode
}

/// 极速状态重放：扫描从 0 到 up_to_offset 字节的元数据行，重建 CodexFileState
pub fn replay_codex_state(
    file_path: &Path,
    up_to_offset: u64,
) -> Result<CodexFileState, CoreError> {
    let mut state = CodexFileState::default();
    if up_to_offset == 0 {
        return Ok(state);
    }
    let mut file = fs::File::open(file_path)?;
    let mut handle = (&mut file).take(up_to_offset);
    let mut buf = Vec::new();
    handle.read_to_end(&mut buf)?;

    let mut line_start = 0usize;
    for i in 0..buf.len() {
        if buf[i] == b'\n' {
            let line = &buf[line_start..i];
            parse_codex_line(line, &mut state);
            line_start = i + 1;
        }
    }
    let tail = &buf[line_start..];
    if !tail.is_empty() {
        parse_codex_line(tail, &mut state);
    }
    Ok(state)
}

/// checkpoint 的键：文件规范路径（符号链接/相对路径统一成同一把钥匙）
pub fn checkpoint_key(path: &Path) -> String {
    fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned()
}
/// 从 omp 会话文件中提取项目的实际工作目录 cwd
pub fn extract_omp_cwd(path: &Path) -> Option<String> {
    if let Ok(file) = fs::File::open(path) {
        use std::io::BufRead;
        let mut reader = std::io::BufReader::new(file);
        let mut line = String::new();
        // 扫描前 10 行，提取 {"type":"session", ... "cwd":"..."}
        for _ in 0..10 {
            line.clear();
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                break;
            }
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) {
                if v.get("type").and_then(|s| s.as_str()) == Some("session") {
                    if let Some(cwd) = v.get("cwd").and_then(|s| s.as_str()) {
                        if !cwd.is_empty() {
                            return Some(cwd.to_string());
                        }
                    }
                }
            }
        }
    }
    // 降级兜底：从父目录名解码，如 "--Volumes-Data-projects-token-watcher--"
    if let Some(dir_name) = path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|s| s.to_str())
    {
        if dir_name.starts_with("--") && dir_name.ends_with("--") {
            let trimmed = dir_name.trim_matches('-');
            if !trimmed.is_empty() {
                return Some(format!("/{}", trimmed.replace('-', "/")));
            }
        }
    }
    None
}

/// 增量读取单个文件并落库
pub fn ingest_file(store: &mut Store, path: &Path) -> Result<FileIngestStats, CoreError> {
    Ok(ingest_file_with_records(store, path)?.0)
}

/// 同 [`ingest_file`]，但把本次解析出的记录一并返回（T3：usage-pulse 用）。
///
/// 「新记录」= 本次从 checkpoint 之后读到的 Record 行，不区分 UPSERT 是否真的
/// 插入（uuid 重复时不会新增行）。重复导入/截断重扫会重复回报这些行，
/// UI 侧脉冲累计按幂等处理（契约允许）。
pub fn ingest_file_with_records(
    store: &mut Store,
    path: &Path,
) -> Result<(FileIngestStats, Vec<UsageRecord>), CoreError> {
    ingest_file_with_records_opts(store, path, false)
}

/// 同 [`ingest_file_with_records`]，`force_read` 强制重读 SQLite 源：
/// 绕过 Antigravity 主库 (mtime,size) 相等短路（v0.7.2 R1）——WAL-only 写入
/// 不改变主库 stat，不强制重读会看到「无变化」而空手而归。
pub fn ingest_file_with_records_forced(
    store: &mut Store,
    path: &Path,
    force_read: bool,
) -> Result<(FileIngestStats, Vec<UsageRecord>), CoreError> {
    ingest_file_with_records_opts(store, path, force_read)
}

fn ingest_file_with_records_opts(
    store: &mut Store,
    path: &Path,
    force_read: bool,
) -> Result<(FileIngestStats, Vec<UsageRecord>), CoreError> {
    let metadata = fs::metadata(path)?;
    let size = metadata.len().min(i64::MAX as u64) as i64;
    let mtime_ms = metadata
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0);

    let key = checkpoint_key(path);
    let engine_kind = detect_file_kind(path, &[]);
    if engine_kind == FileEngineKind::Cline || engine_kind == FileEngineKind::RooCode {
        let previous_count = store
            .get_checkpoint(&key)?
            .map(|c| c.byte_offset)
            .unwrap_or(0);
        let is_same = store
            .get_checkpoint(&key)?
            .map(|c| c.mtime_ms == mtime_ms && c.size == size)
            .unwrap_or(false);
        if is_same {
            return Ok((FileIngestStats::default(), Vec::new()));
        }
        let ui_bytes = match fs::read(path) {
            Ok(b) => b,
            Err(_) => return Ok((FileIngestStats::default(), Vec::new())),
        };
        let history_path = path
            .parent()
            .unwrap_or(path)
            .join("api_conversation_history.json");
        let history_str = fs::read_to_string(&history_path).unwrap_or_default();
        let model = extract_cline_model(&history_str);
        let task_id = path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or("unknown_task");
        let engine_str = if engine_kind == FileEngineKind::RooCode {
            "roo"
        } else {
            "cline"
        };
        let records = match parse_cline_roo_entries(engine_str, task_id, &ui_bytes, &model) {
            Ok(r) => r,
            Err(_) => return Ok((FileIngestStats::default(), Vec::new())),
        };
        let changed = store.upsert_records(&records)?;
        store.set_checkpoint(
            &key,
            Checkpoint {
                byte_offset: records.len() as i64,
                mtime_ms,
                size,
            },
        )?;
        let new_records = if records.len() as i64 >= previous_count {
            records[previous_count as usize..].to_vec()
        } else {
            records
        };
        let stats = FileIngestStats {
            records: new_records.len() as u64,
            bad_lines: 0,
            truncated: false,
            changed,
        };
        return Ok((stats, new_records));
    }

    if engine_kind == FileEngineKind::OpenCode {
        let previous_ts = store
            .get_checkpoint(&key)?
            .map(|c| c.byte_offset)
            .unwrap_or(0);
        let conn = match rusqlite::Connection::open_with_flags(
            path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
        ) {
            Ok(c) => c,
            Err(_) => return Ok((FileIngestStats::default(), Vec::new())),
        };
        let _ = conn.execute_batch("PRAGMA query_only = ON;");
        // v0.7.2 R5：游标含边界（>=）。同毫秒并列行与事后补 usage 行不再被
        // 永久跳过；边界行重复读入由 record_id UPSERT 幂等兜底。
        let mut stmt = match conn.prepare(
            "SELECT m.id, m.session_id, m.time_created, m.data FROM message m WHERE m.time_created >= ? ORDER BY m.time_created ASC",
        ) {
            Ok(s) => s,
            Err(_) => return Ok((FileIngestStats::default(), Vec::new())),
        };
        let mut rows = match stmt.query(rusqlite::params![previous_ts]) {
            Ok(r) => r,
            Err(_) => return Ok((FileIngestStats::default(), Vec::new())),
        };
        let mut records = Vec::new();
        let mut max_ts = previous_ts;
        while let Ok(Some(row)) = rows.next() {
            let id: String = row.get(0).unwrap_or_default();
            let session_id: String = row.get(1).unwrap_or_default();
            let time_created: i64 = row.get(2).unwrap_or(0);
            let data_json: String = row.get(3).unwrap_or_default();
            if time_created > max_ts {
                max_ts = time_created;
            }
            if let Some(record) = parse_opencode_message(&id, &session_id, time_created, &data_json)
            {
                records.push(record);
            }
        }
        let changed = if records.is_empty() {
            0
        } else {
            store.upsert_records(&records)?
        };
        if max_ts > previous_ts {
            store.set_checkpoint(
                &key,
                Checkpoint {
                    byte_offset: max_ts,
                    mtime_ms,
                    size,
                },
            )?;
        }
        let stats = FileIngestStats {
            records: records.len() as u64,
            bad_lines: 0,
            truncated: false,
            changed,
        };
        return Ok((stats, records));
    }

    if engine_kind == FileEngineKind::Continue {
        let is_same = store
            .get_checkpoint(&key)?
            .map(|c| c.mtime_ms == mtime_ms && c.size == size)
            .unwrap_or(false);
        if is_same {
            return Ok((FileIngestStats::default(), Vec::new()));
        }
        let bytes = match fs::read(path) {
            Ok(b) => b,
            Err(_) => return Ok((FileIngestStats::default(), Vec::new())),
        };
        let file_stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("continue-session");
        let records = match parse_continue_session(file_stem, &bytes) {
            Ok(r) => r,
            Err(_) => return Ok((FileIngestStats::default(), Vec::new())),
        };
        let changed = store.upsert_records(&records)?;
        store.set_checkpoint(
            &key,
            Checkpoint {
                byte_offset: records.len() as i64,
                mtime_ms,
                size,
            },
        )?;
        let stats = FileIngestStats {
            records: records.len() as u64,
            bad_lines: 0,
            truncated: false,
            changed,
        };
        return Ok((stats, records));
    }

    if engine_kind == FileEngineKind::Antigravity {
        let mut previous_idx = -1i64;
        let mut is_truncated = false;
        if let Some(c) = store.get_checkpoint(&key)? {
            // 大小与修改时间一致时短路跳过，避免重复轮询 SQLite 产生开销。
            // v0.7.2 R1：force_read（WAL 增长 / .db-wal 事件）必须绕过该短路——
            // WAL-only 写入不改变主库 stat，否则读到「无变化」漏掉新行。
            if !force_read && c.mtime_ms == mtime_ms && c.size == size {
                return Ok((FileIngestStats::default(), Vec::new()));
            }
            if size < c.size || mtime_ms < c.mtime_ms {
                is_truncated = true;
                previous_idx = -1;
            } else {
                previous_idx = c.byte_offset;
            }
        }

        let (records, max_idx) = parse_antigravity_db(path, previous_idx)?;
        let changed = if records.is_empty() {
            0
        } else {
            store.upsert_records(&records)?
        };
        if max_idx > previous_idx || is_truncated {
            store.set_checkpoint(
                &key,
                Checkpoint {
                    byte_offset: max_idx,
                    mtime_ms,
                    size,
                },
            )?;
        }
        let stats = FileIngestStats {
            records: records.len() as u64,
            bad_lines: 0,
            truncated: is_truncated,
            changed,
        };
        return Ok((stats, records));
    }

    let mut truncated = false;
    let mut offset = 0i64;
    if let Some(checkpoint) = store.get_checkpoint(&key)? {
        if size < checkpoint.byte_offset || mtime_ms < checkpoint.mtime_ms {
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

    let engine_kind = detect_file_kind(path, &buf[..buf.len().min(1024)]);
    let mut codex_state = if engine_kind == FileEngineKind::CodexCli {
        if offset > 0 {
            replay_codex_state(path, offset as u64)?
        } else {
            CodexFileState::default()
        }
    } else {
        CodexFileState::default()
    };
    let mut aider_tracker = AiderSessionTracker::default();
    let omp_cwd = if engine_kind == FileEngineKind::Omp {
        extract_omp_cwd(path)
    } else {
        None
    };
    let mut records = Vec::new();
    let mut new_offset = offset;
    let mut line_start = 0usize;
    for i in 0..buf.len() {
        if buf[i] == b'\n' {
            let line = &buf[line_start..i];
            let omp_session_id = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("omp-session");
            let file_session_id = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("session");
            let parsed = match engine_kind {
                FileEngineKind::ClaudeCode => parse_line(line),
                FileEngineKind::CodexCli => parse_codex_line(line, &mut codex_state),
                FileEngineKind::Aider => parse_aider_line(line, &mut aider_tracker),
                FileEngineKind::Omp => parse_omp_line(line, omp_session_id, omp_cwd.as_deref()),
                FileEngineKind::ZCode => parse_zcode_line(line, file_session_id),
                FileEngineKind::Qoder => parse_qoder_line(line, file_session_id),
                FileEngineKind::WorkBuddy => parse_workbuddy_line(line, file_session_id),
                FileEngineKind::Cline
                | FileEngineKind::RooCode
                | FileEngineKind::OpenCode
                | FileEngineKind::Continue
                | FileEngineKind::Antigravity => ParsedLine::Ignored,
            };
            match parsed {
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
        let omp_session_id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("omp-session");
        let file_session_id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("session");
        let parsed = match engine_kind {
            FileEngineKind::ClaudeCode => parse_line(tail),
            FileEngineKind::CodexCli => parse_codex_line(tail, &mut codex_state),
            FileEngineKind::Aider => parse_aider_line(tail, &mut aider_tracker),
            FileEngineKind::Omp => parse_omp_line(tail, omp_session_id, omp_cwd.as_deref()),
            FileEngineKind::ZCode => parse_zcode_line(tail, file_session_id),
            FileEngineKind::Qoder => parse_qoder_line(tail, file_session_id),
            FileEngineKind::WorkBuddy => parse_workbuddy_line(tail, file_session_id),
            FileEngineKind::Cline
            | FileEngineKind::RooCode
            | FileEngineKind::OpenCode
            | FileEngineKind::Continue
            | FileEngineKind::Antigravity => ParsedLine::Ignored,
        };
        match parsed {
            ParsedLine::Record(record) => records.push(record),
            ParsedLine::Bad => stats.bad_lines += 1,
            ParsedLine::Ignored => {}
        }
        new_offset = offset + buf.len() as i64;
    }
    stats.records = records.len() as u64;

    stats.changed = store.upsert_records(&records)?;
    store.set_checkpoint(
        &key,
        Checkpoint {
            byte_offset: new_offset,
            mtime_ms,
            size: size.max(new_offset),
        },
    )?;
    Ok((stats, records))
}

/// 递归扫描目录下所有 `*.jsonl` 并增量落库。
///
/// 根目录不存在按空态容忍；根目录不可读直接报错（D17 的第一档）。
/// 单个文件失败跳过计数，不中断整轮扫描；DB 故障则上抛。
pub fn scan_dir(store: &mut Store, root: &Path) -> Result<ScanStats, CoreError> {
    scan_dir_with_progress(store, root, |_, _| {})
}

/// 同 [`scan_dir`]，并在枚举完成后回调 `(0, files_total)`、每处理完一个文件
/// 回调 `(files_done, files_total)`（T3：scan-progress 事件）。
pub fn scan_dir_with_progress<F>(
    store: &mut Store,
    root: &Path,
    mut progress: F,
) -> Result<ScanStats, CoreError>
where
    F: FnMut(u64, u64),
{
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

    let total = files.len() as u64;
    progress(0, total);
    for (index, path) in files.into_iter().enumerate() {
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
        progress(index as u64 + 1, total);
    }
    Ok(stats)
}

pub fn collect_jsonl(dir: &Path, out: &mut Vec<PathBuf>, dirs_failed: &mut u64) {
    collect_source_files(dir, out, dirs_failed);
}

pub fn collect_source_files(dir: &Path, out: &mut Vec<PathBuf>, dirs_failed: &mut u64) {
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
            collect_source_files(&path, out, dirs_failed);
        } else if metadata.is_file() {
            let is_jsonl = path.extension().and_then(|e| e.to_str()) == Some("jsonl");
            let is_ui_messages =
                path.file_name().and_then(|n| n.to_str()) == Some("ui_messages.json");
            let is_continue = path.extension().and_then(|e| e.to_str()) == Some("json")
                && (path.to_string_lossy().contains(".continue")
                    || path.to_string_lossy().contains("continue"));
            let is_opencode = path.file_name().and_then(|n| n.to_str()) == Some("opencode.db");
            let is_antigravity = path.extension().and_then(|e| e.to_str()) == Some("db")
                && (path.to_string_lossy().contains("antigravity")
                    || path.to_string_lossy().contains(".gemini")
                    || path.to_string_lossy().contains("gemini"));
            if is_jsonl || is_ui_messages || is_continue || is_opencode || is_antigravity {
                out.push(path);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_detect_file_kind() {
        assert_eq!(
            detect_file_kind(Path::new("rollout-2026-10-20.jsonl"), b""),
            FileEngineKind::CodexCli
        );
        assert_eq!(
            detect_file_kind(Path::new("sess-123.jsonl"), b"{\"type\":\"session_meta\"}"),
            FileEngineKind::CodexCli
        );
        assert_eq!(
            detect_file_kind(Path::new("sess-123.jsonl"), b"{\"type\":\"assistant\"}"),
            FileEngineKind::ClaudeCode
        );
        assert_eq!(
            detect_file_kind(
                Path::new("/Users/u/.gemini/antigravity/conversations/conv.db"),
                b""
            ),
            FileEngineKind::Antigravity
        );
        assert_eq!(
            detect_file_kind(
                Path::new("/Users/u/.zcode/cli/rollout/model-io-session_123.jsonl"),
                b""
            ),
            FileEngineKind::ZCode
        );
        assert_eq!(
            detect_file_kind(
                Path::new("/Users/u/.qoder/projects/demo/session.jsonl"),
                b""
            ),
            FileEngineKind::Qoder
        );
        assert_eq!(
            detect_file_kind(
                Path::new("/Users/u/.codebuddy/projects/demo/session.jsonl"),
                b""
            ),
            FileEngineKind::WorkBuddy
        );
        // 确保包含引擎名称的普通 Claude 项目目录绝不被误判
        assert_eq!(
            detect_file_kind(
                Path::new("/Users/u/.claude/projects/-qoder-demo/sess.jsonl"),
                b"{\"type\":\"assistant\"}"
            ),
            FileEngineKind::ClaudeCode
        );
    }

    #[test]
    fn test_codex_incremental_replay_and_ingest() {
        let temp_dir = tempfile_dir("codex-test");
        let file_path = temp_dir.join("rollout-2026-10-20.jsonl");
        let mut store = Store::open_in_memory().unwrap();

        // 模拟第 1 轮写入：session_meta + turn_context(A) + usage(A)
        {
            let mut f = fs::File::create(&file_path).unwrap();
            writeln!(
                f,
                r#"{{"type":"session_meta","payload":{{"session_id":"s-1","cwd":"/my/project","originator":"codex-tui"}}}}"#
            ).unwrap();
            writeln!(
                f,
                r#"{{"type":"turn_context","payload":{{"turn_id":"t-1","model":"gpt-6.1-sol"}}}}"#
            )
            .unwrap();
            writeln!(
                f,
                r#"{{"timestamp":"2026-10-20T10:00:00.000Z","type":"token_usage_record","payload":{{"session_id":"s-1","turn_id":"t-1","response_id":"resp-1","usage":{{"input_tokens":1000,"output_tokens":100,"total_tokens":1100}}}}}}"#
            ).unwrap();
        }

        // 第 1 次 ingest（从 offset 0 读起）
        let (stats1, records1) = ingest_file_with_records(&mut store, &file_path).unwrap();
        assert_eq!(stats1.records, 1);
        assert_eq!(records1.len(), 1);
        assert_eq!(records1[0].model, "gpt-6.1-sol");
        assert_eq!(records1[0].entrypoint, "codex-tui");
        assert_eq!(records1[0].record_id, "codex:s-1:resp-1");

        // 模拟追加第 2 轮：更换模型为 gemini-4-argon，并追加 usage(B)
        {
            let mut f = fs::OpenOptions::new()
                .append(true)
                .open(&file_path)
                .unwrap();
            writeln!(
                f,
                r#"{{"type":"turn_context","payload":{{"turn_id":"t-2","model":"gemini-4-argon"}}}}"#
            ).unwrap();
            writeln!(
                f,
                r#"{{"timestamp":"2026-10-20T10:05:00.000Z","type":"token_usage_record","payload":{{"session_id":"s-1","turn_id":"t-2","response_id":"resp-2","usage":{{"input_tokens":2000,"output_tokens":200,"total_tokens":2200}}}}}}"#
            ).unwrap();
        }

        // 第 2 次增量 ingest（从 checkpoint offset 续读，触发 replay 验证）
        let (stats2, records2) = ingest_file_with_records(&mut store, &file_path).unwrap();
        assert_eq!(stats2.records, 1);
        assert_eq!(records2.len(), 1);
        assert_eq!(records2[0].model, "gemini-4-argon");
        assert_eq!(records2[0].record_id, "codex:s-1:resp-2");

        // 模拟更细粒度切分：紧随其后追加第三条 usage(C) 在同一 turn_context 之后（断点位于 turn_context 之后）
        {
            let mut f = fs::OpenOptions::new()
                .append(true)
                .open(&file_path)
                .unwrap();
            writeln!(
                f,
                r#"{{"timestamp":"2026-10-20T10:06:00.000Z","type":"token_usage_record","payload":{{"session_id":"s-1","turn_id":"t-2","response_id":"resp-3","usage":{{"input_tokens":3000,"output_tokens":300,"total_tokens":3300}}}}}}"#
            ).unwrap();
        }

        // 第 3 次增量续读：此时续读点刚好停在 turn_context 之后，通过 replay 重建状态，必须拿到 gemini-4-argon！
        let (stats3, records3) = ingest_file_with_records(&mut store, &file_path).unwrap();
        assert_eq!(stats3.records, 1);
        assert_eq!(records3.len(), 1);
        assert_eq!(records3[0].model, "gemini-4-argon");
        assert_eq!(records3[0].record_id, "codex:s-1:resp-3");

        let _ = fs::remove_dir_all(&temp_dir);
    }

    fn tempfile_dir(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("tw-test-{}-{}", name, std::process::id()));
        let _ = fs::create_dir_all(&p);
        p
    }
}

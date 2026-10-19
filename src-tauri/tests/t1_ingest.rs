//! T1 集成测试：fixture 驱动的 parse → store → ingest 全链路验证
//!
//! 覆盖验收点：
//! 1. 全量导入 fixture 后四项 token 求和正确
//! 2. 同批数据重复导入总量不变（SC3 幂等）
//! 3. 不完整尾行缓冲 → 补齐后正确解析且不重复
//! 4. 坏行跳过 + 计数
//! 5. 文件截断触发重扫且幂等
//! 6. 跨文件同 uuid 后到者胜、不双计
//! 7. 今日总量按本地时区 00:00 边界

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::json;
use token_watcher_lib::core::ingest;
use token_watcher_lib::core::parse::{parse_line, ParsedLine};
use token_watcher_lib::core::store::{Store, Totals};

/// 自建临时目录（避免新增 tempfile 依赖）
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("tw-t1-{}-{nanos}-{n}", std::process::id()));
        fs::create_dir_all(&path).unwrap();
        TempDir { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

/// 构造一行 assistant 用量记录
fn usage_line(
    uuid: &str,
    session: &str,
    timestamp: &str,
    input: i64,
    output: i64,
    cache_read: i64,
    cache_creation: i64,
) -> String {
    json!({
        "type": "assistant",
        "uuid": uuid,
        "session_id": session,
        "sessionId": session,
        "timestamp": timestamp,
        "message": {
            "model": "glm-5.3",
            "requestedModel": "claude-opus-5[1m]",
            "usage": {
                "input_tokens": input,
                "output_tokens": output,
                "cache_read_input_tokens": cache_read,
                "cache_creation_input_tokens": cache_creation
            }
        }
    })
    .to_string()
}

/// epoch 毫秒 → ISO-8601 UTC 字符串（借 SQLite 格式化，测试辅助）
fn iso_utc(ms: i64) -> String {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.query_row(
        "SELECT strftime('%Y-%m-%dT%H:%M:%fZ', ?1 / 1000.0, 'unixepoch')",
        [ms],
        |row| row.get(0),
    )
    .unwrap()
}

fn all_time_totals(store: &Store) -> Totals {
    store.totals_between(i64::MIN, i64::MAX).unwrap()
}

fn fixture_records(name: &str) -> Vec<token_watcher_lib::core::parse::UsageRecord> {
    fs::read_to_string(fixture(name))
        .unwrap()
        .lines()
        .filter_map(|line| match parse_line(line.as_bytes()) {
            ParsedLine::Record(r) => Some(r),
            _ => None,
        })
        .collect()
}

// 1. 全量导入 fixture → 四项 token 求和
#[test]
fn full_import_matches_expected_totals() {
    let mut store = Store::open_in_memory().unwrap();
    let stats = ingest::ingest_file(&mut store, &fixture("basic.jsonl")).unwrap();

    assert_eq!(stats.records, 3, "basic.jsonl 应有 3 条用量记录");
    assert_eq!(stats.bad_lines, 0);
    assert!(!stats.truncated);

    let totals = all_time_totals(&store);
    assert_eq!(
        totals,
        Totals {
            input: 2_050,        // 1200 + 800 + 50
            output: 427,         // 300 + 120 + 7
            cache_read: 70_000,  // 50000 + 20000 + 0
            cache_creation: 500, // 400 + 100 + 0
        }
    );
    assert_eq!(totals.total(), 72_977);
    assert_eq!(store.count_records().unwrap(), 3);
}

// 2. 同批数据导入两次 → 总量不变（SC3 幂等）
#[test]
fn repeated_import_is_idempotent() {
    let mut store = Store::open_in_memory().unwrap();
    ingest::ingest_file(&mut store, &fixture("basic.jsonl")).unwrap();
    let first = all_time_totals(&store);

    // (a) 同一文件再扫一次：checkpoint 续读，无新行
    let stats = ingest::ingest_file(&mut store, &fixture("basic.jsonl")).unwrap();
    assert_eq!(stats.records, 0);
    assert_eq!(all_time_totals(&store), first);
    assert_eq!(store.count_records().unwrap(), 3);

    // (b) 同一批记录直接 UPSERT 两次（模拟重放/重启恢复）：不累加
    let records = fixture_records("basic.jsonl");
    store.upsert_records(&records).unwrap();
    store.upsert_records(&records).unwrap();
    assert_eq!(all_time_totals(&store), first);
    assert_eq!(store.count_records().unwrap(), 3);
}

// 3. 不完整尾行：截断的行不计入，补齐后正确解析且不重复
#[test]
fn incomplete_tail_waits_for_completion() {
    let dir = TempDir::new();
    let path = dir.path().join("tail.jsonl");
    let l1 = usage_line("tail-1", "s", "2026-10-19T01:00:00.000Z", 10, 1, 0, 0);
    let l2 = usage_line("tail-2", "s", "2026-10-19T01:01:00.000Z", 20, 2, 0, 0);
    let l3 = usage_line("tail-3", "s", "2026-10-19T01:02:00.000Z", 30, 3, 0, 0);
    let split = l3.len() / 2;
    fs::write(&path, format!("{l1}\n{l2}\n{}", &l3[..split])).unwrap();

    let mut store = Store::open_in_memory().unwrap();
    let first = ingest::ingest_file(&mut store, &path).unwrap();
    assert_eq!(first.records, 2, "残行不得计入");
    assert_eq!(first.bad_lines, 0, "残行不是坏行");
    assert_eq!(all_time_totals(&store).input, 30);
    assert_eq!(store.count_records().unwrap(), 2);

    // 补齐残行剩余字节 + 换行
    use std::io::Write;
    let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(format!("{}\n", &l3[split..]).as_bytes())
        .unwrap();
    file.flush().unwrap();
    drop(file);

    let second = ingest::ingest_file(&mut store, &path).unwrap();
    assert_eq!(second.records, 1, "只应解析补齐的那一行，不重读前两行");
    assert_eq!(second.bad_lines, 0);
    let totals = all_time_totals(&store);
    assert_eq!(totals.input, 60);
    assert_eq!(totals.output, 6);
    assert_eq!(store.count_records().unwrap(), 3, "补齐后不得重复计数");
}

// 3b. 文件末尾是完整 JSON 但没有结尾换行 → 仍是完整行，应计入
#[test]
fn complete_tail_without_newline_is_ingested() {
    let dir = TempDir::new();
    let path = dir.path().join("no-newline.jsonl");
    let l1 = usage_line("nl-1", "s", "2026-10-19T01:00:00.000Z", 10, 1, 0, 0);
    let l2 = usage_line("nl-2", "s", "2026-10-19T01:01:00.000Z", 20, 2, 0, 0);
    fs::write(&path, format!("{l1}\n{l2}")).unwrap(); // 末行无换行

    let mut store = Store::open_in_memory().unwrap();
    let stats = ingest::ingest_file(&mut store, &path).unwrap();
    assert_eq!(stats.records, 2);
    assert_eq!(stats.bad_lines, 0);
    assert_eq!(all_time_totals(&store).input, 30);

    // 之后追加换行 + 新行：只解析新行，不重复末行
    use std::io::Write;
    let l3 = usage_line("nl-3", "s", "2026-10-19T01:02:00.000Z", 40, 4, 0, 0);
    let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(format!("\n{l3}\n").as_bytes()).unwrap();
    file.flush().unwrap();
    drop(file);

    let stats = ingest::ingest_file(&mut store, &path).unwrap();
    assert_eq!(stats.records, 1);
    assert_eq!(all_time_totals(&store).input, 70);
    assert_eq!(store.count_records().unwrap(), 3);
}

// 4. 坏行：中间位置非法 JSON / 四项全缺 / 字段非数字 / timestamp 无效 → 跳过 + 计数；
//    cache 字段缺失（team 占位记录形态）按 0 入库，不算坏行
#[test]
fn bad_lines_are_skipped_and_counted() {
    let mut store = Store::open_in_memory().unwrap();
    let stats = ingest::ingest_file(&mut store, &fixture("bad_lines.jsonl")).unwrap();

    assert_eq!(stats.records, 3, "两条完整记录 + 一条缺 cache 字段的记录");
    assert_eq!(
        stats.bad_lines, 4,
        "非法 JSON + 四项全缺 + 非数字 + 坏 timestamp"
    );
    let totals = all_time_totals(&store);
    assert_eq!(
        totals,
        Totals {
            input: 47, // 10 + 7（cache 缺失按 0）+ 30
            output: 5, // 1 + 1 + 3
            cache_read: 0,
            cache_creation: 0,
        }
    );
    assert_eq!(store.count_records().unwrap(), 3);
}

// 5. 文件截断（size < checkpoint offset）→ 重扫；重放幂等、最终总量与文件真值一致
#[test]
fn truncated_file_rescans_idempotently() {
    let dir = TempDir::new();
    let path = dir.path().join("trunc.jsonl");
    let l1 = usage_line("trunc-1", "s", "2026-10-19T02:00:00.000Z", 10, 1, 0, 0);
    let l2 = usage_line("trunc-2", "s", "2026-10-19T02:01:00.000Z", 20, 2, 0, 0);
    let l3 = usage_line("trunc-3", "s", "2026-10-19T02:02:00.000Z", 30, 3, 0, 0);
    let l4 = usage_line("trunc-4", "s", "2026-10-19T02:03:00.000Z", 40, 4, 0, 0);

    fs::write(&path, format!("{l1}\n{l2}\n{l3}\n")).unwrap();
    let mut store = Store::open_in_memory().unwrap();
    ingest::ingest_file(&mut store, &path).unwrap();
    assert_eq!(store.count_records().unwrap(), 3);

    // 截断到第一行：size 小于 checkpoint offset → 判定截断并从 0 重扫
    fs::write(&path, format!("{l1}\n")).unwrap();
    let stats = ingest::ingest_file(&mut store, &path).unwrap();
    assert!(stats.truncated, "应检测到截断");
    assert_eq!(stats.records, 1, "从 0 重扫，重放第一行");
    assert_eq!(store.count_records().unwrap(), 3, "重放不产生新记录");
    assert_eq!(all_time_totals(&store).input, 60);

    // 文件重新增长到 l1..l4：从 checkpoint（l1 末尾）续读，l2/l3 重放 + l4 新增
    fs::write(&path, format!("{l1}\n{l2}\n{l3}\n{l4}\n")).unwrap();
    let stats = ingest::ingest_file(&mut store, &path).unwrap();
    assert!(!stats.truncated);
    assert_eq!(stats.records, 3, "l2/l3/l4 三行");
    assert_eq!(store.count_records().unwrap(), 4, "重放不双计");
    assert_eq!(all_time_totals(&store).input, 100);
    assert_eq!(all_time_totals(&store).output, 10);
}

// 6. 跨文件同 uuid：后到者胜、不双计；文件内同 uuid 也是后到者胜
#[test]
fn duplicate_uuid_last_import_wins() {
    let mut store = Store::open_in_memory().unwrap();
    ingest::ingest_file(&mut store, &fixture("dup_a.jsonl")).unwrap();
    ingest::ingest_file(&mut store, &fixture("dup_b.jsonl")).unwrap();

    // a、b 各两条记录，其中共享 uuid dddd…d；b 后导入 → b 的版本胜出
    assert_eq!(store.count_records().unwrap(), 3);
    let totals = all_time_totals(&store);
    assert_eq!(totals.input, 253); // 250(b 版) + 1(a) + 2(b)
    assert_eq!(totals.output, 28); // 25 + 1 + 2

    // 再以“后到”的身份导入 a 的记录 → a 版本重新胜出，仍只有 3 条
    store
        .upsert_records(&fixture_records("dup_a.jsonl"))
        .unwrap();
    assert_eq!(store.count_records().unwrap(), 3);
    assert_eq!(all_time_totals(&store).input, 103); // 100 + 1 + 2

    // 文件内同 uuid 两行：后一行胜出
    let mut store2 = Store::open_in_memory().unwrap();
    let stats = ingest::ingest_file(&mut store2, &fixture("dup_within.jsonl")).unwrap();
    assert_eq!(stats.records, 2);
    assert_eq!(store2.count_records().unwrap(), 1);
    assert_eq!(all_time_totals(&store2).input, 777);
    assert_eq!(all_time_totals(&store2).output, 77);
}

// 6b. 目录扫描：递归、确定性顺序（同 uuid 后到者胜）、非 jsonl 忽略、缺失根容忍
#[test]
fn scan_dir_recurses_and_orders_files() {
    let dir = TempDir::new();
    let nested = dir.path().join("project-a").join("nested");
    fs::create_dir_all(&nested).unwrap();
    fs::copy(fixture("dup_a.jsonl"), dir.path().join("dup_a.jsonl")).unwrap();
    fs::copy(fixture("dup_b.jsonl"), nested.join("dup_b.jsonl")).unwrap();
    fs::write(dir.path().join("notes.txt"), "ignore me").unwrap();

    let mut store = Store::open_in_memory().unwrap();
    let stats = ingest::scan_dir(&mut store, dir.path()).unwrap();
    assert_eq!(stats.files_scanned, 2);
    assert_eq!(stats.files_failed, 0);
    assert_eq!(stats.records, 4);
    assert_eq!(stats.bad_lines, 0);
    // 排序后 dup_a 先、dup_b 后 → 共享 uuid 取 b 的版本
    assert_eq!(store.count_records().unwrap(), 3);
    assert_eq!(all_time_totals(&store).input, 253);

    // 根目录不存在：空态容忍
    let empty = ingest::scan_dir(&mut store, &dir.path().join("missing")).unwrap();
    assert_eq!(empty, ingest::ScanStats::default());
}

// 7. 今日总量：本地 00:00 边界（UTC 时间戳，本地时区换算）
#[test]
fn today_totals_use_local_midnight() {
    let mut store = Store::open_in_memory().unwrap();
    let today = store.local_today().unwrap();
    let (day_start, day_end) = store.local_day_bounds(&today).unwrap();
    assert!(day_end > day_start);

    let dir = TempDir::new();
    let path = dir.path().join("today.jsonl");
    let lines = [
        // 本地昨日 23:59:59.999 → 不计入今日
        usage_line("day-before", "s", &iso_utc(day_start - 1), 1, 0, 0, 0),
        // 本地今日 00:00:00.000 → 计入
        usage_line("day-start", "s", &iso_utc(day_start), 10, 0, 0, 0),
        // 本地今日 23:59:59.999 → 计入
        usage_line("day-end", "s", &iso_utc(day_end - 1), 100, 0, 0, 0),
        // 本地明日 00:00:00.000 → 不计入
        usage_line("day-after", "s", &iso_utc(day_end), 1_000, 0, 0, 0),
    ];
    fs::write(&path, format!("{}\n", lines.join("\n"))).unwrap();
    ingest::ingest_file(&mut store, &path).unwrap();

    let todays = store.today_totals().unwrap();
    assert_eq!(
        todays.input, 110,
        "跨午夜边界：只含今日 [00:00, 24:00) 两条"
    );
    assert_eq!(todays.total(), 110);
    assert_eq!(store.totals_local_day(&today).unwrap(), todays);
    // 四项都验证一遍边界行为一致
    assert_eq!(todays.output, 0);
    assert_eq!(todays.cache_read, 0);
    assert_eq!(todays.cache_creation, 0);
}

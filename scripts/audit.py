#!/usr/bin/env python3
"""token-watcher v0.7.2 对账工具（仅标准库）。

快检：按源族对比 checkpoint 与源文件当前状态，报告未消费增量。
深检（--deep）：v1 仅 omp + Claude Code，逐行 usage 与 usage_records 对账。

只读约束：所有 SQLite 均以 mode=ro + busy_timeout 打开，绝不写用户库；
应用可能正在写入，读取失败按异常报告而不是崩溃。

口径（docs/designs/v0.7.2-reliability-spec.md P2 + R5）：
- JSONL：当前 size > checkpoint.byte_offset 即未消费字节；
  未读尾部且文件 mtime 超过 60s → 异常；60s 内视为正在写入，不算异常。
- OpenCode：最新 time_created 与 checkpoint 时间戳对比，边界含 >=：
  latest > checkpoint → 落后；latest == checkpoint 时校验边界行是否已入账
  （T2 的 >= 游标会幂等重读边界行）。
- Antigravity：gen_metadata 最新 idx > checkpoint.byte_offset → 未消费。
- 记录数型（Cline/Roo ui_messages.json、Continue *.json）：当前 (mtime,size)
  与 checkpoint 存储值不一致 → changed 残留。
- SQLite 源的 WAL 指纹在 DataEngine 内存、不在 DB（spec R2），本脚本只能报告
  WAL stat，无法与持久基线比对。

10-21 已知战损用脚本内常量 KNOWN_CASUALTIES 标注（唯一一条；只吸收深检缺行），
报告会单独列出吸收条数，便于将来清理。

退出码：0 = PASS；1 = 存在异常；2 = 无法运行（数据库不可读等）。
"""

from __future__ import annotations

import argparse
import datetime as _dt
import hashlib
import json
import os
import re
import sqlite3
import sys
import time
import unicodedata
from pathlib import Path

STALE_SECONDS = 60

# 10-21 已知战损（spec Problem 段：omp 管道全量对账唯一缺失行，修复切换窗口）。
KNOWN_CASUALTIES = {
    "omp:2026-10-21T09-51-56-885Z_01a15ddf-e5d5-73cc-8817-1015c65de6a5:2e94694d":
        "2026-10-21 修复切换窗口战损（spec Problem 已记录）",
}

ENGINE_PREFIXES = (
    "omp:", "codex:", "opencode:", "continue:", "antigravity:",
    "zcode:", "qoder:", "workbuddy:", "aider:", "cline:", "roo:",
)

ENGINE_CASE_SQL = """
CASE
  WHEN record_id LIKE 'omp:%'          THEN 'omp'
  WHEN record_id LIKE 'codex:%'        THEN 'codex'
  WHEN record_id LIKE 'opencode:%'     THEN 'opencode'
  WHEN record_id LIKE 'continue:%'     THEN 'continue'
  WHEN record_id LIKE 'antigravity:%'  THEN 'antigravity'
  WHEN record_id LIKE 'zcode:%'        THEN 'zcode'
  WHEN record_id LIKE 'qoder:%'        THEN 'qoder'
  WHEN record_id LIKE 'workbuddy:%'    THEN 'workbuddy'
  WHEN record_id LIKE 'aider:%'        THEN 'aider'
  WHEN record_id LIKE 'cline:%'        THEN 'cline'
  WHEN record_id LIKE 'roo:%'          THEN 'roo'
  ELSE 'claude'
END
"""


# ---------------------------------------------------------------- 基础工具

def now_ms() -> int:
    return time.time_ns() // 1_000_000


def fmt_ms(ms: int | None) -> str:
    if ms is None:
        return "-"
    try:
        return _dt.datetime.fromtimestamp(ms / 1000).strftime("%Y-%m-%d %H:%M:%S")
    except (OverflowError, OSError, ValueError):
        return str(ms)


def fmt_bytes(n: int) -> str:
    if n < 1024:
        return f"{n} B"
    for unit in ("KB", "MB", "GB"):
        n /= 1024.0
        if n < 1024 or unit == "GB":
            return f"{n:.1f} {unit}"
    return f"{n:.1f} GB"


def display_width(text: str) -> int:
    width = 0
    for ch in text:
        width += 2 if unicodedata.east_asian_width(ch) in ("W", "F") else 1
    return width


def pad(text: str, width: int) -> str:
    return text + " " * max(0, width - display_width(text))


def print_table(headers: list[str], rows: list[list[str]]) -> None:
    widths = [display_width(h) for h in headers]
    for row in rows:
        for i, cell in enumerate(row):
            widths[i] = max(widths[i], display_width(cell))
    print("  ".join(pad(h, widths[i]) for i, h in enumerate(headers)).rstrip())
    print("  ".join("-" * widths[i] for i in range(len(headers))))
    for row in rows:
        print("  ".join(pad(cell, widths[i]) for i, cell in enumerate(row)).rstrip())


def canonical(path: str | Path) -> str:
    return os.path.realpath(os.fspath(path))


def connect_ro(path: str | Path, timeout_ms: int = 5000) -> sqlite3.Connection:
    """只读连接；WAL 一致性快照由 SQLite 自身处理，不使用 immutable。"""
    uri = Path(os.path.abspath(os.fspath(path))).as_uri() + "?mode=ro"
    conn = sqlite3.connect(uri, uri=True, timeout=timeout_ms / 1000.0)
    conn.execute("PRAGMA query_only = ON;")
    conn.execute(f"PRAGMA busy_timeout = {timeout_ms};")
    return conn


def table_exists(conn: sqlite3.Connection, name: str) -> bool:
    row = conn.execute(
        "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?", (name,)
    ).fetchone()
    return row is not None


def file_stat_ms(path: str) -> tuple[int, int]:
    st = os.stat(path)
    return st.st_mtime_ns // 1_000_000, st.st_size


def load_checkpoints(db_path: str) -> dict[str, tuple[int, int, int]]:
    conn = connect_ro(db_path)
    try:
        if not table_exists(conn, "file_checkpoints"):
            raise RuntimeError("file_checkpoints 表不存在")
        return {
            row[0]: (row[1], row[2], row[3])
            for row in conn.execute(
                "SELECT file_path, byte_offset, mtime_ms, size FROM file_checkpoints"
            )
        }
    finally:
        conn.close()


# ---------------------------------------------------------------- 数据源路径

def source_paths(home: str) -> dict:
    platform = sys.platform
    if platform == "darwin":
        global_storage = os.path.join(
            home, "Library/Application Support/Code/User/globalStorage"
        )
        opencode_db = os.path.join(home, ".local/share/opencode/opencode.db")
    elif platform.startswith("win"):
        appdata = os.environ.get("APPDATA") or os.path.join(home, "AppData/Roaming")
        global_storage = os.path.join(appdata, "Code/User/globalStorage")
        localappdata = os.environ.get("LOCALAPPDATA") or os.path.join(
            home, "AppData/Local"
        )
        opencode_db = os.path.join(localappdata, "opencode/opencode.db")
    else:
        global_storage = os.path.join(home, ".config/Code/User/globalStorage")
        xdg = os.environ.get("XDG_DATA_HOME") or os.path.join(home, ".local/share")
        opencode_db = os.path.join(xdg, "opencode/opencode.db")

    aider_primary = os.path.join(home, ".aider/analytics.jsonl")
    aider_legacy = os.path.join(home, ".aider.analytics.jsonl")
    aider_path = aider_primary if os.path.exists(aider_primary) else aider_legacy

    return {
        "jsonl_roots": [
            ("claude", os.path.join(home, ".claude/projects")),
            ("codex", os.path.join(home, ".codex/sessions")),
            ("omp", os.path.join(home, ".omp/agent/sessions")),
            ("zcode", os.path.join(home, ".zcode")),
            ("qoder", os.path.join(home, ".qoder")),
            ("workbuddy", os.path.join(home, ".codebuddy")),
        ],
        "aider": aider_path,
        "record_roots": [
            (
                "cline",
                os.path.join(global_storage, "saoudrizwan.claude-dev", "tasks"),
                "ui_messages.json",
            ),
            (
                "roo",
                os.path.join(global_storage, "rooveterinaryinc.roo-cline", "tasks"),
                "ui_messages.json",
            ),
            ("continue", os.path.join(home, ".continue/sessions"), "*.json"),
        ],
        "opencode_db": opencode_db,
        "antigravity_root": os.path.join(home, ".gemini/antigravity/conversations"),
    }


def walk_files(root: str, suffix: str) -> list[str]:
    out: list[str] = []
    if not os.path.isdir(root):
        return out
    for dirpath, _dirnames, filenames in os.walk(root):
        for name in filenames:
            if name.endswith(suffix):
                out.append(os.path.join(dirpath, name))
    out.sort()
    return out


def continue_files(root: str) -> list[str]:
    """Continue 契约：根下 *.json（与 watch.rs collect_dir_sources 同口径）。"""
    if not os.path.isdir(root):
        return []
    return sorted(
        os.path.join(root, name)
        for name in os.listdir(root)
        if name.endswith(".json") and os.path.isfile(os.path.join(root, name))
    )


# ---------------------------------------------------------------- 快检

def check_jsonl_family(
    engine: str,
    files: list[str],
    checkpoints: dict,
    current_ms: int,
) -> dict:
    unread_bytes = 0
    trailing = 0
    truncated: list[str] = []
    stale: list[tuple[str, int, int, float]] = []
    no_checkpoint = 0

    for path in files:
        mtime_ms, size = file_stat_ms(path)
        cp = checkpoints.get(canonical(path))
        if cp is None:
            no_checkpoint += 1
            if size > 0:
                unread_bytes += size
                age = (current_ms - mtime_ms) / 1000.0
                if age > STALE_SECONDS:
                    stale.append((path, size, 0, age))
            continue
        byte_offset = cp[0]
        if size > byte_offset:
            delta = size - byte_offset
            unread_bytes += delta
            age = (current_ms - mtime_ms) / 1000.0
            if age > STALE_SECONDS:
                stale.append((path, size, byte_offset, age))
            else:
                trailing += 1
        elif size < byte_offset:
            truncated.append(path)

    reasons = [
        f"{path}: 未读 {size - offset} 字节（offset={offset}, size={size}, mtime {age:.0f}s 前）"
        for path, size, offset, age in stale
    ]
    return {
        "engine": engine,
        "files": len(files),
        "unread": unread_bytes,
        "trailing": trailing,
        "no_checkpoint": no_checkpoint,
        "truncated": len(truncated),
        "anomalies": reasons,
        "note": "截断待重扫" if truncated else "",
    }


def check_record_family(
    engine: str,
    root: str,
    pattern: str,
    checkpoints: dict,
) -> dict:
    files = continue_files(root) if pattern == "*.json" else walk_files(root, pattern)
    changed: list[str] = []
    no_checkpoint = 0
    for path in files:
        mtime_ms, size = file_stat_ms(path)
        cp = checkpoints.get(canonical(path))
        if cp is None:
            if size > 0:
                no_checkpoint += 1
                changed.append(f"{path}: 无 checkpoint（size={size}）")
            continue
        if cp[1] != mtime_ms or cp[2] != size:
            changed.append(
                f"{path}: checkpoint (mtime={cp[1]}, size={cp[2]}) != 当前 "
                f"(mtime={mtime_ms}, size={size})"
            )
    return {
        "engine": engine,
        "files": len(files),
        "changed": len(changed),
        "no_checkpoint": no_checkpoint,
        "anomalies": changed,
    }


def sqlite_stat_summary(path: str, cp: tuple[int, int, int] | None) -> tuple[str, str]:
    if not os.path.isfile(path):
        return "-", "-"
    mtime_ms, size = file_stat_ms(path)
    if cp is None:
        main = "无 checkpoint"
    elif cp[1] == mtime_ms and cp[2] == size:
        main = "与 checkpoint 一致"
    else:
        main = f"主库 stat 已变（cp {fmt_ms(cp[1])}/{cp[2]}B → 现 {fmt_ms(mtime_ms)}/{size}B）"
    wal_path = path + "-wal"
    if os.path.isfile(wal_path):
        wal_mtime, wal_size = file_stat_ms(wal_path)
        wal = f"{fmt_bytes(wal_size)} @ {fmt_ms(wal_mtime)}（无持久基线）"
    else:
        wal = "无"
    return main, wal


def _token_value(value) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or value < 0:
        return None
    return value


def parse_opencode_usage(data_json: str) -> tuple[int, int, int, int] | None:
    """复刻 parse_opencode_message 的用量提取（只关心四项 token）。"""
    try:
        obj = json.loads(data_json)
    except (TypeError, ValueError):
        return None
    if not isinstance(obj, dict) or obj.get("role") != "assistant":
        return None
    tokens = obj.get("tokens")
    if not isinstance(tokens, dict):
        return None
    inp = _token_value(tokens.get("input")) or 0
    out = _token_value(tokens.get("output")) or 0
    cache = tokens.get("cache") if isinstance(tokens.get("cache"), dict) else {}
    cache_read = _token_value(cache.get("read")) or 0
    cache_write = _token_value(cache.get("write")) or 0
    if inp == out == cache_read == cache_write == 0:
        return None
    return inp, out, cache_read, cache_write


def check_opencode(
    db_path: str,
    app_db: str,
    checkpoints: dict,
) -> tuple[dict, list[str]]:
    result = {
        "engine": "opencode",
        "path": db_path,
        "files": 1 if os.path.isfile(db_path) else 0,
        "unread": 0,
        "boundary": 0,
        "boundary_unread": 0,
        "latest": None,
        "checkpoint": None,
        "main_stat": "-",
        "wal": "-",
        "anomalies": [],
    }
    if not os.path.isfile(db_path):
        result["note"] = "源库不存在"
        return result, []

    cp = checkpoints.get(canonical(db_path))
    cp_ts = cp[0] if cp else 0
    result["checkpoint"] = cp_ts if cp else None
    result["main_stat"], result["wal"] = sqlite_stat_summary(db_path, cp)

    try:
        conn = connect_ro(db_path)
        try:
            if not table_exists(conn, "message"):
                raise RuntimeError("message 表不存在")
            latest = conn.execute("SELECT MAX(time_created) FROM message").fetchone()[0]
            unread = conn.execute(
                "SELECT COUNT(*) FROM message WHERE time_created > ?", (cp_ts,)
            ).fetchone()[0]
            boundary_rows = conn.execute(
                "SELECT id, session_id, data FROM message WHERE time_created = ?",
                (cp_ts,),
            ).fetchall() if cp else []
        finally:
            conn.close()
    except Exception as exc:  # 只读打开/查询失败：如实报告，不崩溃
        result["anomalies"].append(f"{db_path}: 无法读取源库（{exc}）")
        return result, result["anomalies"]

    result["latest"] = latest
    result["unread"] = unread
    result["boundary"] = len(boundary_rows)

    # R5 边界口径：latest == checkpoint 时，time_created 相同的边界行由 >= 游标
    # 幂等重读；这里只校验这些边界行是否真的入账（防止旧 > 游标永久漏行）。
    boundary_ids: list[str] = []
    for msg_id, session_id, data in boundary_rows:
        usage = parse_opencode_usage(data or "")
        if usage is not None and msg_id and session_id:
            boundary_ids.append(f"opencode:{session_id}:{msg_id}")
    if boundary_ids:
        app_conn = connect_ro(app_db)
        try:
            if table_exists(app_conn, "usage_records"):
                existing = {
                    row[0]
                    for row in app_conn.execute(
                        "SELECT record_id FROM usage_records WHERE record_id IN "
                        f"({','.join('?' * len(boundary_ids))})",
                        boundary_ids,
                    )
                }
            else:
                existing = set()
        finally:
            app_conn.close()
        missing = [rid for rid in boundary_ids if rid not in existing]
        result["boundary_unread"] = len(missing)
        for rid in missing[:20]:
            result["anomalies"].append(f"{db_path}: 边界行未入账 {rid}")

    if latest is not None and latest > cp_ts:
        result["anomalies"].append(
            f"{db_path}: OpenCode 游标落后（checkpoint={cp_ts}, latest={latest}, "
            f"未消费 {unread} 行）"
        )
    return result, result["anomalies"]


def check_antigravity(
    root: str,
    checkpoints: dict,
) -> tuple[list[dict], list[str]]:
    results: list[dict] = []
    anomalies: list[str] = []
    for db_path in walk_files(root, ".db"):
        cp = checkpoints.get(canonical(db_path))
        cp_idx = cp[0] if cp else -1
        row = {
            "db": os.path.basename(db_path),
            "path": db_path,
            "checkpoint_idx": cp[0] if cp else None,
            "latest_idx": None,
            "unread": 0,
            "main_stat": "-",
            "wal": "-",
            "anomalies": [],
        }
        row["main_stat"], row["wal"] = sqlite_stat_summary(db_path, cp)
        try:
            conn = connect_ro(db_path)
            try:
                if not table_exists(conn, "gen_metadata"):
                    raise RuntimeError("gen_metadata 表不存在")
                latest = conn.execute("SELECT MAX(idx) FROM gen_metadata").fetchone()[0]
                unread = conn.execute(
                    "SELECT COUNT(*) FROM gen_metadata WHERE idx > ?", (cp_idx,)
                ).fetchone()[0]
            finally:
                conn.close()
        except Exception as exc:
            msg = f"{db_path}: 无法读取源库（{exc}）"
            row["anomalies"].append(msg)
            anomalies.append(msg)
            results.append(row)
            continue

        row["latest_idx"] = latest
        row["unread"] = unread
        if latest is not None and latest > cp_idx:
            msg = (
                f"{db_path}: Antigravity idx 落后（checkpoint={cp_idx}, latest={latest}, "
                f"未消费 {unread} 行）"
            )
            row["anomalies"].append(msg)
            anomalies.append(msg)
        results.append(row)
    return results, anomalies


# ---------------------------------------------------------------- 各引擎最新时刻

def engine_latest(conn: sqlite3.Connection) -> list[tuple[str, int, int]]:
    if not table_exists(conn, "usage_records"):
        return []
    return [
        (row[0], row[1], row[2])
        for row in conn.execute(
            f"SELECT {ENGINE_CASE_SQL} AS engine, COUNT(*), MAX(timestamp_ms) "
            "FROM usage_records GROUP BY engine ORDER BY engine"
        )
    ]


# ---------------------------------------------------------------- 深检解析

_TRIM = b" \t\n\r\x0c"


def _trim(line: bytes) -> bytes:
    return line.strip(_TRIM)


_ISO_RE = re.compile(
    r"^(\d{4})-(\d{2})-(\d{2})[T ](\d{2}):(\d{2}):(\d{2})"
    r"(?:\.(\d+))?([Zz]|[+-]\d{2}:\d{2})"
)


def parse_iso_ms(text: str) -> int | None:
    if not isinstance(text, str):
        return None
    match = _ISO_RE.match(text)
    if not match:
        return None
    year, month, day, hour, minute, second = (int(g) for g in match.groups()[:6])
    fraction, offset = match.group(7), match.group(8)
    millis = int((fraction or "0")[:3].ljust(3, "0"))
    try:
        base = _dt.datetime(year, month, day, hour, minute, second)
    except ValueError:
        return None
    base_ms = int(base.replace(tzinfo=_dt.timezone.utc).timestamp() * 1000) + millis
    if offset in ("Z", "z"):
        return base_ms
    sign = 1 if offset[0] == "+" else -1
    delta_min = int(offset[1:3]) * 60 + int(offset[4:6])
    return base_ms - sign * delta_min * 60_000


def parse_claude_line(line: bytes):
    """复刻 parse_line 的记录判定；返回 (record_id, session_id, values) 或 None。"""
    line = _trim(line)
    if not line:
        return None
    try:
        obj = json.loads(line)
    except Exception:
        return None
    if not isinstance(obj, dict) or obj.get("type") != "assistant":
        return None
    message = obj.get("message")
    if not isinstance(message, dict):
        return None
    usage = message.get("usage")
    if usage is None or not isinstance(usage, dict):
        return None
    uuid = obj.get("uuid")
    if not isinstance(uuid, str) or not uuid:
        return None

    record_id = None
    mid = message.get("id")
    if isinstance(mid, str) and mid:
        record_id = mid
    if record_id is None:
        ref = obj.get("requestRef")
        if isinstance(ref, str) and ref:
            record_id = ref
    if record_id is None:
        record_id = uuid

    session_id = obj.get("session_id")
    if not isinstance(session_id, str):
        session_id = obj.get("sessionId")
    if not isinstance(session_id, str):
        return None
    if parse_iso_ms(obj.get("timestamp")) is None:
        return None

    keys = (
        "input_tokens",
        "output_tokens",
        "cache_read_input_tokens",
        "cache_creation_input_tokens",
    )
    values = [0, 0, 0, 0]
    present = 0
    for index, key in enumerate(keys):
        if key in usage:
            present += 1
            value = _token_value(usage[key])
            if value is None:
                return None
            values[index] = value
    if present == 0:
        return None
    return record_id, session_id, tuple(values)


def _omp_token(value) -> int:
    token = _token_value(value)
    return token if token is not None else 0


def parse_omp_line(line: bytes, session_id: str):
    """复刻 parse_omp_line；返回 (record_id, values) 或 None。"""
    line = _trim(line)
    if not line:
        return None
    try:
        obj = json.loads(line)
    except Exception:
        return None
    if not isinstance(obj, dict) or obj.get("type") != "message":
        return None
    message = obj.get("message")
    if not isinstance(message, dict) or message.get("role") != "assistant":
        return None
    usage = message.get("usage")
    if not isinstance(usage, dict):
        return None

    values = (
        _omp_token(usage.get("input")),
        _omp_token(usage.get("output")),
        _omp_token(usage.get("cacheRead")),
        _omp_token(usage.get("cacheWrite")),
    )
    if values == (0, 0, 0, 0):
        return None

    msg_id = obj.get("id")
    if isinstance(msg_id, str) and msg_id:
        id_part = msg_id
    else:
        id_part = hashlib.sha256(line).hexdigest()[:8]
    return f"omp:{session_id}:{id_part}", values


def deep_check_engine(
    engine: str,
    files: list[str],
    parse_line,
    app_db: str,
    current_ms: int,
) -> dict:
    expected: dict[str, tuple[tuple[int, int, int, int], str, str, int, int]] = {}
    sessions: set[str] = set()
    usage_lines = 0

    for path in files:
        mtime_ms, _size = file_stat_ms(path)
        stem = os.path.basename(path)
        if stem.endswith(".jsonl"):
            stem = stem[:-6]
        try:
            handle = open(path, "rb")
        except OSError:
            continue
        with handle:
            for line_no, raw in enumerate(handle, 1):
                if engine == "omp":
                    parsed = parse_line(raw, stem)
                    if parsed is None:
                        continue
                    record_id, values = parsed
                    session_id = stem
                else:
                    parsed = parse_line(raw)
                    if parsed is None:
                        continue
                    record_id, session_id, values = parsed
                usage_lines += 1
                sessions.add(session_id)
                expected[record_id] = (values, session_id, path, line_no, mtime_ms)

    db_rows: dict[str, tuple[int, int, int, int]] = {}
    conn = connect_ro(app_db)
    try:
        if table_exists(conn, "usage_records"):
            ids = list(expected)
            for start in range(0, len(ids), 500):
                chunk = ids[start:start + 500]
                placeholders = ",".join("?" * len(chunk))
                for row in conn.execute(
                    "SELECT record_id, input_tokens, output_tokens, "
                    "cache_read_tokens, cache_creation_tokens "
                    f"FROM usage_records WHERE record_id IN ({placeholders})",
                    chunk,
                ):
                    db_rows[row[0]] = (row[1], row[2], row[3], row[4])

        missing, mismatch = [], []
        for record_id, (values, _session, path, line_no, mtime_ms) in expected.items():
            if record_id not in db_rows:
                missing.append((record_id, path, line_no, values, mtime_ms))
            elif db_rows[record_id] != values:
                mismatch.append((record_id, db_rows[record_id], values, path, line_no))

        extras: list[str] = []
        if sessions and table_exists(conn, "usage_records"):
            session_list = list(sessions)
            for start in range(0, len(session_list), 400):
                chunk = session_list[start:start + 400]
                placeholders = ",".join("?" * len(chunk))
                if engine == "omp":
                    sql = (
                        "SELECT record_id FROM usage_records "
                        f"WHERE session_id IN ({placeholders}) AND record_id LIKE 'omp:%'"
                    )
                else:
                    sql = (
                        "SELECT record_id FROM usage_records "
                        f"WHERE session_id IN ({placeholders})"
                    )
                for row in conn.execute(sql, chunk):
                    if row[0] not in expected:
                        extras.append(row[0])
    finally:
        conn.close()

    allowed = [item for item in missing if item[0] in KNOWN_CASUALTIES]
    unresolved_missing = [item for item in missing if item[0] not in KNOWN_CASUALTIES]

    # 文件 60s 内仍在写 → 该文件的缺行/差值属于正常写入窗口，不计异常。
    def in_flight(mtime_ms: int) -> bool:
        return (current_ms - mtime_ms) / 1000.0 <= STALE_SECONDS

    anomalies: list[str] = []
    pending = 0
    for record_id, path, line_no, _values, mtime_ms in unresolved_missing:
        if in_flight(mtime_ms):
            pending += 1
            continue
        anomalies.append(f"缺行 {record_id} @ {path}:{line_no}")
    for record_id, db_values, values, path, line_no in mismatch:
        mtime_ms = expected[record_id][4]
        if in_flight(mtime_ms):
            pending += 1
            continue
        anomalies.append(
            f"值不一致 {record_id} @ {path}:{line_no} DB={db_values} 文件={values}"
        )
    for record_id in extras:
        anomalies.append(f"多行 {record_id}（DB 有记录、源文件未找到）")

    return {
        "engine": engine,
        "files": len(files),
        "usage_lines": usage_lines,
        "records": len(expected),
        "missing": unresolved_missing,
        "allowed": allowed,
        "mismatch": mismatch,
        "extras": extras,
        "pending": pending,
        "anomalies": anomalies,
    }


# ---------------------------------------------------------------- 报告

def print_quick_report(
    jsonl_rows: list[dict],
    record_rows: list[dict],
    opencode_result: dict,
    antigravity_rows: list[dict],
) -> None:
    print("\n[快检] JSONL 未消费增量（size vs checkpoint.byte_offset）")
    table = []
    for row in jsonl_rows:
        status = "FAIL" if row["anomalies"] else "PASS"
        note = []
        if row["trailing"]:
            note.append(f"{row['trailing']} 个文件 60s 内写入中")
        if row["no_checkpoint"]:
            note.append(f"{row['no_checkpoint']} 个文件无 checkpoint")
        if row["truncated"]:
            note.append(f"{row['truncated']} 个文件截断待重扫")
        table.append([
            row["engine"],
            str(row["files"]),
            fmt_bytes(row["unread"]) if row["unread"] else "0",
            str(row["trailing"]),
            str(len(row["anomalies"])),
            status,
            "；".join(note),
        ])
    print_table(
        ["引擎", "文件数", "未消费", "尾随中", "异常", "状态", "说明"], table
    )

    print("\n[快检] 记录数型 (mtime,size) 对比")
    table = []
    for row in record_rows:
        status = "FAIL" if row["anomalies"] else "PASS"
        table.append([
            row["engine"],
            str(row["files"]),
            str(row["changed"]),
            str(len(row["anomalies"])),
            status,
            "无 checkpoint" if row["no_checkpoint"] and not row["anomalies"] else "",
        ])
    print_table(["引擎", "文件数", "变更未读", "异常", "状态", "说明"], table)

    row = opencode_result
    if row["files"] == 0:
        print("\n[快检] OpenCode 游标：源库不存在（跳过）")
    else:
        print("\n[快检] OpenCode 游标（time_created >= checkpoint 边界口径）")
        print(f"  源库: {row.get('path', '') or ''}")
        print(
            f"  checkpoint: {row['checkpoint']} ({fmt_ms(row['checkpoint']) if row['checkpoint'] else '-'})"
        )
        print(
            f"  最新 time_created: {row['latest']} ({fmt_ms(row['latest'])})"
            f"；未消费行(> checkpoint): {row['unread']}"
        )
        print(
            f"  边界行(== checkpoint): {row['boundary']}；其中未入账: {row['boundary_unread']}"
        )
        print(f"  主库 stat: {row['main_stat']}；WAL: {row['wal']}")
        print(f"  状态: {'FAIL' if row['anomalies'] else 'PASS'}")

    if antigravity_rows:
        print("\n[快检] Antigravity gen_metadata idx")
        table = []
        for row in antigravity_rows:
            table.append([
                row["db"],
                str(row["checkpoint_idx"]) if row["checkpoint_idx"] is not None else "-",
                str(row["latest_idx"]) if row["latest_idx"] is not None else "-",
                str(row["unread"]),
                row["main_stat"],
                row["wal"],
                "FAIL" if row["anomalies"] else "PASS",
            ])
        print_table(
            ["数据库", "checkpoint idx", "最新 idx", "未消费", "主库 stat", "WAL", "状态"],
            table,
        )


def print_deep_report(results: list[dict]) -> None:
    print("\n[深检] 行级对账（v1: omp + Claude Code）")
    table = []
    for row in results:
        status = "FAIL" if row["anomalies"] else "PASS"
        table.append([
            row["engine"],
            str(row["files"]),
            str(row["usage_lines"]),
            str(row["records"]),
            str(len(row["missing"])),
            str(len(row["extras"])),
            str(len(row["mismatch"])),
            str(len(row["allowed"])),
            status,
        ])
    print_table(
        ["引擎", "文件数", "usage 行", "去重记录", "缺行", "多行", "值不一致", "allowlist", "状态"],
        table,
    )
    for row in results:
        for record_id, path, line_no, _values, _mtime in row["missing"][:10]:
            print(f"  缺行 {row['engine']}: {record_id} @ {path}:{line_no}")
        for record_id, db_values, values, path, line_no in row["mismatch"][:10]:
            print(
                f"  值不一致 {row['engine']}: {record_id} @ {path}:{line_no} "
                f"DB={db_values} 文件={values}"
            )
        for record_id in row["extras"][:10]:
            print(f"  多行 {row['engine']}: {record_id}")
        for record_id, _path, _line, _values, _mtime in row["allowed"]:
            print(
                f"  allowlist 吸收 {row['engine']}: {record_id} "
                f"— {KNOWN_CASUALTIES[record_id]}"
            )
        if row["pending"]:
            print(f"  {row['pending']} 条差异在 60s 写入窗口内，未计异常")


def print_engine_latest(conn: sqlite3.Connection) -> None:
    print("\n[DB] 各引擎最新记录时刻")
    rows = engine_latest(conn)
    if not rows:
        print("  usage_records 表不存在或没有记录")
        return
    table = [[engine, str(count), fmt_ms(latest)] for engine, count, latest in rows]
    print_table(["引擎", "记录数", "最新记录时间"], table)


# ---------------------------------------------------------------- 入口

def main() -> int:
    parser = argparse.ArgumentParser(
        description="token-watcher 对账工具：快检 checkpoint 未消费增量；--deep 行级对账。",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=(
            "退出码：0 = PASS；1 = 存在异常；2 = 无法运行。\n"
            "10-21 已知战损以脚本内 KNOWN_CASUALTIES 常量标注（仅吸收深检缺行）。"
        ),
    )
    parser.add_argument("--deep", action="store_true", help="行级对账（v1: omp + Claude Code）")
    parser.add_argument("--db", help="token-watcher SQLite 路径（默认 <home>/.token-watcher/db.sqlite3）")
    parser.add_argument("--home", help="用户 home 目录（默认 $HOME；fixture 测试用）")
    args = parser.parse_args()

    home = os.path.abspath(args.home or os.path.expanduser("~"))
    db_path = args.db or os.path.join(home, ".token-watcher", "db.sqlite3")
    current_ms = now_ms()

    print("token-watcher audit")
    print(f"  db:   {db_path}")
    print(f"  home: {home}")
    print(f"  时间: {fmt_ms(current_ms)}")

    if not os.path.isfile(db_path):
        print(f"错误：数据库不存在：{db_path}", file=sys.stderr)
        return 2
    try:
        checkpoints = load_checkpoints(db_path)
        app_conn = connect_ro(db_path)
    except Exception as exc:
        print(f"错误：无法读取数据库 {db_path}（{exc}）", file=sys.stderr)
        return 2

    try:
        paths = source_paths(home)
        anomalies: list[str] = []

        jsonl_rows = []
        for engine, root in paths["jsonl_roots"]:
            if not os.path.isdir(root):
                jsonl_rows.append({
                    "engine": engine, "files": 0, "unread": 0, "trailing": 0,
                    "no_checkpoint": 0, "truncated": 0, "anomalies": [],
                })
                continue
            row = check_jsonl_family(
                engine, walk_files(root, ".jsonl"), checkpoints, current_ms
            )
            jsonl_rows.append(row)
            anomalies.extend(row["anomalies"])

        aider = paths["aider"]
        if os.path.isfile(aider):
            row = check_jsonl_family("aider", [aider], checkpoints, current_ms)
            jsonl_rows.append(row)
            anomalies.extend(row["anomalies"])

        record_rows = []
        for engine, root, pattern in paths["record_roots"]:
            if not os.path.isdir(root):
                record_rows.append({
                    "engine": engine, "files": 0, "changed": 0,
                    "no_checkpoint": 0, "anomalies": [],
                })
                continue
            row = check_record_family(engine, root, pattern, checkpoints)
            record_rows.append(row)
            anomalies.extend(row["anomalies"])

        opencode_result, opencode_anomalies = check_opencode(
            paths["opencode_db"], db_path, checkpoints
        )
        anomalies.extend(opencode_anomalies)

        antigravity_rows, antigravity_anomalies = check_antigravity(
            paths["antigravity_root"], checkpoints
        )
        anomalies.extend(antigravity_anomalies)

        deep_results: list[dict] = []
        if args.deep:
            omp_files = walk_files(
                os.path.join(home, ".omp/agent/sessions"), ".jsonl"
            )
            deep_results.append(
                deep_check_engine("omp", omp_files, parse_omp_line, db_path, current_ms)
            )
            claude_files = walk_files(
                os.path.join(home, ".claude/projects"), ".jsonl"
            )
            deep_results.append(
                deep_check_engine("claude", claude_files, parse_claude_line, db_path, current_ms)
            )
            for row in deep_results:
                anomalies.extend(row["anomalies"])

        print_quick_report(jsonl_rows, record_rows, opencode_result, antigravity_rows)
        if deep_results:
            print_deep_report(deep_results)
        print_engine_latest(app_conn)

        print("\n[汇总]")
        if anomalies:
            print(f"  FAIL — {len(anomalies)} 个异常")
            for message in anomalies[:50]:
                print(f"  - {message}")
            if len(anomalies) > 50:
                print(f"  ... 另有 {len(anomalies) - 50} 条")
            return 1
        suffix = ""
        if args.deep:
            allowed = sum(len(row["allowed"]) for row in deep_results)
            suffix = "；深检 0 未解释差异"
            if allowed:
                suffix += f"（allowlist 吸收 {allowed} 条已知战损）"
        print(f"  PASS — 快检 0 异常{suffix}")
        return 0
    finally:
        app_conn.close()


if __name__ == "__main__":
    sys.exit(main())

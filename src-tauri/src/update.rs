//! GitHub Releases API 版本检测与缓存
//!
//! 规约：docs/designs/v0.6-feature-spec.md
//! - 请求：https://api.github.com/repos/johnhorse/token-watcher/releases/latest
//! - 超时：5 秒，带 User-Agent: token-watcher/{version}
//! - 语义化版本比对（major.minor.patch）
//! - 6 小时内存缓存

use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub const REPO_OWNER: &str = "johnhorse";
pub const REPO_NAME: &str = "token-watcher";
pub const CACHE_TTL: Duration = Duration::from_secs(6 * 3600);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UpdateInfo {
    pub has_update: bool,
    pub current_version: String,
    pub latest_version: String,
    pub release_url: String,
}

#[derive(Debug, Deserialize)]
struct GithubRelease {
    tag_name: String,
    html_url: String,
}

/// 解析语义化三段式版本号，如 "0.5.1" -> Some((0, 5, 1))
pub fn parse_semver(version: &str) -> Option<(u64, u64, u64)> {
    let clean = version.trim().trim_start_matches(['v', 'V']);
    let parts: Vec<&str> = clean.split('.').collect();
    if parts.len() < 3 {
        return None;
    }
    let major = parts[0].parse::<u64>().ok()?;
    let minor = parts[1].parse::<u64>().ok()?;
    // patch 字段可能包含预发布标识符，如 1-beta.1，取数字前缀
    let patch_part = parts[2].split('-').next().unwrap_or(parts[2]);
    let patch = patch_part.parse::<u64>().ok()?;
    Some((major, minor, patch))
}

/// 比对是否 latest > current
pub fn is_newer_version(current: &str, latest: &str) -> bool {
    match (parse_semver(current), parse_semver(latest)) {
        (Some((c_maj, c_min, c_pat)), Some((l_maj, l_min, l_pat))) => {
            (l_maj, l_min, l_pat) > (c_maj, c_min, c_pat)
        }
        _ => false,
    }
}

pub struct UpdateChecker {
    current_version: String,
    cache: Mutex<Option<(UpdateInfo, Instant)>>,
}

impl UpdateChecker {
    pub fn new(current_version: String) -> Self {
        Self {
            current_version,
            cache: Mutex::new(None),
        }
    }

    pub async fn check(&self) -> Result<UpdateInfo, String> {
        // 检查缓存
        if let Ok(guard) = self.cache.lock() {
            if let Some((info, timestamp)) = guard.as_ref() {
                if timestamp.elapsed() < CACHE_TTL {
                    return Ok(info.clone());
                }
            }
        }

        let url = format!("https://api.github.com/repos/{REPO_OWNER}/{REPO_NAME}/releases/latest");
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .map_err(|e| format!("创建 HTTP 客户端失败: {e}"))?;

        let user_agent = format!("token-watcher/{}", self.current_version);
        let resp = client
            .get(&url)
            .header("User-Agent", user_agent)
            .header("Accept", "application/vnd.github.v3+json")
            .send()
            .await
            .map_err(|e| format!("请求 GitHub Release 失败: {e}"))?;

        if !resp.status().is_success() {
            return Err(format!("GitHub API 返回非成功状态码: {}", resp.status()));
        }

        let release: GithubRelease = resp
            .json()
            .await
            .map_err(|e| format!("解析 GitHub Release 响应失败: {e}"))?;

        let latest_version = release
            .tag_name
            .trim()
            .trim_start_matches(['v', 'V'])
            .to_string();

        let has_update = is_newer_version(&self.current_version, &latest_version);

        let info = UpdateInfo {
            has_update,
            current_version: self.current_version.clone(),
            latest_version,
            release_url: release.html_url,
        };

        if let Ok(mut guard) = self.cache.lock() {
            *guard = Some((info.clone(), Instant::now()));
        }

        Ok(info)
    }
}

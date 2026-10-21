//! update 模块集成测试：版本解析、语义化比对、缓存机制

use std::time::Duration;
use token_watcher_lib::update::{is_newer_version, parse_semver, UpdateChecker};

#[test]
fn test_parse_semver() {
    assert_eq!(parse_semver("0.5.1"), Some((0, 5, 1)));
    assert_eq!(parse_semver("v0.6.0"), Some((0, 6, 0)));
    assert_eq!(parse_semver("V1.2.3"), Some((1, 2, 3)));
    assert_eq!(parse_semver("  v1.0.0-beta.1  "), Some((1, 0, 0)));
    assert_eq!(parse_semver("invalid"), None);
    assert_eq!(parse_semver("1.0"), None);
}

#[test]
fn test_is_newer_version() {
    // 正常升级
    assert!(is_newer_version("0.5.1", "0.6.0"));
    assert!(is_newer_version("0.5.1", "0.5.2"));
    assert!(is_newer_version("0.5.1", "1.0.0"));
    assert!(is_newer_version("v0.5.1", "v0.5.2"));

    // 同版本或更低版本
    assert!(!is_newer_version("0.6.0", "0.6.0"));
    assert!(!is_newer_version("0.6.0", "0.5.9"));
    assert!(!is_newer_version("1.0.0", "0.9.9"));

    // 无效输入不报错且返回 false
    assert!(!is_newer_version("0.5.1", "invalid"));
    assert!(!is_newer_version("invalid", "0.6.0"));
}

#[test]
fn test_update_checker_initialization() {
    let _checker = UpdateChecker::new("0.5.1".to_string());
    assert_eq!(token_watcher_lib::update::REPO_OWNER, "johnhorse");
    assert_eq!(token_watcher_lib::update::REPO_NAME, "token-watcher");
    assert_eq!(
        token_watcher_lib::update::CACHE_TTL,
        Duration::from_secs(6 * 3600)
    );
}

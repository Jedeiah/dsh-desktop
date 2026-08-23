//! npm registry 查询（dsh 版本发现）。仅依赖 std + serde_json + ureq（均已在依赖树）。

use std::cmp::Ordering;
use std::io::Read;
use std::time::Duration;

const PKG: &str = "@deepseek-ai/dsh";
// 默认使用国内镜像（npmmirror）：本产品面向国内用户最频繁的分发场景，
// 直接连 registry.npmjs.org 在部分网络下慢/超时；Registry 源可在壳页随时改回官方源。
const DEFAULT_REGISTRY: &str = "https://registry.npmmirror.com";

/// Canonical registry base URL (no trailing slash).
pub fn registry_url(registry: Option<&str>) -> String {
    match registry {
        Some(r) if !r.trim().is_empty() => r.trim_end_matches('/').to_string(),
        _ => DEFAULT_REGISTRY.to_string(),
    }
}

/// 校验版本号是否形如 semver：`数字.数字.数字`，可选 `-` 预发布段
/// （预发布段由非空字母数字段以 `.` 分隔）。用于安装/更新命令入口的
/// 白名单校验，防止任意字符串（如 npm 参数注入）进入安装流程。
pub fn valid_version(s: &str) -> bool {
    let s = s.trim();
    let (main, pre) = match s.split_once('-') {
        Some((m, p)) => (m, Some(p)),
        None => (s, None),
    };
    let nums: Vec<&str> = main.split('.').collect();
    if nums.len() != 3
        || nums
            .iter()
            .any(|n| n.is_empty() || !n.chars().all(|c| c.is_ascii_digit()))
    {
        return false;
    }
    if let Some(p) = pre {
        if p.is_empty()
            || p.split('.').any(|seg| {
                seg.is_empty() || !seg.chars().all(|c| c.is_ascii_alphanumeric())
            })
        {
            return false;
        }
    }
    true
}

/// Simple semver-ish comparator (handles `0.1.0-rc.6`).
pub fn cmp_versions(a: &str, b: &str) -> Ordering {
    let (am, asuf) = a.split_once('-').unwrap_or((a, ""));
    let (bm, bsuf) = b.split_once('-').unwrap_or((b, ""));
    let ap: Vec<u64> = am.split('.').filter_map(|s| s.parse().ok()).collect();
    let bp: Vec<u64> = bm.split('.').filter_map(|s| s.parse().ok()).collect();
    for (x, y) in ap.iter().zip(bp.iter()) {
        match x.cmp(y) {
            Ordering::Equal => {}
            o => return o,
        }
    }
    match ap.len().cmp(&bp.len()) {
        Ordering::Equal => {}
        o => return o,
    }
    // release (no suffix) > pre-release; pre-release suffix compared with
    // numeric segment awareness: rc.10 > rc.9 (string compare would say rc.9
    // > rc.10, which mis-orders real version lists)
    match (asuf.is_empty(), bsuf.is_empty()) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Greater,
        (false, true) => Ordering::Less,
        (false, false) => {
            let an: Vec<&str> = asuf.split('.').collect();
            let bn: Vec<&str> = bsuf.split('.').collect();
            for (x, y) in an.iter().zip(bn.iter()) {
                match (x.parse::<u64>(), y.parse::<u64>()) {
                    (Ok(a), Ok(b)) if a != b => return a.cmp(&b),
                    _ => match x.cmp(y) {
                        Ordering::Equal => {}
                        o => return o,
                    },
                }
            }
            an.len().cmp(&bn.len())
        }
    }
}

/// Parse `versions` keys from a full registry package document, newest first.
pub fn parse_versions(body: &str) -> Vec<String> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
        return vec![];
    };
    let Some(map) = v.get("versions").and_then(|x| x.as_object()) else {
        return vec![];
    };
    let mut vs: Vec<String> = map.keys().cloned().collect();
    vs.sort_by(|a, b| cmp_versions(b, a));
    vs
}

/// Query the registry for the `latest` dist-tag version.
pub fn latest_version(registry: &str) -> Result<String, String> {
    let url = format!("{registry}/{PKG}/latest");
    let resp = ureq::get(&url)
        .timeout(Duration::from_secs(20))
        .call()
        .map_err(|e| format!("查询 {url} 失败: {e}"))?;
    let mut body = String::new();
    resp.into_reader()
        .take(1 << 20)
        .read_to_string(&mut body)
        .map_err(|e| format!("读取 registry 响应失败: {e}"))?;
    let v: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| format!("解析 registry 响应失败: {e}"))?;
    v.get("version")
        .and_then(|x| x.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| "registry 响应缺少 version 字段".into())
}

/// List every published version of `@deepseek-ai/dsh`, newest first.
pub fn list_versions(registry: &str) -> Result<Vec<String>, String> {
    let url = format!("{registry}/{PKG}");
    let resp = ureq::get(&url)
        .timeout(Duration::from_secs(20))
        .call()
        .map_err(|e| format!("查询 {url} 失败: {e}"))?;
    let mut body = String::new();
    resp.into_reader()
        .take(4 << 20)
        .read_to_string(&mut body)
        .map_err(|e| format!("读取 registry 响应失败: {e}"))?;
    Ok(parse_versions(&body))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering;

    #[test]
    fn cmp_versions_orders_rc_and_release() {
        assert_eq!(cmp_versions("0.1.1-rc.2", "0.1.1-rc.1"), Ordering::Greater);
        assert_eq!(cmp_versions("0.1.0-rc.7", "0.1.1-rc.1"), Ordering::Less);
        assert_eq!(cmp_versions("0.1.0", "0.1.0-rc.1"), Ordering::Greater); // release > prerelease
        assert_eq!(cmp_versions("0.1.0-rc.10", "0.1.0-rc.9"), Ordering::Greater);
        assert_eq!(cmp_versions("0.1.0-rc.7", "0.1.0-rc.7"), Ordering::Equal);
    }

    #[test]
    fn parse_versions_sorts_desc() {
        let body = r#"{"versions":{
            "0.1.0-rc.2":{},"0.1.1-rc.1":{},"0.1.0-rc.6":{},
            "0.1.1-rc.2":{},"0.0.1-rc.1":{},"0.1.0":{}
        }}"#;
        assert_eq!(
            parse_versions(body),
            vec!["0.1.1-rc.2", "0.1.1-rc.1", "0.1.0", "0.1.0-rc.6", "0.1.0-rc.2", "0.0.1-rc.1"]
        );
    }

    #[test]
    fn parse_versions_tolerates_garbage() {
        assert!(parse_versions("not json").is_empty());
        assert!(parse_versions(r#"{"versions":null}"#).is_empty());
    }

    #[test]
    fn registry_url_normalizes() {
        assert_eq!(registry_url(None), "https://registry.npmmirror.com");
        assert_eq!(registry_url(Some("https://registry.npmmirror.com/")), "https://registry.npmmirror.com");
        assert_eq!(registry_url(Some("  ")), "https://registry.npmmirror.com");
    }

    #[test]
    fn valid_version_accepts_semver() {
        assert!(valid_version("0.1.1-rc.2"));
        assert!(valid_version("0.1.0"));
        assert!(valid_version("1.2.3"));
        assert!(valid_version("1.2.3-beta.1"));
        assert!(valid_version("0.1.0-rc1"));
    }

    #[test]
    fn valid_version_rejects_non_semver() {
        assert!(!valid_version("latest"));
        assert!(!valid_version(".."));
        assert!(!valid_version("1;rm -rf"));
        assert!(!valid_version(""));
        assert!(!valid_version("0.1"));
        assert!(!valid_version("0.1.0-"));
        assert!(!valid_version("0.1.0-rc."));
        assert!(!valid_version("0.1.0 rc"));
    }
}

//! npm registry 查询（dsh 版本发现）。仅依赖 std + serde_json + ureq（均已在依赖树）。

use std::cmp::Ordering;
use std::io::Read;
use std::time::Duration;

const PKG: &str = "@deepseek-ai/dsh";
// 默认用官方源 registry.npmjs.org（2026-09 起）：npm 官方源是事实标准，
// 索引语义、内容与 CDN 最稳。国内网络下若慢，用户可在壳页「Registry 源设置」换成镜像
// （如 https://registry.npmmirror.com），改动立即生效并持久化在 settings。
const DEFAULT_REGISTRY: &str = "https://registry.npmjs.org";

/// Canonical registry base URL (no trailing slash).
pub fn registry_url(registry: Option<&str>) -> String {
    match registry {
        Some(r) if !r.trim().is_empty() => r.trim_end_matches('/').to_string(),
        _ => DEFAULT_REGISTRY.to_string(),
    }
}

/// 版本号长度上限。前端两个版本输入框的 maxlength 取得更紧（32）：真实 dsh 版本
/// 形如 `0.1.2-alpha.5`（约 14 字符），32 足够手输；后端留到 64 是给「registry 返回的
/// 版本列表」留余量。UI 上限 ≤ 后端上限是安全方向：输入框能填的值后端一定接受。
pub const MAX_VERSION_LEN: usize = 64;

/// 校验版本号是否形如 semver：`数字.数字.数字`，可选 `-` 预发布段
/// （预发布段由非空字母数字段以 `.` 分隔）。用于安装/更新命令入口的
/// 白名单校验，防止任意字符串（如 npm 参数注入）进入安装流程。
/// 另限制总长度（MAX_VERSION_LEN）：结构白名单本身不限长，超长串会被拼进
/// registry URL，既无意义、也会把失败信息撑爆。
pub fn valid_version(s: &str) -> bool {
    let s = s.trim();
    if s.is_empty() || s.len() > MAX_VERSION_LEN {
        return false;
    }
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
/// 任意包的 latest（`dist-tags.latest`）：走 `<registry>/<包名>/latest`。
/// 供 dsh 自身更新检查（latest_version）与「插件更新检查」共用。
pub fn latest_version_of(registry: &str, pkg: &str) -> Result<String, String> {
    let url = format!("{registry}/{pkg}/latest");
    let resp = ureq::get(&url)
        .timeout(Duration::from_secs(20))
        .call()
        .map_err(|e| crate::i18n::tr_args("err_query_failed", &[("url", &url), ("e", &e.to_string())]))?;
    let mut body = String::new();
    resp.into_reader()
        .take(1 << 20)
        .read_to_string(&mut body)
        .map_err(|e| crate::i18n::tr_args("err_read_registry_response", &[("e", &e.to_string())]))?;
    let v: serde_json::Value =
        serde_json::from_str(&body)
            .map_err(|e| crate::i18n::tr_args("err_parse_registry_response", &[("e", &e.to_string())]))?;
    v.get("version")
        .and_then(|x| x.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| crate::i18n::tr("err_registry_no_version_field"))
}

pub fn latest_version(registry: &str) -> Result<String, String> {
    latest_version_of(registry, PKG)
}

/// 200 响应体是否表示「版本存在」。部分 registry 镜像对不存在的版本返回
/// 200 + `{"error": ...}`，仅按状态码会误判为存在，需按 body 判断。
/// 判断依据是 JSON **顶层**有没有 `error` 字段，不做子串匹配：版本元数据里出现
/// 字面量 `"error"`（scripts 里有个叫 error 的项、依赖名或描述命中）时，子串匹配
/// 会把确实存在的版本判成不存在，用户看到「版本不存在」而装不上。
fn version_exists_response(body: &str) -> bool {
    match serde_json::from_str::<serde_json::Value>(body) {
        Ok(v) => v.get("error").is_none(),
        // 解析不了（HTML 错误页/空 body）：保守判为存在，让后续真实下载给出准确报错
        Err(_) => true,
    }
}

/// 查询指定版本是否存在于 registry（`GET {registry}/{pkg}/{ver}`：200=存在，
/// 404=不存在，其它状态/网络错误视为 Err）。供「输入版本号安装」在下载前校验，
/// 避免用户输入不存在的版本白白触发一次大下载。
pub fn version_exists(registry: &str, ver: &str) -> Result<bool, String> {
    let url = format!("{registry}/{PKG}/{ver}");
    match ureq::get(&url).timeout(Duration::from_secs(20)).call() {
        Ok(resp) => {
            let mut body = String::new();
            resp.into_reader()
                .take(16 << 20)
                .read_to_string(&mut body)
                .map_err(|e| crate::i18n::tr_args("err_read_registry_response", &[("e", &e.to_string())]))?;
            Ok(version_exists_response(&body))
        }
        Err(ureq::Error::Status(404, _)) => Ok(false),
        Err(e) => Err(crate::i18n::tr_args(
            "err_query_version_existence",
            &[("e", &e.to_string())],
        )),
    }
}

/// List every published version of `@deepseek-ai/dsh`, newest first.
/// 为支持「搜索全部版本」需完整文档；上限放宽到 16MB（原 4MB 在版本非常多时
/// 会被截断 → parse_versions 静默丢版本/为空）。截断仍是兜底,超限会解析失败返回空,
/// 前端提示"未获取到可用版本"。
pub fn list_versions(registry: &str) -> Result<Vec<String>, String> {
    let url = format!("{registry}/{PKG}");
    let resp = ureq::get(&url)
        .timeout(Duration::from_secs(20))
        .call()
        .map_err(|e| crate::i18n::tr_args("err_query_failed", &[("url", &url), ("e", &e.to_string())]))?;
    let mut body = String::new();
    resp.into_reader()
        .take(16 << 20)
        .read_to_string(&mut body)
        .map_err(|e| crate::i18n::tr_args("err_read_registry_response", &[("e", &e.to_string())]))?;
    Ok(parse_versions(&body))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering;

    #[test]
    fn version_exists_response_detects_error_body() {
        // 正常 200：版本元数据 JSON，无 error 字段 → 存在
        assert!(version_exists_response(r#"{"name":"@deepseek-ai/dsh","version":"0.1.0"}"#));
        // 部分 registry 镜像对不存在的版本返回 200 + {"error": ...} → 不存在
        assert!(!version_exists_response(r#"{"error":"Not found"}"#));
        assert!(!version_exists_response(r#"{"error":"version not found: 9.9.9"}"#));
        // 顶层 error 之外出现字面量 "error" 不能误判：版本元数据里带 error 字样的
        // 字段/脚本很常见（旧实现用 body.contains("\"error\"")，会把存在的版本判成不存在）
        assert!(version_exists_response(
            r#"{"name":"@deepseek-ai/dsh","version":"0.1.0","scripts":{"error":"echo x"},"description":"handles \"error\" cases"}"#
        ));
        // 解析不了的 body：保守放行（后续真实下载会给出准确报错）
        assert!(version_exists_response("<html>404</html>"));
        assert!(version_exists_response(""));
    }

    #[test]
    fn valid_version_accepts_semver_and_caps_length() {
        // 正常形态
        assert!(valid_version("0.1.1"));
        assert!(valid_version("0.1.1-rc.2"));
        assert!(valid_version("0.1.2-alpha.5"));
        assert!(valid_version("  1.2.3  ")); // 前后空白先 trim
        // 结构不合法
        assert!(!valid_version("0.1"));
        assert!(!valid_version("v0.1.1"));
        assert!(!valid_version("0.1.1-"));
        assert!(!valid_version("0.1.1-rc..2"));
        assert!(!valid_version("1.2.3; rm -rf /"));
        // 长度上限：结构合法但过长（前端输入框 maxlength 同值，正常途径填不出来）
        let long_pre = "a".repeat(MAX_VERSION_LEN); // 3+2+1+64 > 64
        assert!(!valid_version(&format!("1.2.3-{long_pre}")));
        assert!(valid_version(&format!("1.2.3-{}", "a".repeat(MAX_VERSION_LEN - 6))));
    }

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
        assert_eq!(registry_url(None), "https://registry.npmjs.org");
        assert_eq!(registry_url(Some("https://registry.npmmirror.com/")), "https://registry.npmmirror.com");
        assert_eq!(registry_url(Some("  ")), "https://registry.npmjs.org");
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

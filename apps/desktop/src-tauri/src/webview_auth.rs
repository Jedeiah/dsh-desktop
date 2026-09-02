//! dsh 工作台 iframe 认证桥。
//!
//! dsh 0.1.2-alpha.5 起 index 认证 =「?token= → 303 + Set-Cookie（HttpOnly;
//! SameSite=Strict）」会话制（见 @deepseek-ai/dsh-client-connection
//! authorizeIndex）。壳页在 tauri://localhost，iframe 加载 http://127.0.0.1
//! 属**跨站上下文**：
//!   - SameSite=Strict 的 cookie 不随跨站 iframe 请求发送；
//!   - WKWebView/WebView2 的第三方 cookie 策略会丢弃跨站响应里的 Set-Cookie。
//!
//! 结果：iframe 永远 401（工作台显示 "dsh web authentication required;
//! reopen the URL printed by dsh web."）。
//!
//! 修复：壳在 dsh 就绪（stdout 解析出启动 URL）后，主动用带 token 的 URL 完成
//! 一次认证，捕获 303 下发的会话 cookie，**改写 SameSite=None + Secure 后注入
//! WebView 的 cookie store**——127.0.0.1 是潜在可信源（Secure 豁免成立），
//! iframe 跨站请求即可携带该 cookie 通过认证。dsh 每次启动 token/端口都可能
//! 变化（cookie 名含 authority 哈希），故每次就绪都重新预认证注入。

use std::time::Duration;

/// 用带 token 的启动 URL 完成 dsh 会话认证，取回会话 cookie（name=value）。
/// dsh：GET /?token=… → 303 Location:/ + Set-Cookie（HttpOnly; SameSite=Strict）。
/// ureq 需关闭自动重定向才能读到 303 响应上的 Set-Cookie。
pub fn fetch_session_cookie(token_url: &str) -> Result<(String, String), String> {
    let agent = ureq::AgentBuilder::new()
        .redirects(0)
        .timeout(Duration::from_secs(10))
        .build();
    let resp = match agent.get(token_url).call() {
        Ok(_resp) => {
            // 无 token 直放行的环境（200）：无需注入，调用方容忍后跳过
            return Err("工作台无需 token 认证（200）".into());
        }
        Err(ureq::Error::Status(303, resp)) => resp,
        Err(e) => return Err(format!("dsh 认证请求失败: {e}")),
    };
    let cookie = resp
        .header("set-cookie")
        .ok_or_else(|| "认证响应无 Set-Cookie".to_string())?;
    let (name, value) = cookie
        .split_once('=')
        .ok_or_else(|| format!("Set-Cookie 格式异常: {cookie}"))?;
    Ok((
        name.trim().to_string(),
        value.split(';').next().unwrap_or("").trim().to_string(),
    ))
}

/// dsh 就绪后：预认证 + 注入 WebView cookie store。全程失败容忍——
/// 注入失败仅静默跳过，不阻断启动（浏览器顶层打开仍是兜底路径）。
pub fn preauth_and_inject(app: &tauri::AppHandle, token_url: &str) {
    let Ok((name, value)) = fetch_session_cookie(token_url) else {
        return;
    };
    let app2 = app.clone();
    let _ = app2.clone().run_on_main_thread(move || {
        let _ = inject_into_window(&app2, name, value);
    });
}

fn inject_into_window(app: &tauri::AppHandle, name: String, value: String) -> Result<(), String> {
    use tauri::Manager;
    let Some(window) = app.get_webview_window(crate::WINDOW_LABEL) else {
        return Err("主窗不存在，跳过 cookie 注入".into());
    };
    #[cfg(target_os = "macos")]
    {
        macos::inject_macos(&window, name, value)
    }
    #[cfg(target_os = "windows")]
    {
        windows::inject_windows(&window, name, value)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = (&window, name, value);
        Err("不支持的平台".into())
    }
}

/// macOS：WKHTTPCookieStore 注入。domain=127.0.0.1 的 SameSite=None cookie
/// 在跨站 iframe 请求中携带（ITP 对 loopback 豁免；Secure 对潜在可信源豁免）。
/// tauri 的 with_webview 闭包要求 Send + 'static，返回值经 channel 回传。
#[cfg(target_os = "macos")]
pub mod macos {
    use super::*;
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    use objc2_foundation::{NSMutableDictionary, NSString};
    use tauri::WebviewWindow;

    pub fn inject_macos(window: &WebviewWindow, name: String, value: String) -> Result<(), String> {
        let (tx, rx) = std::sync::mpsc::channel::<Result<(), String>>();
        let _ = window.with_webview(move |webview| {
            let r = (|| -> Result<(), String> {
                // PlatformWebview.inner(): *mut c_void（WKWebView）
                let wk = webview.inner() as *mut AnyObject;
                let store_ptr: *mut AnyObject = unsafe { msg_send![wk, cookieStore] };
                let store =
                    unsafe { Retained::retain(store_ptr) }.ok_or("cookieStore 不可用")?;
                let cookie = build_cookie(&name, &value)?;
                // setCookie:completionHandler:（异步）；block 持住 store/cookie 防提前释放
                let handler = {
                    let store = store.clone();
                    let cookie = cookie.clone();
                    block2::RcBlock::new(move || {
                        let _ = (&store, &cookie);
                    })
                };
                unsafe {
                    msg_send![&*store, setCookie:&*cookie, completionHandler:&*handler]
                }
                Ok(())
            })();
            let _ = tx.send(r);
        });
        rx.recv_timeout(Duration::from_secs(3))
            .map_err(|e| format!("cookie 注入超时/失败: {e}"))?
    }

    fn build_cookie(name: &str, value: &str) -> Result<Retained<AnyObject>, String> {
        unsafe {
            let dict = NSMutableDictionary::<AnyObject, AnyObject>::new();
            let set = |k: &str, v: &str| {
                let key = NSString::from_str(k);
                let val = NSString::from_str(v);
                let _: () = msg_send![&*dict, setObject:&*val, forKey:&*key];
            };
            set("NSHTTPCookieName", name);
            set("NSHTTPCookieValue", value);
            set("NSHTTPCookieDomain", "127.0.0.1");
            set("NSHTTPCookiePath", "/");
            set("NSHTTPCookieSecure", "TRUE");
            set("NSHTTPCookieSameSitePolicy", "None"); // 跨站 iframe 必须 None
            let cls = class!(NSHTTPCookie);
            let cookie: *mut AnyObject = msg_send![cls, cookieWithProperties:&*dict];
            if cookie.is_null() {
                return Err("NSHTTPCookie 构造失败".into());
            }
            Ok(Retained::retain(cookie).ok_or("NSHTTPCookie retain 失败")?)
        }
    }
}

/// Windows：CoreWebView2CookieManager 注入（SameSite=None + Secure，
/// 127.0.0.1 潜在可信源，http 传输下 Chromium 仍接受并携带）。
#[cfg(target_os = "windows")]
pub mod windows {
    use super::*;
    use tauri::WebviewWindow;
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        COREWEBVIEW2_COOKIE_SAME_SITE_KIND_NONE, ICoreWebView2, ICoreWebView2_4,
        ICoreWebView2Controller,
    };
    use windows::core::{Interface, PCWSTR};

    pub fn inject_windows(
        window: &WebviewWindow,
        name: String,
        value: String,
    ) -> Result<(), String> {
        let (tx, rx) = std::sync::mpsc::channel::<Result<(), String>>();
        let _ = window.with_webview(move |webview| {
            let r = (|| -> Result<(), String> {
                let controller: &ICoreWebView2Controller = webview.controller();
                let core: ICoreWebView2 = unsafe {
                    controller
                        .CoreWebView2()
                        .map_err(|e| format!("CoreWebView2: {e}"))?
                };
                // CookieManager 位于版本化接口（webview2-com 去 Get 前缀命名），
                // 基类→版本接口用 cast（QueryInterface）
                let core4: ICoreWebView2_4 = core
                    .cast()
                    .map_err(|e| format!("cast ICoreWebView2_4: {e}"))?;
                let mgr = unsafe {
                    core4
                        .CookieManager()
                        .map_err(|e| format!("CookieManager: {e}"))?
                };
                // CreateCookie 收 PCWSTR（宽字符串），Vec<u16> 需存活到调用结束
                let to_wide =
                    |s: &str| -> Vec<u16> { s.encode_utf16().chain(std::iter::once(0)).collect() };
                let (name_w, value_w, domain_w, path_w) = (
                    to_wide(&name),
                    to_wide(&value),
                    to_wide("127.0.0.1"),
                    to_wide("/"),
                );
                let cookie = unsafe {
                    mgr.CreateCookie(
                        PCWSTR(name_w.as_ptr()),
                        PCWSTR(value_w.as_ptr()),
                        PCWSTR(domain_w.as_ptr()),
                        PCWSTR(path_w.as_ptr()),
                    )
                    .map_err(|e| format!("CreateCookie: {e}"))?
                };
                unsafe {
                    cookie
                        .SetSameSite(COREWEBVIEW2_COOKIE_SAME_SITE_KIND_NONE)
                        .map_err(|e| format!("SetSameSite: {e}"))?;
                    cookie
                        .SetIsSecure(true)
                        .map_err(|e| format!("SetIsSecure: {e}"))?;
                    mgr.AddOrUpdateCookie(&cookie)
                        .map_err(|e| format!("AddOrUpdateCookie: {e}"))?;
                }
                Ok(())
            })();
            let _ = tx.send(r);
        });
        rx.recv_timeout(Duration::from_secs(3))
            .map_err(|e| format!("cookie 注入超时/失败: {e}"))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cookie_parser_extracts_name_value() {
        // 直接测解析逻辑依赖的格式约定
        let raw = "dsh_session_abc=eyJ2IjoiMSJ9; Max-Age=604800; Path=/; Expires=Wed, 09 Sep 2026 00:00:00 GMT; HttpOnly; SameSite=Strict";
        let (k, v) = raw.split_once('=').unwrap();
        assert_eq!(k, "dsh_session_abc");
        assert_eq!(v.split(';').next().unwrap().trim(), "eyJ2IjoiMSJ9");
    }

    #[test]
    fn fetch_rejects_unreachable() {
        // 不联网：连接被拒 → 返回 Err 且消息非空
        let err = fetch_session_cookie("http://127.0.0.1:1/").unwrap_err();
        assert!(!err.is_empty());
    }
}
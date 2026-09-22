// 自绘弹窗（modal.html）：读取 Rust 侧 modal_spec 内容，渲染标题/消息/按钮，
// 点击后调用 modal_respond 回传结果并关闭。弹窗 ESC 关闭（危险操作页禁用此处）。
(() => {
  'use strict';
  const title = document.getElementById('title');
  const message = document.getElementById('message');
  const btnOk = document.getElementById('btnOk');
  const btnNo = document.getElementById('btnNo');
  const closeX = document.getElementById('closeX');

  // 弹窗是独立窗口，拿不到 shell state：按系统语言选语种（具体文案仍由 Rust 的 modal_spec 提供）
  // 注入的持久化偏好优先（"auto" 或缺失时回退浏览器语言）
  DSH_I18N.setLocale(window.__DSH_LOCALE__ && window.__DSH_LOCALE__ !== 'auto'
    ? window.__DSH_LOCALE__ : navigator.language);

  let spec = null;
  let responded = false;

  function respond(accept) {
    if (responded) return;
    responded = true;
    window.__TAURI__.core.invoke('modal_respond', { accept }).catch(() => {});
    // Rust 侧会 close 窗口；这里兜底延迟关闭
    setTimeout(() => window.close(), 120);
  }

  async function render() {
    try {
      spec = await window.__TAURI__.core.invoke('modal_spec');
    } catch (e) {
      title.textContent = 'DeepSeek Harness Desktop';
      message.textContent = DSH_I18N.t('modal_fetch_failed') + (e.message || e);
      btnOk.textContent = DSH_I18N.t('ok');
      return;
    }
    title.textContent = spec.title || 'DeepSeek Harness Desktop';
    message.textContent = spec.message || '';
    // 按钮文案：优先 spec 自定义（ok_label/no_label），回退默认「确定」/「稍后」
    btnOk.textContent = spec.ok_label || DSH_I18N.t('ok');
    if (spec.kind === 'yesno') {
      btnNo.hidden = false;
      btnNo.textContent = spec.no_label || DSH_I18N.t('later');
    } else {
      btnNo.hidden = true;
    }
  }

  // ✕ / Esc 的返回值：yesno（危险操作，如「卸载并清除数据」）一律 false = 取消；普通提示窗
  // true = 等同于「确定」。**spec 未就绪（modal_spec 还没 resolve 或失败）也必须 false**：
  // 旧写法 `spec && spec.kind === 'yesno' ? false : true` 在 spec 为 null 时给 true ——
  // 弹窗刚打开还没渲染完就按 Esc/✕，对卸载确认等于直接确认卸载并删数据。Rust 侧契约
  // 相反（show_modal_with_labels 阻塞等待用户点击，超时兜底 false）。
  const dismissAccept = () => (spec ? spec.kind !== 'yesno' : false);
  btnOk.addEventListener('click', () => respond(true));
  btnNo.addEventListener('click', () => respond(false));
  closeX.addEventListener('click', () => respond(dismissAccept()));
  // ESC：非危险操作页可关闭；yesno 视为取消
  window.addEventListener('keydown', (e) => {
    if (e.key === 'Escape') respond(dismissAccept());
  });

  render();
})();

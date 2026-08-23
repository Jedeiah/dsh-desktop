// 自绘弹窗（modal.html）：读取 Rust 侧 modal_spec 内容，渲染标题/消息/按钮，
// 点击后调用 modal_respond 回传结果并关闭。弹窗 ESC 关闭（危险操作页禁用此处）。
(() => {
  'use strict';
  const title = document.getElementById('title');
  const message = document.getElementById('message');
  const btnOk = document.getElementById('btnOk');
  const btnNo = document.getElementById('btnNo');
  const closeX = document.getElementById('closeX');

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
      title.textContent = 'DeepSeek Harness';
      message.textContent = '无法获取提示内容：' + (e.message || e);
      btnOk.textContent = '确定';
      return;
    }
    title.textContent = spec.title || 'DeepSeek Harness';
    message.textContent = spec.message || '';
    // 按钮文案：优先 spec 自定义（ok_label/no_label），回退默认「确定」/「稍后」
    btnOk.textContent = spec.ok_label || '确定';
    if (spec.kind === 'yesno') {
      btnNo.hidden = false;
      btnNo.textContent = spec.no_label || '稍后';
    } else {
      btnNo.hidden = true;
    }
  }

  btnOk.addEventListener('click', () => respond(true));
  btnNo.addEventListener('click', () => respond(false));
  closeX.addEventListener('click', () => respond(spec && spec.kind === 'yesno' ? false : true));
  // ESC：非危险操作页可关闭；yesno 视为取消
  window.addEventListener('keydown', (e) => {
    if (e.key === 'Escape') respond(spec && spec.kind === 'yesno' ? false : true);
  });

  render();
})();

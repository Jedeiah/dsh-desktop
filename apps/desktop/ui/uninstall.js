// 卸载确认：三选一（取消 / 保留 ~/.dsh / 一并删除），执行后 App 会退出。
(() => {
  'use strict';
  const status = document.getElementById('status');
  const overlay = document.getElementById('busyOverlay');
  const btns = ['cancel', 'keep', 'wipe'].map((id) => document.getElementById(id));

  function disableAll() {
    btns.forEach((b) => { b.disabled = true; });
  }

  async function run(wipe) {
    disableAll();
    overlay.classList.add('show');
    status.textContent = '';
    try {
      await window.__TAURI__.core.invoke('uninstall_run', { wipe });
      // App 即将退出；此处不再刷新
    } catch (e) {
      overlay.classList.remove('show');
      status.textContent = '卸载未完成：' + (e.message || e);
      status.className = 'status err';
      btns.forEach((b) => { b.disabled = false; });
    }
  }

  document.getElementById('cancel').addEventListener('click', () => window.close());
  const closeBtn = document.getElementById('closeBtn');
  if (closeBtn) closeBtn.addEventListener('click', () => window.close()); // 等于取消，不触发卸载
  document.getElementById('keep').addEventListener('click', () => run(false));
  document.getElementById('wipe').addEventListener('click', () => run(true));
})();

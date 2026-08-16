// 插件管理：调用 Rust 侧 plugin_op 命令（安装/卸载），回显输出。
(() => {
  'use strict';
  const pkg = document.getElementById('pkg');
  const btnInstall = document.getElementById('btnInstall');
  const btnRemove = document.getElementById('btnRemove');
  const status = document.getElementById('status');
  const output = document.getElementById('output');

  function setBusy(busy) {
    btnInstall.disabled = busy;
    btnRemove.disabled = busy;
    pkg.disabled = busy;
  }

  function setStatus(text, kind) {
    status.textContent = text;
    status.className = 'status ' + (kind || '');
  }

  async function run(op) {
    const name = pkg.value.trim();
    if (!name) {
      setStatus('请输入包名', 'err');
      return;
    }
    setBusy(true);
    setStatus(op === 'add' ? '正在安装…（首次需下载依赖，可能较久）' : '正在卸载…');
    output.hidden = false;
    output.textContent = '';
    try {
      const text = await window.__TAURI__.core.invoke('plugin_op', { op, pkg: name });
      output.textContent = text;
      setStatus(op === 'add' ? '安装完成，请重启工作台生效' : '卸载完成，请重启工作台生效', 'ok');
    } catch (e) {
      const msg = typeof e === 'string' ? e : (e && e.message) || String(e);
      output.textContent = msg;
      setStatus('操作失败，详见下方输出', 'err');
    } finally {
      setBusy(false);
    }
  }

  btnInstall.addEventListener('click', () => run('add'));
  btnRemove.addEventListener('click', () => run('remove'));
  pkg.addEventListener('keydown', (e) => {
    if (e.key === 'Enter') run('add');
  });
})();

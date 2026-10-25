// 共享 Tauri API 封装（withGlobalTauri 注入）。
// 不在模块顶层直接解构 window.__TAURI__：注入时机偶尔晚于模块求值，
// 顶层访问会让整个页面脚本死于 TypeError。
function tauriApi() {
  const t = window.__TAURI__;
  if (!t || !t.core || !t.event) {
    throw new Error("Tauri API 尚未注入");
  }
  return t;
}

export function invoke(...args) {
  return tauriApi().core.invoke(...args);
}

export function listen(...args) {
  return tauriApi().event.listen(...args);
}

/**
 * 带超时重试的 invoke：窗口刚创建时主线程可能还在 setup()，
 * 过早发出的 IPC 消息会静默丢失（Promise 永远 pending），
 * 超时后重发，事件循环就绪后必然成功。
 */
export async function invokeRetry(
  cmd,
  args,
  { attempts = 5, perTryMs = 2500, gapMs = 700 } = {}
) {
  let lastErr = null;
  for (let i = 0; i < attempts; i++) {
    try {
      return await Promise.race([
        invoke(cmd, args),
        new Promise((_, reject) =>
          setTimeout(() => reject(new Error("IPC 超时: " + cmd)), perTryMs)
        ),
      ]);
    } catch (e) {
      lastErr = e;
      await new Promise((r) => setTimeout(r, gapMs));
    }
  }
  throw lastErr || new Error("IPC 失败: " + cmd);
}

export function escapeHtml(s) {
  return String(s)
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

export function basename(p) {
  return String(p).split(/[\\/]/).pop() || "";
}

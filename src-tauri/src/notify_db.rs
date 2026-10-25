//! Windows 通知库（wpndatabase.db）只读访问：动态绑定 winsqlite3.dll 的最小 SQLite 包装。
//!
//! 为什么不加 rusqlite：项目不允许新增 crate 依赖，而 `System32\winsqlite3.dll`
//! 随 Windows 自带；这里用 LoadLibraryW + GetProcAddress 运行时取符号，
//! 编译期不链接该 DLL，取不到符号时能返回可读错误而不是启动失败。
//!
//! 通知历史是**只读镜像**（库不可写，系统也没有删除 API），因此只实现
//! `open_v2 / prepare_v2 / step / column_* / finalize / errmsg / busy_timeout / close_v2`。
//! 主库打开失败时降级：把 db / -wal / -shm 快照到 `%TEMP%\easy-dash-wpndb\` 后重试一次。
//! 任何失败都返回中文 `Err`，调用方只需记录日志，不会 panic。
#![allow(dead_code)]

use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_uchar, c_void, CStr, CString};
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::OnceLock;

use windows::core::{PCSTR, PCWSTR};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

// ==================== SQLite 常量（见 sqlite3.h） ====================

const SQLITE_OK: c_int = 0;
const SQLITE_ROW: c_int = 100;
const SQLITE_DONE: c_int = 101;
/// 只读打开（不允许写入，符合通知库使用方式）
const SQLITE_OPEN_READONLY: c_int = 0x0001;
/// 忙等超时：系统进程持有通知库写锁时不要无限等待
const BUSY_TIMEOUT_MS: c_int = 2000;
/// 调用方传入 limit 的防御上限
const MAX_LIMIT: usize = 200;
/// 降级快照目录（%TEMP% 下）
const SNAPSHOT_DIR: &str = "easy-dash-wpndb";

// ==================== 对外的数据形状 ====================

/// 从通知库读出的原始一行（payload 是未解析的 toast XML 文本）
#[derive(Debug, Clone)]
pub struct RawNotification {
    pub id: i64,
    pub handler_id: i64,
    /// 到达时间：Windows FILETIME（自 1601-01-01 起的 100ns 计数）
    pub arrival_ft: i64,
    /// BLOB，内容为 UTF-8 的 toast XML
    pub payload: String,
}

/// 通知处理器（一个应用/系统组件）的标识信息
#[derive(Debug, Clone)]
pub struct HandlerInfo {
    /// 例如 "QQ"、"dev.zcode.app"、exe 路径、或 AUMID
    pub primary_id: String,
    /// 例如 "app:desktop" / "app:system" / "app:immersive"
    pub handler_type: String,
}

// ==================== winsqlite3.dll 动态符号 ====================

type SqliteOpenV2 =
    unsafe extern "system" fn(*const c_char, *mut *mut c_void, c_int, *const c_char) -> c_int;
type SqliteCloseV2 = unsafe extern "system" fn(*mut c_void) -> c_int;
type SqlitePrepareV2 = unsafe extern "system" fn(
    *mut c_void,   // db
    *const c_char, // sql
    c_int,         // nByte（-1 = 以 NUL 结尾）
    *mut *mut c_void,       // stmt 输出
    *mut *const c_char,     // tail 输出（可为 null）
) -> c_int;
type SqliteStep = unsafe extern "system" fn(*mut c_void) -> c_int;
type SqliteColumnText = unsafe extern "system" fn(*mut c_void, c_int) -> *const c_uchar;
type SqliteColumnBytes = unsafe extern "system" fn(*mut c_void, c_int) -> c_int;
type SqliteColumnInt64 = unsafe extern "system" fn(*mut c_void, c_int) -> i64;
type SqliteFinalize = unsafe extern "system" fn(*mut c_void) -> c_int;
type SqliteErrmsg = unsafe extern "system" fn(*mut c_void) -> *const c_char;
type SqliteBusyTimeout = unsafe extern "system" fn(*mut c_void, c_int) -> c_int;

/// 用到的 winsqlite3 导出函数表（进程内只加载一次）
struct Api {
    open_v2: SqliteOpenV2,
    close_v2: SqliteCloseV2,
    prepare_v2: SqlitePrepareV2,
    step: SqliteStep,
    column_text: SqliteColumnText,
    column_bytes: SqliteColumnBytes,
    column_int64: SqliteColumnInt64,
    finalize: SqliteFinalize,
    errmsg: SqliteErrmsg,
    busy_timeout: SqliteBusyTimeout,
}

/// 取一个导出符号并转成对应函数指针类型。
/// 签名以 sqlite3.h 为准；`transmute` 的源/目标同为指针宽度，编译期即可校验。
macro_rules! load_sym {
    ($module:expr, $name:literal, $ty:ty) => {{
        let cname = CString::new($name).map_err(|_| format!("非法导出名: {}", $name))?;
        // SAFETY: GetProcAddress 只读取模块导出表，不产生副作用
        let proc = unsafe { GetProcAddress($module, PCSTR::from_raw(cname.as_ptr() as *const u8)) }
            .ok_or_else(|| format!("winsqlite3.dll 缺少导出符号 {}", $name))?;
        // SAFETY: 签名由 sqlite3.h 保证；FARPROC 与具体函数指针同为指针宽度。
        // 源类型写 `_`：GetProcAddress 返回的 FARPROC 是 Option<fn() -> isize>，`?` 已解包成裸函数指针。
        unsafe { std::mem::transmute::<_, $ty>(proc) }
    }};
}

impl Api {
    fn load() -> Result<Api, String> {
        let dll_name = crate::winapi::common::to_wide("winsqlite3.dll");
        // SAFETY: 入参是以 NUL 结尾的宽字符串；失败返回 Err 由上层降级处理
        let module = unsafe { LoadLibraryW(PCWSTR(dll_name.as_ptr())) }
            .map_err(|e| format!("加载 winsqlite3.dll 失败：{e}"))?;
        Ok(Api {
            open_v2: load_sym!(module, "sqlite3_open_v2", SqliteOpenV2),
            close_v2: load_sym!(module, "sqlite3_close_v2", SqliteCloseV2),
            prepare_v2: load_sym!(module, "sqlite3_prepare_v2", SqlitePrepareV2),
            step: load_sym!(module, "sqlite3_step", SqliteStep),
            column_text: load_sym!(module, "sqlite3_column_text", SqliteColumnText),
            column_bytes: load_sym!(module, "sqlite3_column_bytes", SqliteColumnBytes),
            column_int64: load_sym!(module, "sqlite3_column_int64", SqliteColumnInt64),
            finalize: load_sym!(module, "sqlite3_finalize", SqliteFinalize),
            errmsg: load_sym!(module, "sqlite3_errmsg", SqliteErrmsg),
            busy_timeout: load_sym!(module, "sqlite3_busy_timeout", SqliteBusyTimeout),
        })
    }
}

/// 进程内缓存的 Api（失败结果也缓存，避免每轮刷新都重试加载 DLL）
fn api() -> Result<&'static Api, String> {
    static API: OnceLock<Result<Api, String>> = OnceLock::new();
    API.get_or_init(Api::load)
        .as_ref()
        .map_err(|e| e.clone())
}

/// 读取 sqlite3_errmsg（handle 必须是 db 句柄）
unsafe fn err_text(api: &'static Api, db: *mut c_void) -> String {
    let p = (api.errmsg)(db);
    if p.is_null() {
        return String::from("未知错误");
    }
    CStr::from_ptr(p).to_string_lossy().into_owned()
}

// ==================== 句柄包装 ====================

/// 只读打开的数据库连接（Drop 时 close_v2）
struct Db {
    api: &'static Api,
    db: *mut c_void,
}

impl Db {
    fn open_readonly(path: &Path) -> Result<Db, String> {
        let api = api()?;
        let path_str = path.to_string_lossy();
        let cpath = CString::new(path_str.as_bytes())
            .map_err(|_| format!("通知库路径包含非法字符：{}", path.display()))?;
        let mut db: *mut c_void = ptr::null_mut();
        // SAFETY: cpath 以 NUL 结尾；db 由 sqlite 分配（失败时也可能非空，需关闭）
        let rc = unsafe {
            (api.open_v2)(
                cpath.as_ptr(),
                &mut db,
                SQLITE_OPEN_READONLY,
                ptr::null(),
            )
        };
        if rc != SQLITE_OK {
            let msg = if db.is_null() {
                String::from("未知错误")
            } else {
                unsafe { err_text(api, db) }
            };
            if !db.is_null() {
                unsafe {
                    let _ = (api.close_v2)(db);
                }
            }
            return Err(format!(
                "打开通知库失败（错误码 {rc}）：{msg}（{}）",
                path.display()
            ));
        }
        // 只读连接一般不会被写锁长时间阻塞，但系统正在写通知时仍可能短暂加锁
        unsafe {
            let _ = (api.busy_timeout)(db, BUSY_TIMEOUT_MS);
        }
        Ok(Db { api, db })
    }

    fn prepare(&self, sql: &str) -> Result<Stmt, String> {
        let csql = CString::new(sql).map_err(|_| String::from("查询语句包含非法字符"))?;
        let mut st: *mut c_void = ptr::null_mut();
        // SAFETY: csql 以 NUL 结尾；st 由 sqlite 分配
        let rc = unsafe {
            (self.api.prepare_v2)(self.db, csql.as_ptr(), -1, &mut st, ptr::null_mut())
        };
        if rc != SQLITE_OK {
            return Err(format!(
                "准备通知库查询失败（错误码 {rc}）：{}",
                unsafe { err_text(self.api, self.db) }
            ));
        }
        Ok(Stmt {
            api: self.api,
            db: self.db,
            st,
        })
    }

    /// 执行查询，把每一行交给回调；回调返回 Err 会提前结束。
    fn query(
        &self,
        sql: &str,
        mut f: impl FnMut(&Stmt) -> Result<(), String>,
    ) -> Result<(), String> {
        let stmt = self.prepare(sql)?;
        while stmt.step()? {
            f(&stmt)?;
        }
        Ok(())
    }
}

impl Drop for Db {
    fn drop(&mut self) {
        if !self.db.is_null() {
            // SAFETY: db 由本结构独占，且只关闭一次
            unsafe {
                let _ = (self.api.close_v2)(self.db);
            }
        }
    }
}

/// 预编译语句（Drop 时 finalize）
struct Stmt {
    api: &'static Api,
    db: *mut c_void,
    st: *mut c_void,
}

impl Stmt {
    /// 推进一行：`true` = 取到一行，`false` = 结果集结束
    fn step(&self) -> Result<bool, String> {
        // SAFETY: st 有效且由本结构独占
        let rc = unsafe { (self.api.step)(self.st) };
        match rc {
            SQLITE_ROW => Ok(true),
            SQLITE_DONE => Ok(false),
            other => Err(format!(
                "读取通知库失败（错误码 {other}）：{}",
                unsafe { err_text(self.api, self.db) }
            )),
        }
    }

    /// 取第 i 列文本；SQL NULL 返回 None。
    /// Payload 是 BLOB，但内容就是 UTF-8 文本，column_text 会原样给出字节。
    fn text(&self, i: c_int) -> Option<String> {
        // SAFETY: 指针归属于 stmt，长度取自 sqlite3_column_bytes；立刻复制出来
        unsafe {
            let p = (self.api.column_text)(self.st, i);
            if p.is_null() {
                return None;
            }
            let n = (self.api.column_bytes)(self.st, i);
            if n <= 0 {
                return Some(String::new());
            }
            Some(String::from_utf8_lossy(std::slice::from_raw_parts(p, n as usize)).into_owned())
        }
    }

    fn i64(&self, i: c_int) -> i64 {
        // SAFETY: st 有效；NULL 时 sqlite 返回 0
        unsafe { (self.api.column_int64)(self.st, i) }
    }
}

impl Drop for Stmt {
    fn drop(&mut self) {
        if !self.st.is_null() {
            // SAFETY: st 由本结构独占，且只 finalize 一次
            unsafe {
                let _ = (self.api.finalize)(self.st);
            }
        }
    }
}

// ==================== 路径与降级快照 ====================

/// `%LOCALAPPDATA%\Microsoft\Windows\Notifications\wpndatabase.db`
fn notifications_db_path() -> Result<PathBuf, String> {
    let local = std::env::var_os("LOCALAPPDATA")
        .ok_or_else(|| String::from("取不到 LOCALAPPDATA 环境变量，无法定位通知库"))?;
    Ok(PathBuf::from(local)
        .join("Microsoft")
        .join("Windows")
        .join("Notifications")
        .join("wpndatabase.db"))
}

/// 把通知库（含 -wal / -shm）复制到 `%TEMP%\easy-dash-wpndb\`，返回快照目录。
/// 先删旧快照再复制：上次残留的 -wal 与本次主库不匹配会让 SQLite 读到半截数据。
fn snapshot_to_temp() -> Result<PathBuf, String> {
    let src = notifications_db_path()?;
    if !src.is_file() {
        return Err(format!("通知库不存在：{}", src.display()));
    }
    let dir = std::env::temp_dir().join(SNAPSHOT_DIR);
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("创建通知库快照目录失败（{}）：{e}", dir.display()))?;
    let src_dir = src
        .parent()
        .ok_or_else(|| String::from("通知库路径异常（没有父目录）"))?;
    for name in ["wpndatabase.db", "wpndatabase.db-wal", "wpndatabase.db-shm"] {
        let from = src_dir.join(name);
        let to = dir.join(name);
        // 旧快照可能残留（尤其 -wal/-shm），先清理再复制
        let _ = std::fs::remove_file(&to);
        if from.is_file() {
            std::fs::copy(&from, &to)
                .map_err(|e| format!("复制通知库文件失败（{}）：{e}", from.display()))?;
        }
    }
    Ok(dir)
}

// ==================== 对外入口 ====================

/// 读取最近 `limit` 条通知原始行，以及全量「处理器 id → 处理器信息」映射。
///
/// 顺序：按 ArrivalTime（100ns FILETIME）倒序，最新在前。
/// 主库直接打开失败时复制快照到临时目录重试一次；仍失败返回中文错误。
pub fn load_recent(
    limit: usize,
) -> Result<(Vec<RawNotification>, HashMap<i64, HandlerInfo>), String> {
    let limit = limit.clamp(1, MAX_LIMIT);
    let path = notifications_db_path()?;
    match read_db(&path, limit) {
        Ok(v) => Ok(v),
        Err(first) => {
            // 降级：系统可能长时间持有写锁 / WAL 不一致，快照后只读副本再试
            match snapshot_to_temp() {
                Ok(dir) => read_db(&dir.join("wpndatabase.db"), limit)
                    .map_err(|second| format!("{first}；复制快照后重试仍失败：{second}")),
                Err(snap_err) => Err(format!("{first}；快照复制失败：{snap_err}")),
            }
        }
    }
}

/// 从指定库文件读处理器表 + 最近通知
fn read_db(
    path: &Path,
    limit: usize,
) -> Result<(Vec<RawNotification>, HashMap<i64, HandlerInfo>), String> {
    let db = Db::open_readonly(path)?;

    // 处理器表行数很少（几百行），整表读入内存即可
    let mut handlers: HashMap<i64, HandlerInfo> = HashMap::new();
    db.query(
        "SELECT RecordId, PrimaryId, HandlerType FROM NotificationHandler",
        |row| {
            let record = row.i64(0);
            handlers.insert(
                record,
                HandlerInfo {
                    primary_id: row.text(1).unwrap_or_default(),
                    handler_type: row.text(2).unwrap_or_default(),
                },
            );
            Ok(())
        },
    )?;

    // Notification.Order 是主键、Id 只是业务 id；展示顺序必须按到达时间排。
    // limit 已在入口 clamp 过，直接内联进 SQL（避免再依赖 sqlite3_bind_*）。
    let sql = format!(
        "SELECT Id, HandlerId, ArrivalTime, Payload FROM Notification \
         WHERE Payload IS NOT NULL AND ArrivalTime IS NOT NULL \
         ORDER BY ArrivalTime DESC LIMIT {limit}"
    );
    let mut out: Vec<RawNotification> = Vec::with_capacity(limit);
    db.query(&sql, |row| {
        out.push(RawNotification {
            id: row.i64(0),
            handler_id: row.i64(1),
            arrival_ft: row.i64(2),
            payload: row.text(3).unwrap_or_default(),
        });
        Ok(())
    })?;

    Ok((out, handlers))
}

// ==================== 本机真实库冒烟测试（默认忽略） ====================

#[cfg(test)]
mod tests {
    /// 真实读取本机通知库：依赖当前账号的通知历史，因此默认 `#[ignore]`。
    /// 手动验证：`cargo test --manifest-path src-tauri/Cargo.toml -- --ignored --nocapture`
    #[test]
    #[ignore = "依赖本机 Windows 通知历史，手动执行"]
    fn smoke_load_real_db() {
        let path = match super::notifications_db_path() {
            Ok(p) => p,
            Err(e) => {
                eprintln!("[smoke] 跳过：{e}");
                return;
            }
        };
        if !path.is_file() {
            eprintln!("[smoke] 跳过：通知库不存在（{}）", path.display());
            return;
        }
        let (rows, handlers) = super::load_recent(5).expect("读取本机通知库应成功");
        eprintln!(
            "[smoke] rows={} handlers={}",
            rows.len(),
            handlers.len()
        );
        for r in rows.iter().take(3) {
            let info = handlers.get(&r.handler_id);
            eprintln!(
                "[smoke] id={} handler={:?} ft={} payload_len={}",
                r.id,
                info.map(|h| &h.primary_id),
                r.arrival_ft,
                r.payload.len()
            );
        }
        assert!(!rows.is_empty(), "本机通知库应至少有一条通知");
    }

    /// 验证降级路径：把 db/-wal/-shm 快照到 %TEMP% 后只读打开。
    /// 默认 `#[ignore]`（依赖本机通知历史，且会在临时目录写文件）。
    /// 手动验证：`cargo test --manifest-path src-tauri/Cargo.toml --lib -- --ignored --nocapture`
    #[test]
    #[ignore = "依赖本机 Windows 通知历史，手动执行"]
    fn smoke_snapshot_fallback() {
        match super::notifications_db_path() {
            Ok(p) if p.is_file() => {}
            _ => {
                eprintln!("[smoke] 跳过：通知库不存在");
                return;
            }
        }
        let dir = super::snapshot_to_temp().expect("快照复制应成功");
        let (rows, handlers) =
            super::read_db(&dir.join("wpndatabase.db"), 3).expect("读取快照库应成功");
        eprintln!(
            "[smoke] snapshot rows={} handlers={} dir={}",
            rows.len(),
            handlers.len(),
            dir.display()
        );
        assert!(!rows.is_empty(), "快照库应至少有一条通知");
    }
}

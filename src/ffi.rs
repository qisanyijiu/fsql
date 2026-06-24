use std::ffi::{CStr, CString, c_char};
use std::path::PathBuf;
use std::ptr;

use crate::{Connection, ConnectionPool, Database, QueryResult, Row, Value};

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsqlStatus {
    Ok = 0,
    NullPointer = 1,
    InvalidUtf8 = 2,
    Error = 3,
    OutOfBounds = 4,
    WrongType = 5,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsqlValueKind {
    Null = 0,
    Integer = 1,
    Float = 2,
    Boolean = 3,
    Text = 4,
    Vector = 5,
    Point = 6,
}

#[repr(C)]
pub struct FsqlDatabaseHandle(Database);

#[repr(C)]
pub struct FsqlPoolHandle(ConnectionPool);

#[repr(C)]
pub struct FsqlConnectionHandle(Connection);

#[repr(C)]
pub struct FsqlResultHandle(QueryResult);

#[repr(C)]
pub struct FsqlStringHandle(CString);

#[repr(C)]
pub struct FsqlValueHandle {
    value: Value,
}

#[repr(C)]
pub struct FsqlVectorHandle {
    data: Vec<f32>,
}

#[repr(C)]
pub struct FsqlPoint {
    pub lon: f64,
    pub lat: f64,
}

/// - 中文: 为失败的 FFI 调用创建错误字符串，并在调用方提供输出指针时回写该句柄。
/// - English: Creates an error string for a failed FFI call and writes the handle back when the caller provides an output pointer.
/// - 中文: 传入的 `error_out` 可以为空，此时函数静默跳过错误句柄写入。
/// - English: The incoming `error_out` may be null, in which case the function silently skips writing an error handle.
/// - 中文: 生成的字符串句柄由 Rust 分配，调用方后续必须使用 `fsql_string_free` 释放。
/// - English: The produced string handle is allocated by Rust and must later be released by the caller with `fsql_string_free`.
fn set_error(error_out: *mut *mut FsqlStringHandle, message: impl Into<String>) {
    if error_out.is_null() {
        return;
    }
    unsafe {
        *error_out = string_handle(message.into());
    }
}

/// - 中文: 将普通 Rust 字符串包装成 FFI 可持有的字符串句柄。
/// - English: Wraps a regular Rust string into an FFI-owned string handle.
/// - 中文: 该函数会把内部 `NUL` 字符替换为空格，保证结果可安全构造为 `CString`。
/// - English: This function replaces embedded `NUL` bytes with spaces so the result can be safely converted into a `CString`.
/// - 中文: 返回的句柄拥有独立所有权，必须由调用方或上层 FFI 路径显式释放。
/// - English: The returned handle has independent ownership and must be explicitly released by the caller or higher-level FFI path.
fn string_handle(message: String) -> *mut FsqlStringHandle {
    let sanitized = message.replace('\0', " ");
    let cstring = CString::new(sanitized).expect("CString sanitization removed nul bytes");
    Box::into_raw(Box::new(FsqlStringHandle(cstring)))
}

/// - 中文: 将 C 风格字符串指针读取为 Rust `String`。
/// - English: Reads a C string pointer into a Rust `String`.
/// - 中文: 输入指针不能为空，且内容必须是有效 UTF-8；否则返回对应的 FFI 状态码。
/// - English: The input pointer must be non-null and contain valid UTF-8, otherwise the corresponding FFI status code is returned.
/// - 中文: 该函数只借用输入指针内容，不接管也不释放调用方的内存。
/// - English: This function only borrows the input pointer contents and does not take ownership of or free the caller memory.
fn require_cstr(input: *const c_char) -> Result<String, FsqlStatus> {
    if input.is_null() {
        return Err(FsqlStatus::NullPointer);
    }
    let value = unsafe { CStr::from_ptr(input) };
    value
        .to_str()
        .map(|text| text.to_owned())
        .map_err(|_| FsqlStatus::InvalidUtf8)
}

/// - 中文: 将可变裸指针验证并转换为可变 Rust 引用。
/// - English: Validates a mutable raw pointer and converts it into a mutable Rust reference.
/// - 中文: 该函数只检查空指针，不验证别名、安全生命周期或调用方是否满足 Rust 可变借用约束。
/// - English: This function only checks for null pointers and does not validate aliasing, lifetime safety, or whether the caller satisfies Rust mutable borrowing rules.
/// - 中文: 若指针为空则返回 `NullPointer`，否则继续沿用原始内存所有权模型。
/// - English: It returns `NullPointer` for null inputs and otherwise preserves the original memory ownership model.
fn require_mut<'a, T>(ptr: *mut T) -> Result<&'a mut T, FsqlStatus> {
    if ptr.is_null() {
        return Err(FsqlStatus::NullPointer);
    }
    Ok(unsafe { &mut *ptr })
}

/// - 中文: 将只读裸指针验证并转换为只读 Rust 引用。
/// - English: Validates a read-only raw pointer and converts it into an immutable Rust reference.
/// - 中文: 该函数只处理空指针检查，调用方仍需保证底层对象在借用期间有效且未被错误释放。
/// - English: This function only performs a null check, and the caller must still guarantee that the underlying object stays valid and is not incorrectly freed during the borrow.
/// - 中文: 返回的引用不拥有底层对象，也不会改变原始内存的释放责任。
/// - English: The returned reference does not own the underlying object and does not alter the original responsibility for freeing memory.
fn require_ref<'a, T>(ptr: *const T) -> Result<&'a T, FsqlStatus> {
    if ptr.is_null() {
        return Err(FsqlStatus::NullPointer);
    }
    Ok(unsafe { &*ptr })
}

/// - 中文: 统一执行可能失败的 Rust 闭包，并把错误转换成 FFI 状态与错误字符串。
/// - English: Runs a potentially failing Rust closure and converts errors into an FFI status plus an error string.
/// - 中文: 传入闭包应返回 crate 级 `Result<T>`，以便这里集中处理错误映射逻辑。
/// - English: The closure should return the crate-level `Result<T>` so this helper can centralize error mapping.
/// - 中文: 当闭包失败时，本函数会尝试写入 `error_out`，并返回通用的 `FsqlStatus::Error`。
/// - English: When the closure fails, this helper attempts to populate `error_out` and returns the generic `FsqlStatus::Error`.
fn with_result<T>(
    error_out: *mut *mut FsqlStringHandle,
    f: impl FnOnce() -> crate::Result<T>,
) -> Result<T, FsqlStatus> {
    f().map_err(|error| {
        set_error(error_out, error.to_string());
        FsqlStatus::Error
    })
}

/// - 中文: 将一行结果转换为按列顺序可枚举的键值切片视图。
/// - English: Converts a result row into an enumerable key-value slice view ordered by columns.
/// - 中文: 该辅助函数主要服务于 FFI 结果遍历接口，例如列名和单元格值访问。
/// - English: This helper mainly serves FFI result traversal APIs such as column-name and cell-value access.
/// - 中文: 返回值只借用原始 `Row`，不会分配新值，也不会转移底层值的所有权。
/// - English: The return value only borrows the original `Row`, does not allocate new values, and does not transfer ownership of the underlying data.
fn row_entries(row: &Row) -> Vec<(&str, &Value)> {
    row.iter()
        .map(|(key, value)| (key.as_str(), value))
        .collect()
}

/// - 中文: 返回字符串句柄内部 `CString` 的只读字符指针。
/// - English: Returns a read-only character pointer for the inner `CString` stored in a string handle.
/// - 中文: 调用方必须保证传入句柄非空且在使用返回指针期间仍然存活；空句柄会返回空指针。
/// - English: The caller must ensure the incoming handle is non-null and remains alive while using the returned pointer; null handles yield a null pointer.
/// - 中文: 返回指针是借用视图，不能由调用方释放；释放应针对原始句柄执行 `fsql_string_free`。
/// - English: The returned pointer is a borrowed view and must not be freed by the caller; freeing must be performed on the original handle with `fsql_string_free`.
#[unsafe(no_mangle)]
pub extern "C" fn fsql_string_data(handle: *const FsqlStringHandle) -> *const c_char {
    match require_ref(handle) {
        Ok(handle) => handle.0.as_ptr(),
        Err(_) => ptr::null(),
    }
}

/// - 中文: 释放由 Rust 分配的字符串句柄。
/// - English: Frees a string handle allocated by Rust.
/// - 中文: 该函数接受空指针并将其视为 no-op，方便外部语言在清理路径上统一调用。
/// - English: This function accepts null pointers and treats them as a no-op so foreign-language cleanup paths can call it uniformly.
/// - 中文: 只能用于释放通过 FFI 返回的 `FsqlStringHandle`，不能用于 `fsql_string_data` 返回的字符指针。
/// - English: It may only be used to free `FsqlStringHandle` values returned by the FFI, not the raw character pointer returned by `fsql_string_data`.
#[unsafe(no_mangle)]
pub extern "C" fn fsql_string_free(handle: *mut FsqlStringHandle) {
    if handle.is_null() {
        return;
    }
    unsafe {
        drop(Box::from_raw(handle));
    }
}

/// - 中文: 创建一个基于内存的数据库句柄并返回给外部调用方。
/// - English: Creates an in-memory database handle and returns it to foreign callers.
/// - 中文: `out` 不能为空；若创建失败，本函数会在可能的情况下写入 `error_out`。
/// - English: `out` must be non-null; when creation fails, this function writes `error_out` when possible.
/// - 中文: 成功返回的数据库句柄由 Rust 分配，调用方必须用 `fsql_db_free` 释放。
/// - English: The returned database handle is allocated by Rust and must be released by the caller with `fsql_db_free`.
#[unsafe(no_mangle)]
pub extern "C" fn fsql_db_memory_new(
    out: *mut *mut FsqlDatabaseHandle,
    error_out: *mut *mut FsqlStringHandle,
) -> FsqlStatus {
    let Ok(out) = require_mut(out) else {
        set_error(error_out, "database output pointer is null");
        return FsqlStatus::NullPointer;
    };
    *out = Box::into_raw(Box::new(FsqlDatabaseHandle(Database::memory())));
    FsqlStatus::Ok
}

/// - 中文: 打开一个文件后端数据库并返回外部可持有的数据库句柄。
/// - English: Opens a file-backed database and returns a database handle that can be held by foreign code.
/// - 中文: `path` 必须是有效 UTF-8 的 C 字符串，`out` 必须可写；路径无效或打开失败时会返回错误状态。
/// - English: `path` must be a valid UTF-8 C string and `out` must be writable; invalid paths or open failures return an error status.
/// - 中文: 成功返回的句柄归调用方所有，后续必须使用 `fsql_db_free` 释放。
/// - English: The successful handle is owned by the caller and must later be released with `fsql_db_free`.
#[unsafe(no_mangle)]
pub extern "C" fn fsql_db_open(
    path: *const c_char,
    out: *mut *mut FsqlDatabaseHandle,
    error_out: *mut *mut FsqlStringHandle,
) -> FsqlStatus {
    let Ok(path) = require_cstr(path) else {
        set_error(error_out, "database path is null or invalid utf-8");
        return FsqlStatus::InvalidUtf8;
    };
    let Ok(out) = require_mut(out) else {
        set_error(error_out, "database output pointer is null");
        return FsqlStatus::NullPointer;
    };
    match with_result(error_out, || Database::open(PathBuf::from(path))) {
        Ok(database) => {
            *out = Box::into_raw(Box::new(FsqlDatabaseHandle(database)));
            FsqlStatus::Ok
        }
        Err(status) => status,
    }
}

/// - 中文: 在数据库句柄上执行一条 SQL 并返回结果句柄。
/// - English: Executes a SQL statement on a database handle and returns a result handle.
/// - 中文: `database`、`sql`、`out` 都必须有效；SQL 文本需要是 UTF-8，并遵循引擎当前支持的语法子集。
/// - English: `database`, `sql`, and `out` must all be valid; the SQL text must be UTF-8 and follow the syntax subset currently supported by the engine.
/// - 中文: 成功时结果句柄由 Rust 分配，调用方必须使用 `fsql_result_free` 释放；失败时会尽量通过 `error_out` 返回错误信息。
/// - English: On success the result handle is allocated by Rust and must be freed with `fsql_result_free`; on failure an error message is returned through `error_out` when possible.
/// - 中文: 该调用可能触发查询、副作用写入、事务状态变化以及日志输出。
/// - English: This call may trigger queries, mutating side effects, transaction-state changes, and log output.
#[unsafe(no_mangle)]
pub extern "C" fn fsql_db_execute(
    database: *mut FsqlDatabaseHandle,
    sql: *const c_char,
    out: *mut *mut FsqlResultHandle,
    error_out: *mut *mut FsqlStringHandle,
) -> FsqlStatus {
    let Ok(database) = require_mut(database) else {
        set_error(error_out, "database handle is null");
        return FsqlStatus::NullPointer;
    };
    let Ok(sql) = require_cstr(sql) else {
        set_error(error_out, "sql is null or invalid utf-8");
        return FsqlStatus::InvalidUtf8;
    };
    let Ok(out) = require_mut(out) else {
        set_error(error_out, "result output pointer is null");
        return FsqlStatus::NullPointer;
    };
    match with_result(error_out, || database.0.execute(&sql)) {
        Ok(result) => {
            *out = Box::into_raw(Box::new(FsqlResultHandle(result)));
            FsqlStatus::Ok
        }
        Err(status) => status,
    }
}

/// - 中文: 释放由 FFI 创建并返回的数据库句柄。
/// - English: Frees a database handle created and returned by the FFI.
/// - 中文: 空指针会被视为 no-op；重复释放同一非空句柄仍然属于未定义调用方行为。
/// - English: Null pointers are treated as a no-op, while freeing the same non-null handle twice remains invalid caller behavior.
/// - 中文: 释放后该句柄以及所有基于它派生的借用视图都不能再继续使用。
/// - English: After freeing, the handle and any borrowed views derived from it must no longer be used.
#[unsafe(no_mangle)]
pub extern "C" fn fsql_db_free(handle: *mut FsqlDatabaseHandle) {
    if handle.is_null() {
        return;
    }
    unsafe {
        drop(Box::from_raw(handle));
    }
}

/// - 中文: 创建一个基于内存数据库的连接池句柄。
/// - English: Creates a connection-pool handle backed by an in-memory database.
/// - 中文: `max_connections` 必须大于零，`out` 必须可写；创建失败时会返回错误状态并尽量写入 `error_out`。
/// - English: `max_connections` must be greater than zero and `out` must be writable; failures return an error status and populate `error_out` when possible.
/// - 中文: 成功返回的连接池句柄由 Rust 分配，调用方需要使用 `fsql_pool_free` 释放。
/// - English: The successful pool handle is allocated by Rust and must be released by the caller with `fsql_pool_free`.
#[unsafe(no_mangle)]
pub extern "C" fn fsql_pool_memory_new(
    max_connections: usize,
    out: *mut *mut FsqlPoolHandle,
    error_out: *mut *mut FsqlStringHandle,
) -> FsqlStatus {
    let Ok(out) = require_mut(out) else {
        set_error(error_out, "pool output pointer is null");
        return FsqlStatus::NullPointer;
    };
    match with_result(error_out, || ConnectionPool::memory(max_connections)) {
        Ok(pool) => {
            *out = Box::into_raw(Box::new(FsqlPoolHandle(pool)));
            FsqlStatus::Ok
        }
        Err(status) => status,
    }
}

/// - 中文: 打开一个文件后端数据库并返回对应的连接池句柄。
/// - English: Opens a file-backed database and returns the corresponding connection-pool handle.
/// - 中文: `path` 必须是有效 UTF-8，`max_connections` 必须合法，`out` 需要可写。
/// - English: `path` must be valid UTF-8, `max_connections` must be valid, and `out` must be writable.
/// - 中文: 返回的池句柄归调用方所有，后续需要通过 `fsql_pool_free` 释放。
/// - English: The returned pool handle is owned by the caller and must later be released with `fsql_pool_free`.
#[unsafe(no_mangle)]
pub extern "C" fn fsql_pool_open(
    path: *const c_char,
    max_connections: usize,
    out: *mut *mut FsqlPoolHandle,
    error_out: *mut *mut FsqlStringHandle,
) -> FsqlStatus {
    let Ok(path) = require_cstr(path) else {
        set_error(error_out, "pool path is null or invalid utf-8");
        return FsqlStatus::InvalidUtf8;
    };
    let Ok(out) = require_mut(out) else {
        set_error(error_out, "pool output pointer is null");
        return FsqlStatus::NullPointer;
    };
    match with_result(error_out, || {
        ConnectionPool::open(PathBuf::from(path), max_connections)
    }) {
        Ok(pool) => {
            *out = Box::into_raw(Box::new(FsqlPoolHandle(pool)));
            FsqlStatus::Ok
        }
        Err(status) => status,
    }
}

/// - 中文: 从连接池中阻塞获取一个连接句柄。
/// - English: Blocks until it acquires a connection handle from the connection pool.
/// - 中文: `pool` 和 `out` 必须有效；若池内部状态损坏或获取失败，会返回错误状态。
/// - English: `pool` and `out` must be valid; if the pool state is poisoned or acquisition fails, an error status is returned.
/// - 中文: 成功返回的连接句柄需要由调用方使用 `fsql_connection_free` 释放，释放时也会归还池内 permit。
/// - English: A successful connection handle must be released by the caller with `fsql_connection_free`, which also returns the pool permit.
#[unsafe(no_mangle)]
pub extern "C" fn fsql_pool_get(
    pool: *const FsqlPoolHandle,
    out: *mut *mut FsqlConnectionHandle,
    error_out: *mut *mut FsqlStringHandle,
) -> FsqlStatus {
    let Ok(pool) = require_ref(pool) else {
        set_error(error_out, "pool handle is null");
        return FsqlStatus::NullPointer;
    };
    let Ok(out) = require_mut(out) else {
        set_error(error_out, "connection output pointer is null");
        return FsqlStatus::NullPointer;
    };
    match with_result(error_out, || pool.0.get()) {
        Ok(connection) => {
            *out = Box::into_raw(Box::new(FsqlConnectionHandle(connection)));
            FsqlStatus::Ok
        }
        Err(status) => status,
    }
}

/// - 中文: 尝试从连接池中非阻塞获取一个连接句柄。
/// - English: Tries to acquire a connection handle from the pool without blocking.
/// - 中文: 若池中当前没有可用连接，本函数仍返回 `Ok` 状态，但会把 `out` 设为空指针。
/// - English: If no connection is currently available, this function still returns an `Ok` status but sets `out` to a null pointer.
/// - 中文: 成功返回的非空连接句柄与 `fsql_pool_get` 一样，必须由调用方释放。
/// - English: A successful non-null connection handle follows the same ownership rule as `fsql_pool_get` and must be freed by the caller.
#[unsafe(no_mangle)]
pub extern "C" fn fsql_pool_try_get(
    pool: *const FsqlPoolHandle,
    out: *mut *mut FsqlConnectionHandle,
    error_out: *mut *mut FsqlStringHandle,
) -> FsqlStatus {
    let Ok(pool) = require_ref(pool) else {
        set_error(error_out, "pool handle is null");
        return FsqlStatus::NullPointer;
    };
    let Ok(out) = require_mut(out) else {
        set_error(error_out, "connection output pointer is null");
        return FsqlStatus::NullPointer;
    };
    match with_result(error_out, || pool.0.try_get()) {
        Ok(Some(connection)) => {
            *out = Box::into_raw(Box::new(FsqlConnectionHandle(connection)));
            FsqlStatus::Ok
        }
        Ok(None) => {
            *out = ptr::null_mut();
            FsqlStatus::Ok
        }
        Err(status) => status,
    }
}

/// - 中文: 释放由 FFI 创建并返回的连接池句柄。
/// - English: Frees a connection-pool handle created and returned by the FFI.
/// - 中文: 空指针会被忽略，但调用方仍需避免对同一非空句柄重复释放。
/// - English: Null pointers are ignored, but callers must still avoid freeing the same non-null handle twice.
/// - 中文: 释放池句柄后，不应再继续使用任何依赖该池生命周期的连接句柄。
/// - English: After freeing the pool handle, callers must no longer use connection handles that depend on that pool lifetime.
#[unsafe(no_mangle)]
pub extern "C" fn fsql_pool_free(handle: *mut FsqlPoolHandle) {
    if handle.is_null() {
        return;
    }
    unsafe {
        drop(Box::from_raw(handle));
    }
}

/// - 中文: 在池中连接句柄上执行一条 SQL 并返回结果句柄。
/// - English: Executes a SQL statement on a pooled connection handle and returns a result handle.
/// - 中文: 该调用沿用连接当前的事务上下文，因此 `BEGIN`、`COMMIT`、`ROLLBACK` 会影响该连接自己的事务状态。
/// - English: This call reuses the connection's current transaction context, so `BEGIN`, `COMMIT`, and `ROLLBACK` affect the transaction state owned by this connection.
/// - 中文: 成功返回的结果句柄由调用方释放，失败时会尽量通过 `error_out` 返回错误信息。
/// - English: The successful result handle must be released by the caller, and failures return an error message through `error_out` when possible.
/// - 中文: 该函数可能触发行锁冲突、事务提交/回滚以及池内并发控制路径。
/// - English: This function may trigger row-lock conflicts, transaction commit or rollback, and pool-level concurrency control paths.
#[unsafe(no_mangle)]
pub extern "C" fn fsql_connection_execute(
    connection: *const FsqlConnectionHandle,
    sql: *const c_char,
    out: *mut *mut FsqlResultHandle,
    error_out: *mut *mut FsqlStringHandle,
) -> FsqlStatus {
    let Ok(connection) = require_ref(connection) else {
        set_error(error_out, "connection handle is null");
        return FsqlStatus::NullPointer;
    };
    let Ok(sql) = require_cstr(sql) else {
        set_error(error_out, "sql is null or invalid utf-8");
        return FsqlStatus::InvalidUtf8;
    };
    let Ok(out) = require_mut(out) else {
        set_error(error_out, "result output pointer is null");
        return FsqlStatus::NullPointer;
    };
    match with_result(error_out, || connection.0.execute(&sql)) {
        Ok(result) => {
            *out = Box::into_raw(Box::new(FsqlResultHandle(result)));
            FsqlStatus::Ok
        }
        Err(status) => status,
    }
}

/// - 中文: 释放连接句柄并归还其占用的连接池 permit。
/// - English: Frees a connection handle and returns its acquired pool permit.
/// - 中文: 空指针会被忽略；若连接仍持有事务状态，底层释放流程还会负责清理关联资源。
/// - English: Null pointers are ignored; if the connection still owns transaction state, the underlying drop path also cleans up the associated resources.
/// - 中文: 调用方在释放后不得继续使用该连接句柄或依赖其事务上下文。
/// - English: Callers must not reuse the connection handle or rely on its transaction context after freeing it.
#[unsafe(no_mangle)]
pub extern "C" fn fsql_connection_free(handle: *mut FsqlConnectionHandle) {
    if handle.is_null() {
        return;
    }
    unsafe {
        drop(Box::from_raw(handle));
    }
}

#[unsafe(no_mangle)]
/// - 中文: 返回结果句柄记录的受影响行数。
/// - English: Returns the affected-row count recorded in a result handle.
/// - 中文: 空句柄会被视为无效输入，并回退返回零。
/// - English: A null handle is treated as invalid input and falls back to zero.
/// - 中文: 该函数只读访问结果对象，不转移所有权也不分配新内存。
/// - English: This function reads the result object only, without transferring ownership or allocating new memory.
#[unsafe(no_mangle)]
pub extern "C" fn fsql_result_affected_rows(handle: *const FsqlResultHandle) -> usize {
    require_ref(handle)
        .map(|handle| handle.0.affected_rows)
        .unwrap_or(0)
}

#[unsafe(no_mangle)]
/// - 中文: 返回结果集中包含的行数。
/// - English: Returns the number of rows contained in a result set.
/// - 中文: 需要传入有效结果句柄；空句柄时会返回零。
/// - English: It expects a valid result handle, and returns zero for a null handle.
/// - 中文: 返回值是纯读取结果，不会修改结果集状态。
/// - English: The return value is derived from a read-only lookup and does not modify result-set state.
#[unsafe(no_mangle)]
pub extern "C" fn fsql_result_row_count(handle: *const FsqlResultHandle) -> usize {
    require_ref(handle)
        .map(|handle| handle.0.rows.len())
        .unwrap_or(0)
}

#[unsafe(no_mangle)]
/// - 中文: 复制结果消息文本并通过字符串句柄返回给调用方。
/// - English: Copies the result message text and returns it to the caller through a string handle.
/// - 中文: `handle` 与 `out` 都必须有效，否则返回空指针状态。
/// - English: Both `handle` and `out` must be valid or the function returns a null-pointer status.
/// - 中文: 成功时输出句柄由 Rust 分配，调用方必须使用 `fsql_string_free` 释放。
/// - English: On success the output handle is allocated by Rust and must be released by the caller with `fsql_string_free`.
#[unsafe(no_mangle)]
pub extern "C" fn fsql_result_message(
    handle: *const FsqlResultHandle,
    out: *mut *mut FsqlStringHandle,
) -> FsqlStatus {
    let Ok(handle) = require_ref(handle) else {
        return FsqlStatus::NullPointer;
    };
    let Ok(out) = require_mut(out) else {
        return FsqlStatus::NullPointer;
    };
    *out = string_handle(handle.0.message.clone());
    FsqlStatus::Ok
}

#[unsafe(no_mangle)]
/// - 中文: 返回指定结果行中的列数量。
/// - English: Returns the number of columns in the specified result row.
/// - 中文: 若结果句柄为空或 `row_index` 越界，则回退返回零。
/// - English: It falls back to zero when the result handle is null or `row_index` is out of bounds.
/// - 中文: 该查询只读访问行结构，不创建列名或值句柄。
/// - English: This query reads row structure only and does not create column-name or value handles.
#[unsafe(no_mangle)]
pub extern "C" fn fsql_result_column_count(
    handle: *const FsqlResultHandle,
    row_index: usize,
) -> usize {
    require_ref(handle)
        .ok()
        .and_then(|handle| handle.0.rows.get(row_index))
        .map(|row| row.len())
        .unwrap_or(0)
}

#[unsafe(no_mangle)]
/// - 中文: 返回指定单元格位置对应的列名。
/// - English: Returns the column name associated with a specific cell position.
/// - 中文: `row_index` 和 `column_index` 必须落在结果范围内，`out` 也必须可写。
/// - English: `row_index` and `column_index` must be within result bounds, and `out` must also be writable.
/// - 中文: 成功时会分配新的字符串句柄，越界时返回 `OutOfBounds`。
/// - English: On success it allocates a new string handle, and it returns `OutOfBounds` for invalid indices.
#[unsafe(no_mangle)]
pub extern "C" fn fsql_result_column_name(
    handle: *const FsqlResultHandle,
    row_index: usize,
    column_index: usize,
    out: *mut *mut FsqlStringHandle,
) -> FsqlStatus {
    let Ok(handle) = require_ref(handle) else {
        return FsqlStatus::NullPointer;
    };
    let Ok(out) = require_mut(out) else {
        return FsqlStatus::NullPointer;
    };
    let Some(row) = handle.0.rows.get(row_index) else {
        return FsqlStatus::OutOfBounds;
    };
    let entries = row_entries(row);
    let Some((name, _)) = entries.get(column_index) else {
        return FsqlStatus::OutOfBounds;
    };
    *out = string_handle((*name).to_string());
    FsqlStatus::Ok
}

#[unsafe(no_mangle)]
/// - 中文: 提取结果集中指定位置的值并返回值句柄。
/// - English: Extracts the value at a specific result position and returns it as a value handle.
/// - 中文: 句柄、行列索引和输出指针都必须有效；该接口会克隆底层值。
/// - English: The handle, row and column indices, and output pointer must all be valid; this API clones the underlying value.
/// - 中文: 成功返回的新值句柄由调用方释放，越界时返回 `OutOfBounds`。
/// - English: The new value handle returned on success must be freed by the caller, and out-of-range access returns `OutOfBounds`.
#[unsafe(no_mangle)]
pub extern "C" fn fsql_result_value_at(
    handle: *const FsqlResultHandle,
    row_index: usize,
    column_index: usize,
    out: *mut *mut FsqlValueHandle,
) -> FsqlStatus {
    let Ok(handle) = require_ref(handle) else {
        return FsqlStatus::NullPointer;
    };
    let Ok(out) = require_mut(out) else {
        return FsqlStatus::NullPointer;
    };
    let Some(row) = handle.0.rows.get(row_index) else {
        return FsqlStatus::OutOfBounds;
    };
    let entries = row_entries(row);
    let Some((_, value)) = entries.get(column_index) else {
        return FsqlStatus::OutOfBounds;
    };
    *out = Box::into_raw(Box::new(FsqlValueHandle {
        value: (*value).clone(),
    }));
    FsqlStatus::Ok
}

#[unsafe(no_mangle)]
/// - 中文: 释放结果句柄。
/// - English: Frees a result handle.
/// - 中文: 空指针会被忽略，非空句柄必须来自本 FFI 层分配。
/// - English: Null pointers are ignored, and non-null handles must originate from this FFI layer.
/// - 中文: 释放后不得继续访问结果行、列或任何派生值句柄语义。
/// - English: After freeing, callers must not keep using result-row, column, or derived handle semantics.
#[unsafe(no_mangle)]
pub extern "C" fn fsql_result_free(handle: *mut FsqlResultHandle) {
    if handle.is_null() {
        return;
    }
    unsafe {
        drop(Box::from_raw(handle));
    }
}

#[unsafe(no_mangle)]
/// - 中文: 返回值句柄当前承载的数据类型标签。
/// - English: Returns the data-kind tag carried by a value handle.
/// - 中文: 空句柄会回退为 `Null` 类型，以提供稳定的 C 侧分支行为。
/// - English: A null handle falls back to the `Null` kind to provide stable branching behavior on the C side.
/// - 中文: 该函数只做只读判别，不分配附加资源。
/// - English: This function performs read-only classification and does not allocate extra resources.
#[unsafe(no_mangle)]
pub extern "C" fn fsql_value_kind(handle: *const FsqlValueHandle) -> FsqlValueKind {
    match require_ref(handle) {
        Ok(handle) => match handle.value {
            Value::Null => FsqlValueKind::Null,
            Value::Integer(_) => FsqlValueKind::Integer,
            Value::Float(_) => FsqlValueKind::Float,
            Value::Boolean(_) => FsqlValueKind::Boolean,
            Value::Text(_) => FsqlValueKind::Text,
            Value::Vector(_) => FsqlValueKind::Vector,
            Value::Point(_) => FsqlValueKind::Point,
        },
        Err(_) => FsqlValueKind::Null,
    }
}

#[unsafe(no_mangle)]
/// - 中文: 从值句柄中读取 `i64` 整数。
/// - English: Reads an `i64` integer from a value handle.
/// - 中文: `handle` 和 `out` 必须有效，且底层值类型必须为整数。
/// - English: `handle` and `out` must be valid, and the underlying value type must be integer.
/// - 中文: 成功时写出整数并返回 `Ok`，类型不匹配时返回 `WrongType`。
/// - English: On success it writes the integer and returns `Ok`; on type mismatch it returns `WrongType`.
#[unsafe(no_mangle)]
pub extern "C" fn fsql_value_get_i64(handle: *const FsqlValueHandle, out: *mut i64) -> FsqlStatus {
    let Ok(handle) = require_ref(handle) else {
        return FsqlStatus::NullPointer;
    };
    let Ok(out) = require_mut(out) else {
        return FsqlStatus::NullPointer;
    };
    match handle.value {
        Value::Integer(value) => {
            *out = value;
            FsqlStatus::Ok
        }
        _ => FsqlStatus::WrongType,
    }
}

#[unsafe(no_mangle)]
/// - 中文: 从值句柄中读取 `f64` 浮点数。
/// - English: Reads an `f64` floating-point number from a value handle.
/// - 中文: 输入句柄和输出指针必须有效，且底层值必须是浮点类型。
/// - English: The input handle and output pointer must be valid, and the underlying value must be of float type.
/// - 中文: 成功时写入结果，类型不匹配时返回 `WrongType`。
/// - English: It writes the result on success and returns `WrongType` for type mismatches.
#[unsafe(no_mangle)]
pub extern "C" fn fsql_value_get_f64(handle: *const FsqlValueHandle, out: *mut f64) -> FsqlStatus {
    let Ok(handle) = require_ref(handle) else {
        return FsqlStatus::NullPointer;
    };
    let Ok(out) = require_mut(out) else {
        return FsqlStatus::NullPointer;
    };
    match handle.value {
        Value::Float(value) => {
            *out = value;
            FsqlStatus::Ok
        }
        _ => FsqlStatus::WrongType,
    }
}

#[unsafe(no_mangle)]
/// - 中文: 从值句柄中读取布尔值。
/// - English: Reads a boolean from a value handle.
/// - 中文: 只有布尔类型值才会成功写入 `out`；空指针输入会返回空指针状态。
/// - English: Only boolean values successfully write into `out`; null-pointer inputs return the null-pointer status.
/// - 中文: 成功时返回 `Ok`，类型不匹配时返回 `WrongType`。
/// - English: It returns `Ok` on success and `WrongType` on type mismatch.
#[unsafe(no_mangle)]
pub extern "C" fn fsql_value_get_bool(
    handle: *const FsqlValueHandle,
    out: *mut bool,
) -> FsqlStatus {
    let Ok(handle) = require_ref(handle) else {
        return FsqlStatus::NullPointer;
    };
    let Ok(out) = require_mut(out) else {
        return FsqlStatus::NullPointer;
    };
    match handle.value {
        Value::Boolean(value) => {
            *out = value;
            FsqlStatus::Ok
        }
        _ => FsqlStatus::WrongType,
    }
}

#[unsafe(no_mangle)]
/// - 中文: 从文本值中复制出新的字符串句柄。
/// - English: Copies text data out of a text value into a new string handle.
/// - 中文: `handle` 和 `out` 必须有效，且底层值必须是文本类型。
/// - English: `handle` and `out` must be valid, and the underlying value must be text.
/// - 中文: 成功时返回新的 Rust 分配字符串句柄，调用方必须释放它。
/// - English: On success it returns a newly Rust-allocated string handle that the caller must free.
#[unsafe(no_mangle)]
pub extern "C" fn fsql_value_get_text(
    handle: *const FsqlValueHandle,
    out: *mut *mut FsqlStringHandle,
) -> FsqlStatus {
    let Ok(handle) = require_ref(handle) else {
        return FsqlStatus::NullPointer;
    };
    let Ok(out) = require_mut(out) else {
        return FsqlStatus::NullPointer;
    };
    match &handle.value {
        Value::Text(value) => {
            *out = string_handle(value.clone());
            FsqlStatus::Ok
        }
        _ => FsqlStatus::WrongType,
    }
}

#[unsafe(no_mangle)]
/// - 中文: 从向量值中复制出新的向量句柄。
/// - English: Copies vector data out of a vector value into a new vector handle.
/// - 中文: 输入句柄和输出指针都必须有效，且值类型必须为向量。
/// - English: The input handle and output pointer must both be valid, and the value type must be vector.
/// - 中文: 成功时返回的新向量句柄由调用方用 `fsql_vector_free` 释放。
/// - English: The new vector handle returned on success must be freed by the caller with `fsql_vector_free`.
#[unsafe(no_mangle)]
pub extern "C" fn fsql_value_get_vector(
    handle: *const FsqlValueHandle,
    out: *mut *mut FsqlVectorHandle,
) -> FsqlStatus {
    let Ok(handle) = require_ref(handle) else {
        return FsqlStatus::NullPointer;
    };
    let Ok(out) = require_mut(out) else {
        return FsqlStatus::NullPointer;
    };
    match &handle.value {
        Value::Vector(value) => {
            *out = Box::into_raw(Box::new(FsqlVectorHandle {
                data: value.clone(),
            }));
            FsqlStatus::Ok
        }
        _ => FsqlStatus::WrongType,
    }
}

#[unsafe(no_mangle)]
/// - 中文: 返回向量句柄中的元素数量。
/// - English: Returns the number of elements stored in a vector handle.
/// - 中文: 空句柄会回退为零长度。
/// - English: A null handle falls back to length zero.
/// - 中文: 该查询只读访问向量元数据，不复制底层数组。
/// - English: This query reads vector metadata only and does not copy the underlying array.
#[unsafe(no_mangle)]
pub extern "C" fn fsql_vector_len(handle: *const FsqlVectorHandle) -> usize {
    require_ref(handle)
        .map(|handle| handle.data.len())
        .unwrap_or(0)
}

#[unsafe(no_mangle)]
/// - 中文: 返回向量数据缓冲区的只读指针。
/// - English: Returns a read-only pointer to the vector data buffer.
/// - 中文: 传入句柄必须在使用返回指针期间保持存活；空句柄会返回空指针。
/// - English: The incoming handle must stay alive while the returned pointer is used; null handles return a null pointer.
/// - 中文: 返回指针是借用视图，调用方不能单独释放它。
/// - English: The returned pointer is a borrowed view and must not be freed independently by the caller.
#[unsafe(no_mangle)]
pub extern "C" fn fsql_vector_data(handle: *const FsqlVectorHandle) -> *const f32 {
    require_ref(handle)
        .map(|handle| handle.data.as_ptr())
        .unwrap_or(ptr::null())
}

#[unsafe(no_mangle)]
/// - 中文: 释放向量句柄。
/// - English: Frees a vector handle.
/// - 中文: 空指针会被忽略，非空句柄必须来自本 FFI 层的向量读取接口。
/// - English: Null pointers are ignored, and non-null handles must come from this FFI layer's vector-reading APIs.
/// - 中文: 释放后所有由该句柄导出的数据指针都立即失效。
/// - English: After freeing, all data pointers derived from that handle become invalid immediately.
#[unsafe(no_mangle)]
pub extern "C" fn fsql_vector_free(handle: *mut FsqlVectorHandle) {
    if handle.is_null() {
        return;
    }
    unsafe {
        drop(Box::from_raw(handle));
    }
}

#[unsafe(no_mangle)]
/// - 中文: 从点值中提取经纬度并写入 FFI 点结构。
/// - English: Extracts longitude and latitude from a point value and writes them into the FFI point struct.
/// - 中文: `handle` 与 `out` 都必须有效，且值类型必须是点。
/// - English: `handle` and `out` must both be valid, and the value type must be point.
/// - 中文: 成功时按值复制坐标，类型错误时返回 `WrongType`。
/// - English: On success it copies coordinates by value, and it returns `WrongType` for type errors.
#[unsafe(no_mangle)]
pub extern "C" fn fsql_value_get_point(
    handle: *const FsqlValueHandle,
    out: *mut FsqlPoint,
) -> FsqlStatus {
    let Ok(handle) = require_ref(handle) else {
        return FsqlStatus::NullPointer;
    };
    let Ok(out) = require_mut(out) else {
        return FsqlStatus::NullPointer;
    };
    match handle.value {
        Value::Point(point) => {
            *out = FsqlPoint {
                lon: point.lon,
                lat: point.lat,
            };
            FsqlStatus::Ok
        }
        _ => FsqlStatus::WrongType,
    }
}

#[unsafe(no_mangle)]
/// - 中文: 释放值句柄。
/// - English: Frees a value handle.
/// - 中文: 空指针会被忽略，调用方应只释放由 `fsql_result_value_at` 返回的句柄。
/// - English: Null pointers are ignored, and callers should only free handles returned by `fsql_result_value_at`.
/// - 中文: 释放后该值及其任何派生借用视图都不能继续使用。
/// - English: After freeing, the value and any derived borrowed views must no longer be used.
#[unsafe(no_mangle)]
pub extern "C" fn fsql_value_free(handle: *mut FsqlValueHandle) {
    if handle.is_null() {
        return;
    }
    unsafe {
        drop(Box::from_raw(handle));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ptr;

    #[test]
    /// - 中文: 验证 FFI 数据库执行路径和结果读取接口。
    /// - English: Verifies the FFI database execution path and result-reading APIs.
    /// - 中文: 测试覆盖建表、插入、查询以及文本值提取和清理流程。
    /// - English: The test covers table creation, insertion, querying, text extraction, and cleanup flow.
    /// - 中文: 断言失败会中止测试，副作用仅限进程内分配的 FFI 句柄。
    /// - English: Assertion failures abort the test, and side effects stay limited to in-process FFI allocations.
    fn ffi_database_execute_and_read_values() {
        let mut db = ptr::null_mut();
        let mut error = ptr::null_mut();
        assert_eq!(fsql_db_memory_new(&mut db, &mut error), FsqlStatus::Ok);
        assert!(error.is_null());

        let mut result = ptr::null_mut();
        let create =
            CString::new("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)").unwrap();
        assert_eq!(
            fsql_db_execute(db, create.as_ptr(), &mut result, &mut error),
            FsqlStatus::Ok
        );
        fsql_result_free(result);

        let insert = CString::new("INSERT INTO users VALUES (1, 'Ada')").unwrap();
        assert_eq!(
            fsql_db_execute(db, insert.as_ptr(), &mut result, &mut error),
            FsqlStatus::Ok
        );
        fsql_result_free(result);

        let select = CString::new("SELECT name FROM users WHERE id = 1").unwrap();
        assert_eq!(
            fsql_db_execute(db, select.as_ptr(), &mut result, &mut error),
            FsqlStatus::Ok
        );
        assert_eq!(fsql_result_row_count(result), 1);
        assert_eq!(fsql_result_column_count(result, 0), 1);

        let mut value = ptr::null_mut();
        assert_eq!(
            fsql_result_value_at(result, 0, 0, &mut value),
            FsqlStatus::Ok
        );
        assert_eq!(fsql_value_kind(value), FsqlValueKind::Text);

        let mut text = ptr::null_mut();
        assert_eq!(fsql_value_get_text(value, &mut text), FsqlStatus::Ok);
        let text = unsafe { CStr::from_ptr(fsql_string_data(text)) }
            .to_str()
            .unwrap()
            .to_owned();
        assert_eq!(text, "Ada");

        fsql_string_free(text_handle_from_string(text));
        fsql_value_free(value);
        fsql_result_free(result);
        fsql_db_free(db);
    }

    #[test]
    /// - 中文: 验证连接池相关的 FFI 往返调用可以成功执行。
    /// - English: Verifies that pool-related FFI round trips execute successfully.
    /// - 中文: 该测试覆盖建池、取连接、执行 SQL 和句柄释放流程。
    /// - English: The test covers pool creation, connection checkout, SQL execution, and handle release.
    /// - 中文: 所有副作用都局限在内存数据库和测试期句柄生命周期内。
    /// - English: All side effects remain confined to the in-memory database and test-time handle lifetimes.
    fn ffi_pool_round_trip_works() {
        let mut pool = ptr::null_mut();
        let mut error = ptr::null_mut();
        assert_eq!(
            fsql_pool_memory_new(2, &mut pool, &mut error),
            FsqlStatus::Ok
        );

        let mut connection = ptr::null_mut();
        assert_eq!(
            fsql_pool_get(pool, &mut connection, &mut error),
            FsqlStatus::Ok
        );

        let create =
            CString::new("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)").unwrap();
        let mut result = ptr::null_mut();
        assert_eq!(
            fsql_connection_execute(connection, create.as_ptr(), &mut result, &mut error),
            FsqlStatus::Ok
        );
        fsql_result_free(result);
        fsql_connection_free(connection);
        fsql_pool_free(pool);
    }

    #[test]
    /// - 中文: 验证 FFI 调用失败时会返回错误状态并填充错误字符串。
    /// - English: Verifies that failed FFI calls return an error status and populate an error string.
    /// - 中文: 测试通过查询缺失表触发错误路径。
    /// - English: The test triggers the error path by querying a missing table.
    /// - 中文: 结束时会释放错误句柄和数据库句柄，避免测试内存泄漏。
    /// - English: It frees the error and database handles at the end to avoid test-time memory leaks.
    fn ffi_reports_errors() {
        let mut db = ptr::null_mut();
        let mut error = ptr::null_mut();
        assert_eq!(fsql_db_memory_new(&mut db, &mut error), FsqlStatus::Ok);

        let mut result = ptr::null_mut();
        let bad = CString::new("SELECT * FROM missing").unwrap();
        assert_eq!(
            fsql_db_execute(db, bad.as_ptr(), &mut result, &mut error),
            FsqlStatus::Error
        );
        assert!(!error.is_null());
        let message = unsafe { CStr::from_ptr(fsql_string_data(error)) }
            .to_str()
            .unwrap();
        assert!(message.contains("unknown table"));

        fsql_string_free(error);
        fsql_db_free(db);
    }

    /// - 中文: 为测试辅助地把 Rust 字符串转换为 FFI 字符串句柄。
    /// - English: Converts a Rust string into an FFI string handle for test helpers.
    /// - 中文: 该辅助函数直接复用正式的 `string_handle` 分配路径。
    /// - English: This helper directly reuses the production `string_handle` allocation path.
    /// - 中文: 返回句柄需要像普通 FFI 字符串一样释放。
    /// - English: The returned handle must be freed like any other FFI string handle.
    fn text_handle_from_string(text: String) -> *mut FsqlStringHandle {
        string_handle(text)
    }
}

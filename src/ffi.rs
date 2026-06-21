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

fn set_error(error_out: *mut *mut FsqlStringHandle, message: impl Into<String>) {
    if error_out.is_null() {
        return;
    }
    unsafe {
        *error_out = string_handle(message.into());
    }
}

fn string_handle(message: String) -> *mut FsqlStringHandle {
    let sanitized = message.replace('\0', " ");
    let cstring = CString::new(sanitized).expect("CString sanitization removed nul bytes");
    Box::into_raw(Box::new(FsqlStringHandle(cstring)))
}

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

fn require_mut<'a, T>(ptr: *mut T) -> Result<&'a mut T, FsqlStatus> {
    if ptr.is_null() {
        return Err(FsqlStatus::NullPointer);
    }
    Ok(unsafe { &mut *ptr })
}

fn require_ref<'a, T>(ptr: *const T) -> Result<&'a T, FsqlStatus> {
    if ptr.is_null() {
        return Err(FsqlStatus::NullPointer);
    }
    Ok(unsafe { &*ptr })
}

fn with_result<T>(
    error_out: *mut *mut FsqlStringHandle,
    f: impl FnOnce() -> crate::Result<T>,
) -> Result<T, FsqlStatus> {
    f().map_err(|error| {
        set_error(error_out, error.to_string());
        FsqlStatus::Error
    })
}

fn row_entries(row: &Row) -> Vec<(&str, &Value)> {
    row.iter().map(|(key, value)| (key.as_str(), value)).collect()
}

#[unsafe(no_mangle)]
pub extern "C" fn fsql_string_data(handle: *const FsqlStringHandle) -> *const c_char {
    match require_ref(handle) {
        Ok(handle) => handle.0.as_ptr(),
        Err(_) => ptr::null(),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn fsql_string_free(handle: *mut FsqlStringHandle) {
    if handle.is_null() {
        return;
    }
    unsafe {
        drop(Box::from_raw(handle));
    }
}

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

#[unsafe(no_mangle)]
pub extern "C" fn fsql_db_free(handle: *mut FsqlDatabaseHandle) {
    if handle.is_null() {
        return;
    }
    unsafe {
        drop(Box::from_raw(handle));
    }
}

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
    match with_result(error_out, || ConnectionPool::open(PathBuf::from(path), max_connections)) {
        Ok(pool) => {
            *out = Box::into_raw(Box::new(FsqlPoolHandle(pool)));
            FsqlStatus::Ok
        }
        Err(status) => status,
    }
}

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

#[unsafe(no_mangle)]
pub extern "C" fn fsql_pool_free(handle: *mut FsqlPoolHandle) {
    if handle.is_null() {
        return;
    }
    unsafe {
        drop(Box::from_raw(handle));
    }
}

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
pub extern "C" fn fsql_result_affected_rows(handle: *const FsqlResultHandle) -> usize {
    require_ref(handle).map(|handle| handle.0.affected_rows).unwrap_or(0)
}

#[unsafe(no_mangle)]
pub extern "C" fn fsql_result_row_count(handle: *const FsqlResultHandle) -> usize {
    require_ref(handle).map(|handle| handle.0.rows.len()).unwrap_or(0)
}

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
pub extern "C" fn fsql_result_free(handle: *mut FsqlResultHandle) {
    if handle.is_null() {
        return;
    }
    unsafe {
        drop(Box::from_raw(handle));
    }
}

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
pub extern "C" fn fsql_value_get_i64(
    handle: *const FsqlValueHandle,
    out: *mut i64,
) -> FsqlStatus {
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
pub extern "C" fn fsql_value_get_f64(
    handle: *const FsqlValueHandle,
    out: *mut f64,
) -> FsqlStatus {
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
pub extern "C" fn fsql_vector_len(handle: *const FsqlVectorHandle) -> usize {
    require_ref(handle).map(|handle| handle.data.len()).unwrap_or(0)
}

#[unsafe(no_mangle)]
pub extern "C" fn fsql_vector_data(handle: *const FsqlVectorHandle) -> *const f32 {
    require_ref(handle)
        .map(|handle| handle.data.as_ptr())
        .unwrap_or(ptr::null())
}

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
    fn ffi_database_execute_and_read_values() {
        let mut db = ptr::null_mut();
        let mut error = ptr::null_mut();
        assert_eq!(fsql_db_memory_new(&mut db, &mut error), FsqlStatus::Ok);
        assert!(error.is_null());

        let mut result = ptr::null_mut();
        let create = CString::new("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)").unwrap();
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
        assert_eq!(fsql_result_value_at(result, 0, 0, &mut value), FsqlStatus::Ok);
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
    fn ffi_pool_round_trip_works() {
        let mut pool = ptr::null_mut();
        let mut error = ptr::null_mut();
        assert_eq!(fsql_pool_memory_new(2, &mut pool, &mut error), FsqlStatus::Ok);

        let mut connection = ptr::null_mut();
        assert_eq!(fsql_pool_get(pool, &mut connection, &mut error), FsqlStatus::Ok);

        let create = CString::new("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)").unwrap();
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

    fn text_handle_from_string(text: String) -> *mut FsqlStringHandle {
        string_handle(text)
    }
}

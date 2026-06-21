import ctypes
from ctypes import byref, c_bool, c_char_p, c_double, c_int64, c_size_t, c_void_p
from pathlib import Path

lib = ctypes.CDLL(str(Path(__file__).parent / "target" / "debug" / "libfsql.dylib"))

class FsqlPoint(ctypes.Structure):
    _fields_ = [("lon", c_double), ("lat", c_double)]

lib.fsql_db_memory_new.argtypes = [ctypes.POINTER(c_void_p), ctypes.POINTER(c_void_p)]
lib.fsql_db_memory_new.restype = ctypes.c_int
lib.fsql_db_execute.argtypes = [c_void_p, c_char_p, ctypes.POINTER(c_void_p), ctypes.POINTER(c_void_p)]
lib.fsql_db_execute.restype = ctypes.c_int
lib.fsql_db_free.argtypes = [c_void_p]
lib.fsql_result_free.argtypes = [c_void_p]
lib.fsql_result_column_count.argtypes = [c_void_p, c_size_t]
lib.fsql_result_column_count.restype = c_size_t
lib.fsql_result_column_name.argtypes = [c_void_p, c_size_t, c_size_t, ctypes.POINTER(c_void_p)]
lib.fsql_result_column_name.restype = ctypes.c_int
lib.fsql_result_value_at.argtypes = [c_void_p, c_size_t, c_size_t, ctypes.POINTER(c_void_p)]
lib.fsql_result_value_at.restype = ctypes.c_int
lib.fsql_value_kind.argtypes = [c_void_p]
lib.fsql_value_kind.restype = ctypes.c_int
lib.fsql_value_get_i64.argtypes = [c_void_p, ctypes.POINTER(c_int64)]
lib.fsql_value_get_i64.restype = ctypes.c_int
lib.fsql_value_get_text.argtypes = [c_void_p, ctypes.POINTER(c_void_p)]
lib.fsql_value_get_text.restype = ctypes.c_int
lib.fsql_value_free.argtypes = [c_void_p]
lib.fsql_string_data.argtypes = [c_void_p]
lib.fsql_string_data.restype = c_char_p
lib.fsql_string_free.argtypes = [c_void_p]

STATUS_OK = 0
VALUE_INTEGER = 1
VALUE_TEXT = 4


def check(status, error):
    if status == STATUS_OK:
        return
    message = ctypes.string_at(lib.fsql_string_data(error)).decode()
    lib.fsql_string_free(error)
    raise RuntimeError(message)


def execute(db, sql):
    result = c_void_p()
    error = c_void_p()
    check(lib.fsql_db_execute(db, sql.encode(), byref(result), byref(error)), error)
    return result


def main():
    db = c_void_p()
    error = c_void_p()
    check(lib.fsql_db_memory_new(byref(db), byref(error)), error)
    try:
        result = execute(db, "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)")
        lib.fsql_result_free(result)
        result = execute(db, "INSERT INTO users VALUES (1, 'Ada')")
        lib.fsql_result_free(result)
        result = execute(db, "SELECT id, name FROM users")
        try:
            for col in range(lib.fsql_result_column_count(result, 0)):
                name = c_void_p()
                value = c_void_p()
                check(lib.fsql_result_column_name(result, 0, col, byref(name)), None)
                check(lib.fsql_result_value_at(result, 0, col, byref(value)), None)
                key = ctypes.string_at(lib.fsql_string_data(name)).decode()
                kind = lib.fsql_value_kind(value)
                if kind == VALUE_INTEGER:
                    v = c_int64()
                    check(lib.fsql_value_get_i64(value, byref(v)), None)
                    print(f"{key}={v.value}")
                elif kind == VALUE_TEXT:
                    text = c_void_p()
                    check(lib.fsql_value_get_text(value, byref(text)), None)
                    print(f"{key}={ctypes.string_at(lib.fsql_string_data(text)).decode()}")
                    lib.fsql_string_free(text)
                lib.fsql_value_free(value)
                lib.fsql_string_free(name)
        finally:
            lib.fsql_result_free(result)
    finally:
        lib.fsql_db_free(db)


if __name__ == "__main__":
    main()

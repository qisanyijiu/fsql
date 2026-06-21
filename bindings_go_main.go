package main

/*
#cgo CFLAGS: -I${SRCDIR}
#cgo LDFLAGS: -L${SRCDIR}/target/debug -lfsql
#include "include_fsql.h"
#include <stdlib.h>
*/
import "C"

import (
    "fmt"
    "unsafe"
)

func check(status C.FsqlStatus, err *C.FsqlStringHandle) {
    if status == C.FSQL_STATUS_OK {
        return
    }
    defer C.fsql_string_free(err)
    panic(C.GoString(C.fsql_string_data(err)))
}

func main() {
    var db *C.FsqlDatabaseHandle
    var err *C.FsqlStringHandle
    check(C.fsql_db_memory_new(&db, &err), err)
    defer C.fsql_db_free(db)

    exec := func(sql string) *C.FsqlResultHandle {
        csql := C.CString(sql)
        defer C.free(unsafe.Pointer(csql))
        var result *C.FsqlResultHandle
        check(C.fsql_db_execute(db, csql, &result, &err), err)
        return result
    }

    result := exec("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)")
    C.fsql_result_free(result)
    result = exec("INSERT INTO users VALUES (1, 'Ada')")
    C.fsql_result_free(result)
    result = exec("SELECT id, name FROM users")
    defer C.fsql_result_free(result)

    cols := C.fsql_result_column_count(result, 0)
    for i := C.size_t(0); i < cols; i++ {
        var name *C.FsqlStringHandle
        var value *C.FsqlValueHandle
        check(C.fsql_result_column_name(result, 0, i, &name), nil)
        check(C.fsql_result_value_at(result, 0, i, &value), nil)
        key := C.GoString(C.fsql_string_data(name))
        switch C.fsql_value_kind(value) {
        case C.FSQL_VALUE_INTEGER:
            var v C.int64_t
            check(C.fsql_value_get_i64(value, &v), nil)
            fmt.Printf("%s=%d\n", key, int64(v))
        case C.FSQL_VALUE_TEXT:
            var text *C.FsqlStringHandle
            check(C.fsql_value_get_text(value, &text), nil)
            fmt.Printf("%s=%s\n", key, C.GoString(C.fsql_string_data(text)))
            C.fsql_string_free(text)
        }
        C.fsql_value_free(value)
        C.fsql_string_free(name)
    }
}

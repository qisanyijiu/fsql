import Foundation

@_silgen_name("fsql_db_memory_new")
func fsql_db_memory_new(_ out: UnsafeMutablePointer<OpaquePointer?>!, _ errorOut: UnsafeMutablePointer<OpaquePointer?>!) -> Int32
@_silgen_name("fsql_db_execute")
func fsql_db_execute(_ db: OpaquePointer!, _ sql: UnsafePointer<CChar>!, _ out: UnsafeMutablePointer<OpaquePointer?>!, _ errorOut: UnsafeMutablePointer<OpaquePointer?>!) -> Int32
@_silgen_name("fsql_db_free")
func fsql_db_free(_ db: OpaquePointer!)
@_silgen_name("fsql_result_free")
func fsql_result_free(_ result: OpaquePointer!)
@_silgen_name("fsql_result_column_count")
func fsql_result_column_count(_ result: OpaquePointer!, _ rowIndex: Int) -> Int
@_silgen_name("fsql_result_column_name")
func fsql_result_column_name(_ result: OpaquePointer!, _ rowIndex: Int, _ columnIndex: Int, _ out: UnsafeMutablePointer<OpaquePointer?>!) -> Int32
@_silgen_name("fsql_result_value_at")
func fsql_result_value_at(_ result: OpaquePointer!, _ rowIndex: Int, _ columnIndex: Int, _ out: UnsafeMutablePointer<OpaquePointer?>!) -> Int32
@_silgen_name("fsql_value_kind")
func fsql_value_kind(_ value: OpaquePointer!) -> Int32
@_silgen_name("fsql_value_get_i64")
func fsql_value_get_i64(_ value: OpaquePointer!, _ out: UnsafeMutablePointer<Int64>!) -> Int32
@_silgen_name("fsql_value_get_text")
func fsql_value_get_text(_ value: OpaquePointer!, _ out: UnsafeMutablePointer<OpaquePointer?>!) -> Int32
@_silgen_name("fsql_value_free")
func fsql_value_free(_ value: OpaquePointer!)
@_silgen_name("fsql_string_data")
func fsql_string_data(_ string: OpaquePointer!) -> UnsafePointer<CChar>!
@_silgen_name("fsql_string_free")
func fsql_string_free(_ string: OpaquePointer!)

let statusOK: Int32 = 0
let valueInteger: Int32 = 1
let valueText: Int32 = 4

func check(_ status: Int32, _ error: OpaquePointer?) {
    guard status == statusOK else {
        if let error = error {
            let message = String(cString: fsql_string_data(error))
            fsql_string_free(error)
            fatalError(message)
        }
        fatalError("ffi error")
    }
}

var db: OpaquePointer?
var error: OpaquePointer?
check(fsql_db_memory_new(&db, &error), error)

func execute(_ sql: String) -> OpaquePointer? {
    var result: OpaquePointer?
    sql.withCString { cString in
        check(fsql_db_execute(db, cString, &result, &error), error)
    }
    return result
}

var result = execute("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)")
fsql_result_free(result)
result = execute("INSERT INTO users VALUES (1, 'Ada')")
fsql_result_free(result)
result = execute("SELECT id, name FROM users")
if let result {
    for col in 0..<fsql_result_column_count(result, 0) {
        var name: OpaquePointer?
        var value: OpaquePointer?
        check(fsql_result_column_name(result, 0, col, &name), nil)
        check(fsql_result_value_at(result, 0, col, &value), nil)
        let key = String(cString: fsql_string_data(name))
        if fsql_value_kind(value) == valueInteger {
            var v: Int64 = 0
            check(fsql_value_get_i64(value, &v), nil)
            print("\(key)=\(v)")
        } else if fsql_value_kind(value) == valueText {
            var text: OpaquePointer?
            check(fsql_value_get_text(value, &text), nil)
            print("\(key)=\(String(cString: fsql_string_data(text)))")
            fsql_string_free(text)
        }
        fsql_value_free(value)
        fsql_string_free(name)
    }
    fsql_result_free(result)
}
fsql_db_free(db)

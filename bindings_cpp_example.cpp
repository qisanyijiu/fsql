#include "include_fsql.h"

#include <cstdint>
#include <iostream>
#include <stdexcept>
#include <string>

class StringHandle {
public:
  explicit StringHandle(FsqlStringHandle *handle = nullptr) : handle_(handle) {}
  ~StringHandle() { if (handle_) fsql_string_free(handle_); }
  StringHandle(const StringHandle &) = delete;
  StringHandle &operator=(const StringHandle &) = delete;
  StringHandle(StringHandle &&other) noexcept : handle_(other.handle_) { other.handle_ = nullptr; }
  const char *c_str() const { return fsql_string_data(handle_); }
  FsqlStringHandle **out() { return &handle_; }
private:
  FsqlStringHandle *handle_;
};

static void check(FsqlStatus status, FsqlStringHandle *error) {
  if (status == FSQL_STATUS_OK) return;
  StringHandle owned(error);
  throw std::runtime_error(owned.c_str() ? owned.c_str() : "ffi error");
}

int main() {
  FsqlDatabaseHandle *db = nullptr;
  FsqlStringHandle *error = nullptr;
  check(fsql_db_memory_new(&db, &error), error);

  FsqlResultHandle *result = nullptr;
  check(fsql_db_execute(db, "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)", &result, &error), error);
  fsql_result_free(result);
  check(fsql_db_execute(db, "INSERT INTO users VALUES (1, 'Ada')", &result, &error), error);
  fsql_result_free(result);
  check(fsql_db_execute(db, "SELECT id, name FROM users", &result, &error), error);

  for (size_t col = 0; col < fsql_result_column_count(result, 0); ++col) {
    StringHandle name;
    FsqlValueHandle *value = nullptr;
    check(fsql_result_column_name(result, 0, col, name.out()), nullptr);
    check(fsql_result_value_at(result, 0, col, &value), nullptr);
    std::cout << name.c_str() << "=";
    if (fsql_value_kind(value) == FSQL_VALUE_INTEGER) {
      int64_t v = 0;
      fsql_value_get_i64(value, &v);
      std::cout << v;
    } else if (fsql_value_kind(value) == FSQL_VALUE_TEXT) {
      StringHandle text;
      fsql_value_get_text(value, text.out());
      std::cout << text.c_str();
    }
    std::cout << std::endl;
    fsql_value_free(value);
  }

  fsql_result_free(result);
  fsql_db_free(db);
  return 0;
}

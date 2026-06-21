#include "include_fsql.h"

#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>

static void die(FsqlStringHandle *error) {
  fprintf(stderr, "%s\n", fsql_string_data(error));
  fsql_string_free(error);
  exit(1);
}

int main(void) {
  FsqlDatabaseHandle *db = NULL;
  FsqlStringHandle *error = NULL;
  if (fsql_db_memory_new(&db, &error) != FSQL_STATUS_OK) {
    die(error);
  }

  FsqlResultHandle *result = NULL;
  if (fsql_db_execute(db, "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)", &result, &error) != FSQL_STATUS_OK) {
    die(error);
  }
  fsql_result_free(result);

  if (fsql_db_execute(db, "INSERT INTO users VALUES (1, 'Ada')", &result, &error) != FSQL_STATUS_OK) {
    die(error);
  }
  fsql_result_free(result);

  if (fsql_db_execute(db, "SELECT id, name FROM users WHERE id = 1", &result, &error) != FSQL_STATUS_OK) {
    die(error);
  }

  for (size_t col = 0; col < fsql_result_column_count(result, 0); ++col) {
    FsqlStringHandle *name = NULL;
    FsqlValueHandle *value = NULL;
    fsql_result_column_name(result, 0, col, &name);
    fsql_result_value_at(result, 0, col, &value);
    printf("%s=", fsql_string_data(name));
    if (fsql_value_kind(value) == FSQL_VALUE_INTEGER) {
      int64_t v = 0;
      fsql_value_get_i64(value, &v);
      printf("%lld", (long long)v);
    } else if (fsql_value_kind(value) == FSQL_VALUE_TEXT) {
      FsqlStringHandle *text = NULL;
      fsql_value_get_text(value, &text);
      printf("%s", fsql_string_data(text));
      fsql_string_free(text);
    }
    printf("\n");
    fsql_value_free(value);
    fsql_string_free(name);
  }

  fsql_result_free(result);
  fsql_db_free(db);
  return 0;
}

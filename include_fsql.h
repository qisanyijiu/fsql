#ifndef FSQL_H
#define FSQL_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef enum FsqlStatus {
  FSQL_STATUS_OK = 0,
  FSQL_STATUS_NULL_POINTER = 1,
  FSQL_STATUS_INVALID_UTF8 = 2,
  FSQL_STATUS_ERROR = 3,
  FSQL_STATUS_OUT_OF_BOUNDS = 4,
  FSQL_STATUS_WRONG_TYPE = 5
} FsqlStatus;

typedef enum FsqlValueKind {
  FSQL_VALUE_NULL = 0,
  FSQL_VALUE_INTEGER = 1,
  FSQL_VALUE_FLOAT = 2,
  FSQL_VALUE_BOOLEAN = 3,
  FSQL_VALUE_TEXT = 4,
  FSQL_VALUE_VECTOR = 5,
  FSQL_VALUE_POINT = 6
} FsqlValueKind;

typedef struct FsqlDatabaseHandle FsqlDatabaseHandle;
typedef struct FsqlPoolHandle FsqlPoolHandle;
typedef struct FsqlConnectionHandle FsqlConnectionHandle;
typedef struct FsqlResultHandle FsqlResultHandle;
typedef struct FsqlStringHandle FsqlStringHandle;
typedef struct FsqlValueHandle FsqlValueHandle;
typedef struct FsqlVectorHandle FsqlVectorHandle;

typedef struct FsqlPoint {
  double lon;
  double lat;
} FsqlPoint;

const char *fsql_string_data(const FsqlStringHandle *handle);
void fsql_string_free(FsqlStringHandle *handle);

FsqlStatus fsql_db_memory_new(FsqlDatabaseHandle **out, FsqlStringHandle **error_out);
FsqlStatus fsql_db_open(const char *path, FsqlDatabaseHandle **out, FsqlStringHandle **error_out);
FsqlStatus fsql_db_execute(FsqlDatabaseHandle *database, const char *sql, FsqlResultHandle **out, FsqlStringHandle **error_out);
void fsql_db_free(FsqlDatabaseHandle *handle);

FsqlStatus fsql_pool_memory_new(size_t max_connections, FsqlPoolHandle **out, FsqlStringHandle **error_out);
FsqlStatus fsql_pool_open(const char *path, size_t max_connections, FsqlPoolHandle **out, FsqlStringHandle **error_out);
FsqlStatus fsql_pool_get(const FsqlPoolHandle *pool, FsqlConnectionHandle **out, FsqlStringHandle **error_out);
FsqlStatus fsql_pool_try_get(const FsqlPoolHandle *pool, FsqlConnectionHandle **out, FsqlStringHandle **error_out);
void fsql_pool_free(FsqlPoolHandle *handle);

FsqlStatus fsql_connection_execute(const FsqlConnectionHandle *connection, const char *sql, FsqlResultHandle **out, FsqlStringHandle **error_out);
void fsql_connection_free(FsqlConnectionHandle *handle);

size_t fsql_result_affected_rows(const FsqlResultHandle *handle);
size_t fsql_result_row_count(const FsqlResultHandle *handle);
FsqlStatus fsql_result_message(const FsqlResultHandle *handle, FsqlStringHandle **out);
size_t fsql_result_column_count(const FsqlResultHandle *handle, size_t row_index);
FsqlStatus fsql_result_column_name(const FsqlResultHandle *handle, size_t row_index, size_t column_index, FsqlStringHandle **out);
FsqlStatus fsql_result_value_at(const FsqlResultHandle *handle, size_t row_index, size_t column_index, FsqlValueHandle **out);
void fsql_result_free(FsqlResultHandle *handle);

FsqlValueKind fsql_value_kind(const FsqlValueHandle *handle);
FsqlStatus fsql_value_get_i64(const FsqlValueHandle *handle, int64_t *out);
FsqlStatus fsql_value_get_f64(const FsqlValueHandle *handle, double *out);
FsqlStatus fsql_value_get_bool(const FsqlValueHandle *handle, bool *out);
FsqlStatus fsql_value_get_text(const FsqlValueHandle *handle, FsqlStringHandle **out);
FsqlStatus fsql_value_get_vector(const FsqlValueHandle *handle, FsqlVectorHandle **out);
FsqlStatus fsql_value_get_point(const FsqlValueHandle *handle, FsqlPoint *out);
void fsql_value_free(FsqlValueHandle *handle);

size_t fsql_vector_len(const FsqlVectorHandle *handle);
const float *fsql_vector_data(const FsqlVectorHandle *handle);
void fsql_vector_free(FsqlVectorHandle *handle);

#ifdef __cplusplus
}
#endif

#endif

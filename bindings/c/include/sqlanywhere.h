#ifndef SQLANYWHERE_EXPERIMENTAL_H
#define SQLANYWHERE_EXPERIMENTAL_H

#include <stdint.h>

#define SQLANYWHERE_INT 1

#define SQLANYWHERE_FLOAT 2

#define SQLANYWHERE_TEXT 3

#define SQLANYWHERE_BLOB 4

#define SQLANYWHERE_NULL 5

typedef struct sqlanywhere_connection sqlanywhere_connection;

typedef struct sqlanywhere_database sqlanywhere_database;

typedef struct sqlanywhere_row sqlanywhere_row;

typedef struct sqlanywhere_rows sqlanywhere_rows;

typedef struct sqlanywhere_rows_future sqlanywhere_rows_future;

typedef struct sqlanywhere_stmt sqlanywhere_stmt;

typedef const sqlanywhere_database *sqlanywhere_database_t;

typedef struct {
  int frame_no;
  int frames_synced;
} replicated;

typedef struct {
  const char *db_path;
  const char *primary_url;
  const char *auth_token;
  char read_your_writes;
  const char *encryption_key;
  int sync_interval;
  char with_webpki;
  char offline;
  const char *remote_encryption_key;
} sqlanywhere_config;

typedef const sqlanywhere_connection *sqlanywhere_connection_t;

typedef const sqlanywhere_stmt *sqlanywhere_stmt_t;

typedef const sqlanywhere_rows *sqlanywhere_rows_t;

typedef const sqlanywhere_rows_future *sqlanywhere_rows_future_t;

typedef const sqlanywhere_row *sqlanywhere_row_t;

typedef struct {
  const char *ptr;
  int len;
} blob;

#ifdef __cplusplus
extern "C" {
#endif // __cplusplus

int sqlanywhere_enable_internal_tracing(void);

int sqlanywhere_sync(sqlanywhere_database_t db, const char **out_err_msg);

int sqlanywhere_sync2(sqlanywhere_database_t db, replicated *out_replicated, const char **out_err_msg);

int sqlanywhere_open_sync(const char *db_path,
                          const char *primary_url,
                          const char *auth_token,
                          char read_your_writes,
                          const char *encryption_key,
                          sqlanywhere_database_t *out_db,
                          const char **out_err_msg);

int sqlanywhere_open_sync_with_webpki(const char *db_path,
                                      const char *primary_url,
                                      const char *auth_token,
                                      char read_your_writes,
                                      const char *encryption_key,
                                      sqlanywhere_database_t *out_db,
                                      const char **out_err_msg);

int sqlanywhere_open_sync_with_config(sqlanywhere_config config,
                                      sqlanywhere_database_t *out_db,
                                      const char **out_err_msg);

int sqlanywhere_open_ext(const char *url, sqlanywhere_database_t *out_db, const char **out_err_msg);

int sqlanywhere_open_file(const char *url, sqlanywhere_database_t *out_db, const char **out_err_msg);

int sqlanywhere_open_remote(const char *url,
                            const char *auth_token,
                            sqlanywhere_database_t *out_db,
                            const char **out_err_msg);

int sqlanywhere_open_remote_with_remote_encryption(const char *url,
                                                   const char *auth_token,
                                                   const char *remote_encryption_key,
                                                   sqlanywhere_database_t *out_db,
                                                   const char **out_err_msg);

int sqlanywhere_open_remote_with_webpki(const char *url,
                                        const char *auth_token,
                                        sqlanywhere_database_t *out_db,
                                        const char **out_err_msg);

void sqlanywhere_close(sqlanywhere_database_t db);

int sqlanywhere_connect(sqlanywhere_database_t db, sqlanywhere_connection_t *out_conn, const char **out_err_msg);

int sqlanywhere_load_extension(sqlanywhere_connection_t conn,
                               const char *path,
                               const char *entry_point,
                               const char **out_err_msg);

int sqlanywhere_set_reserved_bytes(sqlanywhere_connection_t conn, int32_t reserved_bytes, const char **out_err_msg);

int sqlanywhere_get_reserved_bytes(sqlanywhere_connection_t conn, int32_t *reserved_bytes, const char **out_err_msg);

int sqlanywhere_reset(sqlanywhere_connection_t conn, const char **out_err_msg);

void sqlanywhere_disconnect(sqlanywhere_connection_t conn);

int sqlanywhere_prepare(sqlanywhere_connection_t conn,
                        const char *sql,
                        sqlanywhere_stmt_t *out_stmt,
                        const char **out_err_msg);

int sqlanywhere_bind_int(sqlanywhere_stmt_t stmt, int idx, long long value, const char **out_err_msg);

int sqlanywhere_bind_float(sqlanywhere_stmt_t stmt, int idx, double value, const char **out_err_msg);

int sqlanywhere_bind_null(sqlanywhere_stmt_t stmt, int idx, const char **out_err_msg);

int sqlanywhere_bind_string(sqlanywhere_stmt_t stmt, int idx, const char *value, const char **out_err_msg);

int sqlanywhere_bind_blob(sqlanywhere_stmt_t stmt,
                          int idx,
                          const unsigned char *value,
                          int value_len,
                          const char **out_err_msg);

int sqlanywhere_query_stmt(sqlanywhere_stmt_t stmt, sqlanywhere_rows_t *out_rows, const char **out_err_msg);

int sqlanywhere_execute_stmt(sqlanywhere_stmt_t stmt, const char **out_err_msg);

int sqlanywhere_reset_stmt(sqlanywhere_stmt_t stmt, const char **out_err_msg);

void sqlanywhere_free_stmt(sqlanywhere_stmt_t stmt);

int sqlanywhere_query(sqlanywhere_connection_t conn,
                      const char *sql,
                      sqlanywhere_rows_t *out_rows,
                      const char **out_err_msg);

int sqlanywhere_execute(sqlanywhere_connection_t conn, const char *sql, const char **out_err_msg);

void sqlanywhere_free_rows(sqlanywhere_rows_t res);

void sqlanywhere_free_rows_future(sqlanywhere_rows_future_t res);

void sqlanywhere_wait_result(sqlanywhere_rows_future_t res);

int sqlanywhere_column_count(sqlanywhere_rows_t res);

int sqlanywhere_column_name(sqlanywhere_rows_t res, int col, const char **out_name, const char **out_err_msg);

int sqlanywhere_column_type(sqlanywhere_rows_t res,
                            sqlanywhere_row_t row,
                            int col,
                            int *out_type,
                            const char **out_err_msg);

uint64_t sqlanywhere_changes(sqlanywhere_connection_t conn);

int64_t sqlanywhere_last_insert_rowid(sqlanywhere_connection_t conn);

int sqlanywhere_next_row(sqlanywhere_rows_t res, sqlanywhere_row_t *out_row, const char **out_err_msg);

void sqlanywhere_free_row(sqlanywhere_row_t res);

int sqlanywhere_get_string(sqlanywhere_row_t res, int col, const char **out_value, const char **out_err_msg);

void sqlanywhere_free_string(const char *ptr);

int sqlanywhere_get_int(sqlanywhere_row_t res, int col, long long *out_value, const char **out_err_msg);

int sqlanywhere_get_float(sqlanywhere_row_t res, int col, double *out_value, const char **out_err_msg);

int sqlanywhere_get_blob(sqlanywhere_row_t res, int col, blob *out_blob, const char **out_err_msg);

void sqlanywhere_free_blob(blob b);

#ifdef __cplusplus
} // extern "C"
#endif // __cplusplus

#endif /* SQLANYWHERE_EXPERIMENTAL_H */

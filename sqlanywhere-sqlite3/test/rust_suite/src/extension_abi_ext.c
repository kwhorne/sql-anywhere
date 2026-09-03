/*
** A loadable extension used by extension_abi.rs to probe the SQL Anywhere
** extension thunk from the outside, the way a real third-party extension
** does.
**
** It deliberately reaches sqlanywhere_api_routines only through the public
** macros in sqlite3ext.h, so the test fails if those stop working for an
** out-of-tree extension.
*/
#include "sqlite3ext.h"
SQLITE_EXTENSION_INIT1
SQLANYWHERE_EXTENSION_INIT1

static int gCloseHookRan = 0;

/* The interface version this host library advertises, or -1 if the host
** supplied no SQL Anywhere thunk at all. */
static void abiVersionFunc(sqlite3_context *ctx, int argc, sqlite3_value **argv){
  (void)argc; (void)argv;
  sqlite3_result_int(ctx,
      SQLANYWHERE_API_ATLEAST(1) ? sqlanywhere_api->iVersion : -1);
}

/* SQLANYWHERE_API_ATLEAST(?1), so the test can ask about an interface version
** that does not exist yet.  An extension built against a newer sqlite3ext.h
** than its host must answer 0 here rather than read past the thunk. */
static void abiAtLeastFunc(sqlite3_context *ctx, int argc, sqlite3_value **argv){
  int v;
  (void)argc;
  v = sqlite3_value_int(argv[0]);
  sqlite3_result_int(ctx, SQLANYWHERE_API_ATLEAST(v) ? 1 : 0);
}

static void closeHookCb(void *pArg, sqlite3 *db){
  (void)db;
  *(int*)pArg = 1;
}

/* Install close_hook through the thunk; 1 if the host offers it. */
static void abiInstallCloseHookFunc(
  sqlite3_context *ctx, int argc, sqlite3_value **argv
){
  (void)argc; (void)argv;
  if( SQLANYWHERE_API_ATLEAST(1) ){
    sqlanywhere_close_hook(sqlite3_context_db_handle(ctx), closeHookCb,
                           &gCloseHookRan);
    sqlite3_result_int(ctx, 1);
  }else{
    sqlite3_result_int(ctx, 0);
  }
}

#ifdef _WIN32
__declspec(dllexport)
#endif
int sqlite3_sqlanywhereabitest_init(
  sqlite3 *db,
  char **pzErrMsg,
  const sqlite3_api_routines *pApi,
  const sqlanywhere_api_routines *pSqlanywhereApi
){
  int rc;
  SQLITE_EXTENSION_INIT2(pApi);
  SQLANYWHERE_EXTENSION_INIT2(pSqlanywhereApi);
  (void)pzErrMsg;

  rc = sqlite3_create_function(db, "sa_abi_version", 0, SQLITE_UTF8, 0,
                               abiVersionFunc, 0, 0);
  if( rc==SQLITE_OK ){
    rc = sqlite3_create_function(db, "sa_abi_atleast", 1, SQLITE_UTF8, 0,
                                 abiAtLeastFunc, 0, 0);
  }
  if( rc==SQLITE_OK ){
    rc = sqlite3_create_function(db, "sa_abi_install_close_hook", 0,
                                 SQLITE_UTF8, 0, abiInstallCloseHookFunc, 0, 0);
  }
  return rc;
}

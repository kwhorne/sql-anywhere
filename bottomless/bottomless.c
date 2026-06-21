#ifdef SQLANYWHERE_ENABLE_BOTTOMLESS_WAL

#include "sqlite3ext.h"
SQLITE_EXTENSION_INIT1
SQLANYWHERE_EXTENSION_INIT1

#include <stdio.h>

extern void bottomless_tracing_init();
extern void bottomless_init();
extern struct sqlanywhere_wal_methods* bottomless_methods(struct sqlanywhere_wal_methods*);

int sqlite3_bottomless_init(
  sqlite3 *db, 
  char **pzErrMsg, 
  const sqlite3_api_routines *pApi,
  const sqlanywhere_api_routines *pSqlanywhereApi
) {
  // yes, racy
  static int initialized = 0;
  if (initialized == 0) {
    initialized = 1;
  } else {
    return 0;
  }

  SQLITE_EXTENSION_INIT2(pApi);
  SQLANYWHERE_EXTENSION_INIT2(pSqlanywhereApi);

  bottomless_tracing_init();
  bottomless_init();
  struct sqlanywhere_wal_methods *orig = sqlanywhere_wal_methods_find(0);
  if (!orig) {
    return SQLITE_ERROR;
  }
  struct sqlanywhere_wal_methods *methods = bottomless_methods(orig);

  if (methods) {
    int rc = sqlanywhere_wal_methods_register(methods);
    return rc == SQLITE_OK ? SQLITE_OK_LOAD_PERMANENTLY : rc;
  }
  // It's not fatal to fail to instantiate methods - it will be logged.
  return SQLITE_OK_LOAD_PERMANENTLY;
}

int sqlanywhereBottomlessInit(sqlite3 *db) {
  return sqlite3_bottomless_init(db, NULL, NULL, NULL);
}

#endif

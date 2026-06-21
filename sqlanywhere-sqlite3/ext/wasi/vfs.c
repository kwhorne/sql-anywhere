#include "sqlite3.h"
#include "src/wal.h"
#include <string.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>

#define SQLANYWHERE_IMPORT(name) extern __attribute__((import_module("sqlanywhere_host"), import_name(name)))

SQLANYWHERE_IMPORT("close") int sqlanywhere_wasi_close(sqlite3_file*);
SQLANYWHERE_IMPORT("read") int sqlanywhere_wasi_read(sqlite3_file*, void*, int iAmt, sqlite3_int64 iOfst);
SQLANYWHERE_IMPORT("write") int sqlanywhere_wasi_write(sqlite3_file*, const void*, int iAmt, sqlite3_int64 iOfst);
SQLANYWHERE_IMPORT("truncate") int sqlanywhere_wasi_truncate(sqlite3_file*, sqlite3_int64 size);
SQLANYWHERE_IMPORT("sync") int sqlanywhere_wasi_sync(sqlite3_file*, int flags);
SQLANYWHERE_IMPORT("file_size") int sqlanywhere_wasi_file_size(sqlite3_file*, sqlite3_int64 *pSize);

typedef struct sqlanywhere_wasi_file {
    const struct sqlite3_io_methods* pMethods;
    int64_t fd;
} sqlanywhere_wasi_file;

// We're running in exclusive mode, so locks are noops.
// We need to handle locking in the host.
static int sqlanywhere_wasi_lock(sqlite3_file* f, int eLock) {
    (void)f, (void)eLock;
    return SQLITE_OK;
}

static int sqlanywhere_wasi_unlock(sqlite3_file* f, int eLock) {
    (void)f, (void)eLock;
    return SQLITE_OK;
}

static int sqlanywhere_wasi_check_reserved_lock(sqlite3_file* f, int *pResOut) {
    (void)f, (void)pResOut;
    return SQLITE_OK;
}

static int sqlanywhere_wasi_device_characteristics(sqlite3_file* f) {
    (void)f;
    return SQLITE_IOCAP_ATOMIC | SQLITE_IOCAP_SAFE_APPEND | SQLITE_IOCAP_SEQUENTIAL;
}

static int sqlanywhere_wasi_file_control(sqlite3_file* f, int opcode, void* arg) {
    (void)opcode, (void)f, (void)arg;
    return SQLITE_NOTFOUND;
}

static int sqlanywhere_wasi_sector_size(sqlite3_file* f) {
    (void)f;
    return 512;
}

static const sqlite3_io_methods wasi_io_methods = {
    .iVersion = 1,
    .xClose = &sqlanywhere_wasi_close,
    .xRead = &sqlanywhere_wasi_read,
    .xWrite = &sqlanywhere_wasi_write,
    .xTruncate = &sqlanywhere_wasi_truncate,
    .xSync = &sqlanywhere_wasi_sync,
    .xFileSize = &sqlanywhere_wasi_file_size,
    .xLock = &sqlanywhere_wasi_lock,
    .xUnlock = &sqlanywhere_wasi_unlock,
    .xCheckReservedLock = &sqlanywhere_wasi_check_reserved_lock,
    .xFileControl = &sqlanywhere_wasi_file_control,
    .xSectorSize = &sqlanywhere_wasi_sector_size,
    .xDeviceCharacteristics = &sqlanywhere_wasi_device_characteristics,
};

SQLANYWHERE_IMPORT("open_fd") int64_t sqlanywhere_wasi_open_fd(const char *zName, int flags);
SQLANYWHERE_IMPORT("delete") int sqlanywhere_wasi_delete(sqlite3_vfs*, const char *zName, int syncDir);
SQLANYWHERE_IMPORT("access") int sqlanywhere_wasi_access(sqlite3_vfs*, const char *zName, int flags, int *pResOut);
SQLANYWHERE_IMPORT("full_pathname") int sqlanywhere_wasi_full_pathname(sqlite3_vfs*, const char *zName, int nOut, char *zOut);
SQLANYWHERE_IMPORT("randomness") int sqlanywhere_wasi_randomness(sqlite3_vfs*, int nByte, char *zOut);
SQLANYWHERE_IMPORT("sleep") int sqlanywhere_wasi_sleep(sqlite3_vfs*, int microseconds);
SQLANYWHERE_IMPORT("current_time") int sqlanywhere_wasi_current_time(sqlite3_vfs*, double*);
SQLANYWHERE_IMPORT("get_last_error") int sqlanywhere_wasi_get_last_error(sqlite3_vfs*, int, char*);
SQLANYWHERE_IMPORT("current_time_64") int sqlanywhere_wasi_current_time_64(sqlite3_vfs*, sqlite3_int64*);

int sqlanywhere_wasi_vfs_open(sqlite3_vfs *vfs, const char *zName, sqlite3_file *file_, int flags, int *pOutFlags) {
    sqlanywhere_wasi_file *file = (sqlanywhere_wasi_file*)file_;
    file->fd = sqlanywhere_wasi_open_fd(zName, flags);
    if (file->fd == 0) {
        return SQLITE_CANTOPEN;
    }
    file->pMethods = &wasi_io_methods;
    return SQLITE_OK;
}

sqlite3_vfs sqlanywhere_wasi_vfs = {
    .iVersion = 2,
    .szOsFile = sizeof(sqlanywhere_wasi_file),
    .mxPathname = 100,
    .zName = "sqlanywhere_wasi",

    .xOpen = &sqlanywhere_wasi_vfs_open,
    .xDelete = &sqlanywhere_wasi_delete,
    .xAccess = &sqlanywhere_wasi_access,
    .xFullPathname = &sqlanywhere_wasi_full_pathname,
    .xRandomness = &sqlanywhere_wasi_randomness,
    .xSleep = &sqlanywhere_wasi_sleep,
    .xCurrentTime = &sqlanywhere_wasi_current_time,
    .xGetLastError = &sqlanywhere_wasi_get_last_error,
    .xCurrentTimeInt64 = &sqlanywhere_wasi_current_time_64,
};

sqlanywhere_wal_methods *the_wal_methods = NULL;

int sqlanywhere_wasi_wal_open(sqlite3_vfs* vfs, sqlite3_file* f, const char* path, int no_shm_mode, long long max_size, struct sqlanywhere_wal_methods* wal_methods, sqlanywhere_wal** wal) {
    fprintf(stderr, "Opening virtual WAL at %s: %s\n", path, wal_methods->zName);
    return the_wal_methods->xOpen(vfs, f, path, no_shm_mode, max_size, wal_methods, wal);
}

int sqlanywhere_wasi_wal_close(sqlanywhere_wal* wal, sqlite3* db, int sync_flags, int nBuf, unsigned char* zBuf) {
    return the_wal_methods->xClose(wal, db, sync_flags, nBuf, zBuf);
}

void sqlanywhere_wasi_wal_limit(sqlanywhere_wal* wal, long long limit) {
    return the_wal_methods->xLimit(wal, limit);
}

int sqlanywhere_wasi_wal_begin_read_transaction(sqlanywhere_wal* wal, int* out) {
    return the_wal_methods->xBeginReadTransaction(wal, out);
}

void sqlanywhere_wasi_wal_end_read_transaction(sqlanywhere_wal* wal) {
    return the_wal_methods->xEndReadTransaction(wal);
}

int sqlanywhere_wasi_wal_find_frame(sqlanywhere_wal* wal, unsigned int frame, unsigned int* out) {
    return the_wal_methods->xFindFrame(wal, frame, out);
}

int sqlanywhere_wasi_wal_read_frame(sqlanywhere_wal* wal, unsigned int frame, int n, unsigned char* out) {
    return the_wal_methods->xReadFrame(wal, frame, n, out);
}

unsigned int sqlanywhere_wasi_wal_dbsize(sqlanywhere_wal* wal) {
    return the_wal_methods->xDbsize(wal);
}

int sqlanywhere_wasi_wal_begin_write_transaction(sqlanywhere_wal* wal) {
    return the_wal_methods->xBeginWriteTransaction(wal);
}

int sqlanywhere_wasi_wal_end_write_transaction(sqlanywhere_wal* wal) {
    return the_wal_methods->xEndWriteTransaction(wal);
}

int sqlanywhere_wasi_wal_undo(sqlanywhere_wal* wal, int (*xUndo)(void*, unsigned int), void* pUndoCtx) {
    return the_wal_methods->xUndo(wal, xUndo, pUndoCtx);
}

void sqlanywhere_wasi_wal_savepoint(sqlanywhere_wal* wal, unsigned int* aWalData) {
    return the_wal_methods->xSavepoint(wal, aWalData);
}

int sqlanywhere_wasi_wal_savepoint_undo(sqlanywhere_wal* wal, unsigned int* aWalData) {
    return the_wal_methods->xSavepointUndo(wal, aWalData);
}

int sqlanywhere_wasi_wal_frames(sqlanywhere_wal* wal, int n, sqlanywhere_pghdr* aPgHdr, unsigned int cksum, int mode, int readonly) {
    return the_wal_methods->xFrames(wal, n, aPgHdr, cksum, mode, readonly, NULL);
}

int sqlanywhere_wasi_wal_checkpoint(sqlanywhere_wal* wal, sqlite3* db, int eMode, int (*xBusy)(void*), void* pBusyArg, int sync_flags, int nBuf, unsigned char* zBuf, int* pnLog, int* pnCkpt) {
    return the_wal_methods->xCheckpoint(wal, db, eMode, xBusy, pBusyArg, sync_flags, nBuf, zBuf, pnLog, pnCkpt);
}

int sqlanywhere_wasi_wal_callback(sqlanywhere_wal* wal) {
    return the_wal_methods->xCallback(wal);
}

int sqlanywhere_wasi_wal_exclusive_mode(sqlanywhere_wal* wal, int op) {
    return the_wal_methods->xExclusiveMode(wal, op);
}

int sqlanywhere_wasi_wal_heap_memory(sqlanywhere_wal* wal) {
    return the_wal_methods->xHeapMemory(wal);
}

int sqlanywhere_wasi_wal_snapshot_get(sqlanywhere_wal* wal, sqlite3_snapshot** snapshot) {
    return the_wal_methods->xSnapshotGet(wal, snapshot);
}

void sqlanywhere_wasi_wal_snapshot_open(sqlanywhere_wal* wal, sqlite3_snapshot* snapshot) {
    return the_wal_methods->xSnapshotOpen(wal, snapshot);
}

int sqlanywhere_wasi_wal_snapshot_recover(sqlanywhere_wal* wal) {
    return the_wal_methods->xSnapshotRecover(wal);
}

int sqlanywhere_wasi_wal_snapshot_check(sqlanywhere_wal* wal, sqlite3_snapshot* snapshot) {
    return the_wal_methods->xSnapshotCheck(wal, snapshot);
}

void sqlanywhere_wasi_wal_snapshot_unlock(sqlanywhere_wal* wal) {
    return the_wal_methods->xSnapshotUnlock(wal);
}

int sqlanywhere_wasi_wal_framesize(sqlanywhere_wal* wal) {
    return the_wal_methods->xFramesize(wal);
}

sqlite3_file *sqlanywhere_wasi_wal_file(sqlanywhere_wal* wal) {
    return the_wal_methods->xFile(wal);
}

int sqlanywhere_wasi_wal_writelock(sqlanywhere_wal* wal, int bLock) {
    return the_wal_methods->xWriteLock(wal, bLock);
}

void sqlanywhere_wasi_wal_db(sqlanywhere_wal* wal, sqlite3* db) {
    return the_wal_methods->xDb(wal, db);
}

int sqlanywhere_wasi_wal_pathname_len(int orig_len) {
    return the_wal_methods->xPathnameLen(orig_len);
}

void sqlanywhere_wasi_get_wal_pathname(char *buf, const char *orig, int len) {
    return the_wal_methods->xGetWalPathname(buf, orig, len);
}

int sqlanywhere_wasi_wal_pre_main_db_open(sqlanywhere_wal_methods *methods, const char *path) {
    return 0;
}

sqlanywhere_wal_methods sqlanywhere_wasi_wal_methods = {
    .iVersion = 1,
    .xOpen = &sqlanywhere_wasi_wal_open,
    .xClose = &sqlanywhere_wasi_wal_close,
    .xLimit = &sqlanywhere_wasi_wal_limit,
    .xBeginReadTransaction = &sqlanywhere_wasi_wal_begin_read_transaction,
    .xEndReadTransaction = &sqlanywhere_wasi_wal_end_read_transaction,
    .xFindFrame = &sqlanywhere_wasi_wal_find_frame,
    .xReadFrame = &sqlanywhere_wasi_wal_read_frame,
    .xDbsize = &sqlanywhere_wasi_wal_dbsize,
    .xBeginWriteTransaction = &sqlanywhere_wasi_wal_begin_write_transaction,
    .xEndWriteTransaction = &sqlanywhere_wasi_wal_end_write_transaction,
    .xUndo = &sqlanywhere_wasi_wal_undo,
    .xSavepoint = &sqlanywhere_wasi_wal_savepoint,
    .xSavepointUndo = &sqlanywhere_wasi_wal_savepoint_undo,
    .xFrames = &sqlanywhere_wasi_wal_frames,
    .xCheckpoint = &sqlanywhere_wasi_wal_checkpoint,
    .xCallback = &sqlanywhere_wasi_wal_callback,
    .xExclusiveMode = &sqlanywhere_wasi_wal_exclusive_mode,
    .xHeapMemory = &sqlanywhere_wasi_wal_heap_memory,
    .xSnapshotGet = &sqlanywhere_wasi_wal_snapshot_get,
    .xSnapshotOpen = &sqlanywhere_wasi_wal_snapshot_open,
    .xSnapshotRecover = &sqlanywhere_wasi_wal_snapshot_recover,
    .xSnapshotCheck = &sqlanywhere_wasi_wal_snapshot_check,
    .xSnapshotUnlock = &sqlanywhere_wasi_wal_snapshot_unlock,
    .xFramesize = &sqlanywhere_wasi_wal_framesize,
    .xFile = &sqlanywhere_wasi_wal_file,
    .xWriteLock = &sqlanywhere_wasi_wal_writelock,
    .xDb = &sqlanywhere_wasi_wal_db,
    .xPathnameLen = &sqlanywhere_wasi_wal_pathname_len,
    .xGetWalPathname = &sqlanywhere_wasi_get_wal_pathname,
    .xPreMainDbOpen = &sqlanywhere_wasi_wal_pre_main_db_open,
    .bUsesShm = 0,
    .zName = "sqlanywhere_wasi",
    .pNext = NULL,
};

void sqlanywhere_wasi_init() {
    the_wal_methods = sqlanywhere_wal_methods_find(NULL);
    sqlite3_vfs_register(&sqlanywhere_wasi_vfs, 1);
    sqlanywhere_wal_methods_register(&sqlanywhere_wasi_wal_methods);
    fprintf(stderr, "WASI initialized\n");
}

sqlite3 *sqlanywhere_wasi_open_db(const char *filename) {
    sqlite3 *db;
    fprintf(stderr, "opening database %s\n", filename);
    int rc = sqlanywhere_open(filename, &db, SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE, "sqlanywhere_wasi", "sqlanywhere_wasi");
    if (rc != SQLITE_OK) {
        fprintf(stderr, "Failed to open database: %s\n", sqlite3_errmsg(db));
        return NULL;
    }
    fprintf(stderr, "opened database %s\n", filename);
    rc = sqlite3_exec(db, "PRAGMA journal_mode=WAL;", NULL, NULL, NULL);
    if (rc != SQLITE_OK) {
        fprintf(stderr, "Failed to set journal mode: %s\n", sqlite3_errmsg(db));
        return NULL;
    }
    return db;
}

int sqlanywhere_wasi_exec(sqlite3 *db, const char *sql) {
    sqlite3_stmt *stmt;
    int rc = sqlite3_prepare_v2(db, sql, -1, &stmt, NULL);
    if (rc != SQLITE_OK) {
        fprintf(stderr, "Failed to prepare statement: %s\n", sqlite3_errmsg(db));
        return rc;
    }
    // Step in a loop until SQLITE_DONE or error
    while ((rc = sqlite3_step(stmt)) == SQLITE_ROW) {}
    if (rc != SQLITE_DONE) {
        fprintf(stderr, "Failed to execute statement: %s\n", sqlite3_errmsg(db));
        return rc;
    }
    return SQLITE_OK;
}
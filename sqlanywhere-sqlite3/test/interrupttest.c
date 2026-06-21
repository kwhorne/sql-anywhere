/*
**
** A multi-threaded test for sqlanywhere_stmt_interrupt().
**
** Unlike sqlite3_interrupt(), which aborts every statement running on a
** connection, sqlanywhere_stmt_interrupt() targets a single prepared statement.
** The interesting (and historically broken) case is interrupting a statement
** that is *already executing* inside sqlite3_step() on another thread: the
** per-statement flag must be observed from within the VDBE execution loop,
** not just at the entry to sqlite3_step().
**
** This is a standalone program (it has its own main()); it is not part of the
** Tcl testfixture.  The deterministic, single-threaded regression test lives
** in test/interruptstmt.test; this binary provides the genuine cross-thread
** coverage.  Build and run it from the sqlanywhere-sqlite3 directory with:
**
**   make interrupttest && ./interrupttest
**
** (the Makefile target compiles this file against the freshly built
** amalgamation).  Exit status is 0 if every check passes, non-zero otherwise.
*/
#include "sqlite3.h"

#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

/* Number of rows in the work table.  A 2-way self join over this many rows is
** a single, long-running sqlite3_step() that loops OP_Next (and therefore
** passes through the VDBE's check_for_interrupt label) many times.  Large
** enough that an un-interrupted run takes several seconds, so a prompt return
** unambiguously means the interrupt landed mid-step. */
#define WORK_ROWS 40000

/* How long the test waits before triggering the interrupt, and the upper
** bound on how long the step is allowed to keep running afterwards. */
#define DELAY_MS    250
#define REACT_MS   2000

static int nFail = 0;

#define CHECK(cond, msg) do{                                            \
    if( (cond) ){                                                       \
      printf("  ok    - %s\n", (msg));                                  \
    }else{                                                              \
      printf("  FAIL  - %s\n", (msg));                                  \
      nFail++;                                                          \
    }                                                                   \
  }while(0)

static double now_ms(void){
  struct timespec ts;
  clock_gettime(CLOCK_MONOTONIC, &ts);
  return ts.tv_sec*1000.0 + ts.tv_nsec/1.0e6;
}

static void must_ok(sqlite3 *db, int rc, const char *what){
  if( rc!=SQLITE_OK && rc!=SQLITE_DONE && rc!=SQLITE_ROW ){
    fprintf(stderr, "fatal: %s: %s\n", what, sqlite3_errmsg(db));
    exit(2);
  }
}

static void exec(sqlite3 *db, const char *sql){
  char *zErr = 0;
  int rc = sqlite3_exec(db, sql, 0, 0, &zErr);
  if( rc!=SQLITE_OK ){
    fprintf(stderr, "fatal: exec(%s): %s\n", sql, zErr ? zErr : "?");
    exit(2);
  }
}

/* Worker thread: drive a prepared statement to completion (or until it is
** interrupted), recording the final return code and how long it ran. */
typedef struct Worker {
  sqlite3_stmt *stmt;
  int rc;
  double elapsed_ms;
} Worker;

static void *worker_step(void *arg){
  Worker *w = (Worker*)arg;
  double t0 = now_ms();
  int rc;
  do{
    rc = sqlite3_step(w->stmt);
  }while( rc==SQLITE_ROW );
  w->rc = rc;
  w->elapsed_ms = now_ms() - t0;
  return 0;
}

/* Run `stmt` on a background thread, wait DELAY_MS, then interrupt it via the
** supplied interrupt routine.  Returns the worker result by value. */
static Worker run_and_interrupt(sqlite3_stmt *stmt,
                                void (*interrupt)(void*), void *arg){
  Worker w;
  pthread_t th;
  memset(&w, 0, sizeof(w));
  w.stmt = stmt;
  pthread_create(&th, 0, worker_step, &w);
  usleep(DELAY_MS*1000);     /* let the worker get well inside sqlite3_step() */
  interrupt(arg);
  pthread_join(th, 0);
  return w;
}

static void stmt_interrupt_cb(void *arg){
  sqlanywhere_stmt_interrupt((sqlite3_stmt*)arg);
}

/*
** Test 1: interrupt a statement that is already executing in another thread.
** The step must return SQLITE_INTERRUPT, and it must do so promptly rather
** than running the full join to completion.
*/
static void test_inflight(sqlite3 *db){
  sqlite3_stmt *stmt;
  Worker w;
  printf("test_inflight: interrupt a step() already in flight\n");
  must_ok(db, sqlite3_prepare_v2(db,
      "SELECT count(*) FROM t a, t b", -1, &stmt, 0), "prepare join");

  w = run_and_interrupt(stmt, stmt_interrupt_cb, stmt);

  printf("  ... rc=%d (%s), ran %.0f ms\n",
         w.rc, sqlite3_errstr(w.rc), w.elapsed_ms);
  CHECK(w.rc==SQLITE_INTERRUPT, "step returned SQLITE_INTERRUPT");
  CHECK(w.elapsed_ms < DELAY_MS + REACT_MS,
        "step aborted promptly (mid-execution, not at completion)");
  sqlite3_finalize(stmt);
}

/*
** Test 2: statement-level granularity.  Interrupting statement A must not set
** the connection-wide interrupt state, so an unrelated statement B on the same
** connection continues to run normally.
*/
static void test_granularity(sqlite3 *db){
  sqlite3_stmt *a, *b;
  Worker w;
  int rc;
  printf("test_granularity: interrupting A does not disturb the connection\n");
  must_ok(db, sqlite3_prepare_v2(db,
      "SELECT count(*) FROM t a, t b", -1, &a, 0), "prepare A");
  must_ok(db, sqlite3_prepare_v2(db, "SELECT 1", -1, &b, 0), "prepare B");

  w = run_and_interrupt(a, stmt_interrupt_cb, a);
  CHECK(w.rc==SQLITE_INTERRUPT, "A returned SQLITE_INTERRUPT");
  CHECK(sqlite3_is_interrupted(db)==0,
        "connection interrupt flag is NOT set (per-statement only)");

  rc = sqlite3_step(b);
  CHECK(rc==SQLITE_ROW, "unrelated statement B still runs to a row");
  sqlite3_finalize(a);
  sqlite3_finalize(b);
}

int main(int argc, char **argv){
  sqlite3 *db;
  int rc;
  (void)argc; (void)argv;

  rc = sqlite3_open_v2(":memory:", &db,
         SQLITE_OPEN_READWRITE|SQLITE_OPEN_CREATE|SQLITE_OPEN_FULLMUTEX, 0);
  if( rc!=SQLITE_OK ){
    fprintf(stderr, "fatal: open: %s\n", sqlite3_errmsg(db));
    return 2;
  }

  exec(db, "CREATE TABLE t(x);");
  {
    char zSql[256];
    snprintf(zSql, sizeof(zSql),
        "WITH RECURSIVE c(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM c WHERE i<%d)"
        " INSERT INTO t SELECT i FROM c;", WORK_ROWS);
    exec(db, zSql);
  }

  test_inflight(db);
  test_granularity(db);

  sqlite3_close(db);

  printf("\n%s\n", nFail==0 ? "PASS" : "FAIL");
  return nFail==0 ? 0 : 1;
}

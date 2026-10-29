/*
** 2024-04-25
**
** Copyright 2024 the SqlAnywhere authors
**
** Permission is hereby granted, free of charge, to any person obtaining a copy of
** this software and associated documentation files (the "Software"), to deal in
** the Software without restriction, including without limitation the rights to
** use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies of
** the Software, and to permit persons to whom the Software is furnished to do so,
** subject to the following conditions:
**
** The above copyright notice and this permission notice shall be included in all
** copies or substantial portions of the Software.
**
** THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
** IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS
** FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR
** COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER
** IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN
** CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.
**
******************************************************************************
**
** SqlAnywhere vector search.
*/
#if !defined(SQLITE_OMIT_VECTOR) && !defined(SQLITE_OMIT_VIRTUALTABLE)
#include "sqlite3.h"
#include "vdbeInt.h"
#include "vectorIndexInt.h"

typedef struct vectorVtab vectorVtab;
struct vectorVtab {
  sqlite3_vtab base;       /* Base class - must be first */
  sqlite3 *db;             /* Database connection */
};

typedef struct vectorVtab_cursor vectorVtab_cursor;
struct vectorVtab_cursor {
  // first fields must copy fields from the sqlite3_vtab_cursor_tracked struct
  sqlite3_vtab_cursor base;  /* Base class - must be first */
  int nReads;                /* Number of row read from the storage backing virtual table */
  int nWrites;               /* Number of row written to the storage backing virtual table */
  VectorOutRows rows;
  int iRow;
};

/* Column numbers */
#define VECTOR_COLUMN_IDX    0
#define VECTOR_COLUMN_VECTOR 1
#define VECTOR_COLUMN_K      2
#define VECTOR_COLUMN_OFFSET 3

static int vectorVtabConnect(
  sqlite3 *db,
  void *pAux,
  int argc, const char *const *argv,
  sqlite3_vtab **ppVtab,
  char **pzErr
){
  vectorVtab *pVtab = NULL;
  int rc;
  /*
   * name of the database ignored by SQLite - so we don't need to provide any schema prefix here
   * hidden column are parameters of table-valued function (see https://www.sqlite.org/vtab.html#table_valued_functions)
  */
  rc = sqlite3_declare_vtab(db, "CREATE TABLE x(idx hidden, vector hidden, k hidden, id);");
  if( rc != SQLITE_OK ){
    return rc;
  }
  pVtab = sqlite3_malloc( sizeof(vectorVtab) );
  if( pVtab == NULL ){
    return SQLITE_NOMEM_BKPT;
  }
  // > Eponymous virtual tables exist in the "main" schema only, so they will not work if prefixed with a different schema name. 
  // so, argv[1] always equal to "main" and we can safely ignore it
  // (see https://www.sqlite.org/vtab.html#epovtab)
  memset(pVtab, 0, sizeof(*pVtab));
  pVtab->db = db;
  *ppVtab = (sqlite3_vtab*)pVtab;
  return SQLITE_OK;
}

static int vectorVtabDisconnect(sqlite3_vtab *pVtab){
  vectorVtab *pVTab = (vectorVtab*)pVtab;
  sqlite3_free(pVtab);
  return SQLITE_OK;
}

static int vectorVtabOpen(sqlite3_vtab *p, sqlite3_vtab_cursor **ppCursor){
  vectorVtab *pVTab = (vectorVtab*)p;
  vectorVtab_cursor *pCur;
  pCur = sqlite3_malloc( sizeof(vectorVtab_cursor) );
  if( pCur == NULL ){
    return SQLITE_NOMEM;
  }
  memset(pCur, 0, sizeof(*pCur));
  *ppCursor = &pCur->base;
  return SQLITE_OK;
}

static int vectorVtabClose(sqlite3_vtab_cursor *cur){
  vectorVtab_cursor *pCur = (vectorVtab_cursor*)cur;
  vectorVtab *pVTab = (vectorVtab *)cur->pVtab;
  vectorOutRowsFree(pVTab->db, &pCur->rows);
  sqlite3_free(pCur);
  return SQLITE_OK;
}

static int vectorVtabNext(sqlite3_vtab_cursor *cur){
  vectorVtab_cursor *pCur = (vectorVtab_cursor*)cur;
  pCur->iRow++;
  return SQLITE_OK;
}

static int vectorVtabEof(sqlite3_vtab_cursor *cur){
  vectorVtab_cursor *pCur = (vectorVtab_cursor*)cur;
  return pCur->iRow >= pCur->rows.nRows;
}

static int vectorVtabColumn(
  sqlite3_vtab_cursor *cur,   /* The cursor */
  sqlite3_context *context,   /* First argument to sqlite3_result_...() */
  int iCol                    /* Which column to return */
){
  vectorVtab_cursor *pCur = (vectorVtab_cursor*)cur;
  vectorOutRowsGet(context, &pCur->rows, pCur->iRow, iCol - VECTOR_COLUMN_OFFSET);
  return SQLITE_OK;
}

static int vectorVtabRowid(sqlite3_vtab_cursor *cur, sqlite_int64 *pRowid){
  vectorVtab_cursor *pCur = (vectorVtab_cursor*)cur;
  // rowid used for internal SQLite needs - so we take it if output has single integer column but in other case just return current row index
  if( pCur->rows.aIntValues != NULL ){
    *pRowid = pCur->rows.aIntValues[pCur->iRow];
  }else{
    *pRowid = pCur->iRow;
  }
  return SQLITE_OK;
}

static int vectorVtabFilter(
  sqlite3_vtab_cursor *pVtabCursor,
  int idxNum, const char *idxStr,
  int argc, sqlite3_value **argv
){
  vectorVtab_cursor *pCur = (vectorVtab_cursor *)pVtabCursor;
  vectorVtab *pVTab = (vectorVtab *)pVtabCursor->pVtab;
  pCur->rows.aIntValues = NULL;
  pCur->rows.ppValues = NULL;

  if( vectorIndexSearch(pVTab->db, argc, argv, &pCur->rows, &pCur->nReads, &pCur->nWrites, &pVTab->base.zErrMsg) != 0 ){
    return SQLITE_ERROR;
  }

  assert( pCur->rows.nRows >= 0 );
  assert( pCur->rows.nCols > 0 );
  pCur->iRow = 0;
  return SQLITE_OK;
}

static int vectorVtabBestIndex(
  sqlite3_vtab *tab,
  sqlite3_index_info *pIdxInfo
){
  const struct sqlite3_index_constraint *pConstraint;
  int aUsable[3] = { -1, -1, -1 };  /* constraint for index, vector, k */
  int unusable = 0;                  /* bitmask of arguments known only later */
  int i, iArg, nArg = 0;

  pIdxInfo->estimatedCost = (double)1;
  pIdxInfo->estimatedRows = 100;
  pIdxInfo->idxNum = 1;

  pConstraint = pIdxInfo->aConstraint;
  for(i=0; i<pIdxInfo->nConstraint; i++, pConstraint++){
    if( pConstraint->op != SQLITE_INDEX_CONSTRAINT_EQ ) continue;
    switch( pConstraint->iColumn ){
      case VECTOR_COLUMN_IDX:    iArg = 0; break;
      case VECTOR_COLUMN_VECTOR: iArg = 1; break;
      case VECTOR_COLUMN_K:      iArg = 2; break;
      default: continue;
    }
    if( pConstraint->usable ){
      aUsable[iArg] = i;
    }else{
      unusable |= 1 << iArg;
    }
  }
  /*
  ** An argument that refers to a table later in the join, as in
  **   SELECT ... FROM queries AS q, vector_top_k('idx', q.v, 10)
  ** is not usable in this plan. Refuse the plan rather than search without
  ** it, so the planner puts that table first and passes its column in.
  */
  for(iArg=0; iArg<3; iArg++){
    if( aUsable[iArg] < 0 && (unusable & (1 << iArg)) ){
      return SQLITE_CONSTRAINT;
    }
  }
  for(iArg=0; iArg<3; iArg++){
    if( aUsable[iArg] >= 0 ){
      pIdxInfo->aConstraintUsage[aUsable[iArg]].argvIndex = ++nArg;
      pIdxInfo->aConstraintUsage[aUsable[iArg]].omit = 1;
    }
  }
  return SQLITE_OK;
}

static sqlite3_module vectorModule = {
  /* iVersion    */ 0,
  /* xCreate     */ 0,
  /* xConnect    */ vectorVtabConnect,
  /* xBestIndex  */ vectorVtabBestIndex,
  /* xDisconnect */ vectorVtabDisconnect,
  /* xDestroy    */ 0,
  /* xOpen       */ vectorVtabOpen,
  /* xClose      */ vectorVtabClose,
  /* xFilter     */ vectorVtabFilter,
  /* xNext       */ vectorVtabNext,
  /* xEof        */ vectorVtabEof,
  /* xColumn     */ vectorVtabColumn,
  /* xRowid      */ vectorVtabRowid,
  /* xUpdate     */ 0,
  /* xBegin      */ 0,
  /* xSync       */ 0,
  /* xCommit     */ 0,
  /* xRollback   */ 0,
  /* xFindMethod */ 0,
  /* xRename     */ 0,
  /* xSavepoint  */ 0,
  /* xRelease    */ 0,
  /* xRollbackTo */ 0,
  /* xShadowName */ 0,
  /* xIntegrity  */ 0
};

/*
** hybrid_top_k(vector_index, fts_table, vector, query, k [, rrf_k])
**
** Fuses a vector ranking and an FTS5 keyword ranking with Reciprocal Rank
** Fusion: each ranker contributes its top k rows, a row scores
** 1/(rrf_k + rank) for every ranking it appears in, and the k best scores are
** returned, best first. A NULL vector or a NULL query leaves its ranker out.
** Both rankings must be keyed by the same integer rowid.
*/
typedef struct HybridHit HybridHit;
struct HybridHit {
  i64 id;
  int vectorRank;            /* 1-based rank in the vector ranking, 0 if absent */
  int ftsRank;               /* 1-based rank in the keyword ranking, 0 if absent */
  double score;
};

typedef struct hybridVtab_cursor hybridVtab_cursor;
struct hybridVtab_cursor {
  // first fields must copy fields from the sqlite3_vtab_cursor_tracked struct
  sqlite3_vtab_cursor base;  /* Base class - must be first */
  int nReads;                /* Number of row read from the storage backing virtual table */
  int nWrites;               /* Number of row written to the storage backing virtual table */
  HybridHit *aHits;
  int nHits;
  int nAlloc;
  int iRow;
};

/* Column numbers */
#define HYBRID_COLUMN_IDX         0
#define HYBRID_COLUMN_FTS         1
#define HYBRID_COLUMN_VECTOR      2
#define HYBRID_COLUMN_QUERY       3
#define HYBRID_COLUMN_K           4
#define HYBRID_COLUMN_RRF_K       5
#define HYBRID_COLUMN_ID          6
#define HYBRID_COLUMN_SCORE       7
#define HYBRID_COLUMN_VECTOR_RANK 8
#define HYBRID_COLUMN_FTS_RANK    9

#define HYBRID_N_ARGS        6
#define HYBRID_N_REQUIRED    5
#define HYBRID_RRF_K_DEFAULT 60

static int hybridVtabConnect(
  sqlite3 *db,
  void *pAux,
  int argc, const char *const *argv,
  sqlite3_vtab **ppVtab,
  char **pzErr
){
  vectorVtab *pVtab = NULL;
  int rc;
  rc = sqlite3_declare_vtab(db,
    "CREATE TABLE x(vector_index hidden, fts_table hidden, vector hidden, query hidden, k hidden, rrf_k hidden, "
    "id, score, vector_rank, fts_rank);"
  );
  if( rc != SQLITE_OK ){
    return rc;
  }
  pVtab = sqlite3_malloc( sizeof(vectorVtab) );
  if( pVtab == NULL ){
    return SQLITE_NOMEM_BKPT;
  }
  memset(pVtab, 0, sizeof(*pVtab));
  pVtab->db = db;
  *ppVtab = (sqlite3_vtab*)pVtab;
  return SQLITE_OK;
}

static int hybridVtabOpen(sqlite3_vtab *p, sqlite3_vtab_cursor **ppCursor){
  hybridVtab_cursor *pCur;
  pCur = sqlite3_malloc( sizeof(hybridVtab_cursor) );
  if( pCur == NULL ){
    return SQLITE_NOMEM;
  }
  memset(pCur, 0, sizeof(*pCur));
  *ppCursor = &pCur->base;
  return SQLITE_OK;
}

static int hybridVtabClose(sqlite3_vtab_cursor *cur){
  hybridVtab_cursor *pCur = (hybridVtab_cursor*)cur;
  sqlite3_free(pCur->aHits);
  sqlite3_free(pCur);
  return SQLITE_OK;
}

static int hybridVtabNext(sqlite3_vtab_cursor *cur){
  hybridVtab_cursor *pCur = (hybridVtab_cursor*)cur;
  pCur->iRow++;
  return SQLITE_OK;
}

static int hybridVtabEof(sqlite3_vtab_cursor *cur){
  hybridVtab_cursor *pCur = (hybridVtab_cursor*)cur;
  return pCur->iRow >= pCur->nHits;
}

static int hybridVtabColumn(
  sqlite3_vtab_cursor *cur,   /* The cursor */
  sqlite3_context *context,   /* First argument to sqlite3_result_...() */
  int iCol                    /* Which column to return */
){
  hybridVtab_cursor *pCur = (hybridVtab_cursor*)cur;
  const HybridHit *pHit = &pCur->aHits[pCur->iRow];
  switch( iCol ){
    case HYBRID_COLUMN_ID:
      sqlite3_result_int64(context, pHit->id);
      break;
    case HYBRID_COLUMN_SCORE:
      sqlite3_result_double(context, pHit->score);
      break;
    case HYBRID_COLUMN_VECTOR_RANK:
      if( pHit->vectorRank > 0 ) sqlite3_result_int(context, pHit->vectorRank);
      break;
    case HYBRID_COLUMN_FTS_RANK:
      if( pHit->ftsRank > 0 ) sqlite3_result_int(context, pHit->ftsRank);
      break;
    default:
      /* arguments are not echoed back */
      break;
  }
  return SQLITE_OK;
}

static int hybridVtabRowid(sqlite3_vtab_cursor *cur, sqlite_int64 *pRowid){
  hybridVtab_cursor *pCur = (hybridVtab_cursor*)cur;
  *pRowid = pCur->aHits[pCur->iRow].id;
  return SQLITE_OK;
}

static int hybridAppend(hybridVtab_cursor *pCur, i64 id, int vectorRank, int ftsRank){
  HybridHit *pHit;
  if( pCur->nHits == pCur->nAlloc ){
    int nNew = pCur->nAlloc ? pCur->nAlloc * 2 : 16;
    HybridHit *aNew = sqlite3_realloc64(pCur->aHits, (u64)nNew * sizeof(HybridHit));
    if( aNew == NULL ){
      return SQLITE_NOMEM_BKPT;
    }
    pCur->aHits = aNew;
    pCur->nAlloc = nNew;
  }
  pHit = &pCur->aHits[pCur->nHits++];
  pHit->id = id;
  pHit->vectorRank = vectorRank;
  pHit->ftsRank = ftsRank;
  pHit->score = 0.0;
  return SQLITE_OK;
}

static int hybridCompareId(const void *a, const void *b){
  const HybridHit *pA = (const HybridHit*)a;
  const HybridHit *pB = (const HybridHit*)b;
  return pA->id < pB->id ? -1 : pA->id > pB->id;
}

/* Best score first; equal scores fall back to the smaller id, so the order is stable. */
static int hybridCompareScore(const void *a, const void *b){
  const HybridHit *pA = (const HybridHit*)a;
  const HybridHit *pB = (const HybridHit*)b;
  if( pA->score != pB->score ){
    return pA->score > pB->score ? -1 : 1;
  }
  return hybridCompareId(a, b);
}

/* Appends the vector ranking: the top k rows of the vector index. */
static int hybridVectorRanking(
  hybridVtab_cursor *pCur,
  sqlite3 *db,
  sqlite3_value *pIdx,
  sqlite3_value *pVector,
  int k,
  char **pzErrMsg
){
  VectorOutRows rows;
  sqlite3_value *apSearch[3];
  sqlite3_value *pK;
  int rc, i;

  memset(&rows, 0, sizeof(rows));
  pK = sqlite3ValueNew(db);
  if( pK == NULL ){
    return SQLITE_NOMEM_BKPT;
  }
  sqlite3VdbeMemSetInt64(pK, k);
  apSearch[0] = pIdx;
  apSearch[1] = pVector;
  apSearch[2] = pK;
  rc = vectorIndexSearch(db, 3, apSearch, &rows, &pCur->nReads, &pCur->nWrites, pzErrMsg);
  sqlite3ValueFree(pK);
  if( rc != SQLITE_OK ){
    vectorOutRowsFree(db, &rows);
    return rc;
  }
  if( rows.nRows > 0 && rows.aIntValues == NULL ){
    *pzErrMsg = sqlite3_mprintf("hybrid_top_k: the vector index must be on a table with an integer rowid");
    vectorOutRowsFree(db, &rows);
    return SQLITE_ERROR;
  }
  for(i=0; i<rows.nRows && rc==SQLITE_OK; i++){
    rc = hybridAppend(pCur, rows.aIntValues[i], i + 1, 0);
  }
  vectorOutRowsFree(db, &rows);
  return rc;
}

/* Appends the keyword ranking: the top k rows of the FTS5 table by bm25 rank. */
static int hybridFtsRanking(
  hybridVtab_cursor *pCur,
  sqlite3 *db,
  const char *zFts,
  sqlite3_value *pQuery,
  int k,
  char **pzErrMsg
){
  const char *pDot = strchr(zFts, '.');
  sqlite3_stmt *pStmt = NULL;
  char *zSql;
  int rc, iRank = 0;

  if( pDot != NULL ){
    char *zSchema = sqlite3_mprintf("%.*s", (int)(pDot - zFts), zFts);
    if( zSchema == NULL ){
      return SQLITE_NOMEM_BKPT;
    }
    zSql = sqlite3_mprintf(
      "SELECT rowid FROM \"%w\".\"%w\" WHERE \"%w\" MATCH ?1 ORDER BY rank LIMIT ?2",
      zSchema, pDot + 1, pDot + 1
    );
    sqlite3_free(zSchema);
  }else{
    zSql = sqlite3_mprintf(
      "SELECT rowid FROM \"%w\" WHERE \"%w\" MATCH ?1 ORDER BY rank LIMIT ?2",
      zFts, zFts
    );
  }
  if( zSql == NULL ){
    return SQLITE_NOMEM_BKPT;
  }
  rc = sqlite3_prepare_v2(db, zSql, -1, &pStmt, NULL);
  sqlite3_free(zSql);
  if( rc != SQLITE_OK ){
    *pzErrMsg = sqlite3_mprintf("hybrid_top_k: fts_table '%s' must be an FTS5 table: %s", zFts, sqlite3_errmsg(db));
    return rc;
  }
  sqlite3_bind_value(pStmt, 1, pQuery);
  sqlite3_bind_int(pStmt, 2, k);
  while( (rc = sqlite3_step(pStmt)) == SQLITE_ROW ){
    rc = hybridAppend(pCur, sqlite3_column_int64(pStmt, 0), 0, ++iRank);
    if( rc != SQLITE_OK ){
      break;
    }
  }
  if( rc == SQLITE_DONE ){
    rc = SQLITE_OK;
  }else if( rc != SQLITE_NOMEM ){
    *pzErrMsg = sqlite3_mprintf("hybrid_top_k: %s", sqlite3_errmsg(db));
  }
  pCur->nReads += sqlite3_stmt_status(pStmt, SQLANYWHERE_STMTSTATUS_ROWS_READ, 0);
  sqlite3_finalize(pStmt);
  return rc;
}

/* Reads a non-negative integer argument, as vector_top_k reads k. */
static int hybridIntArg(sqlite3_value *pArg, const char *zName, int *pOut, char **pzErrMsg){
  double d;
  switch( sqlite3_value_type(pArg) ){
    case SQLITE_INTEGER:
      if( sqlite3_value_int64(pArg) >= 0 && sqlite3_value_int64(pArg) <= SQLITE_MAX_LENGTH ){
        *pOut = sqlite3_value_int(pArg);
        return SQLITE_OK;
      }
      break;
    case SQLITE_FLOAT:
      d = sqlite3_value_double(pArg);
      if( d >= 0 && d <= SQLITE_MAX_LENGTH && (double)(int)d == d ){
        *pOut = (int)d;
        return SQLITE_OK;
      }
      break;
  }
  *pzErrMsg = sqlite3_mprintf("hybrid_top_k: %s must be a non-negative integer", zName);
  return SQLITE_ERROR;
}

static int hybridVtabFilter(
  sqlite3_vtab_cursor *pVtabCursor,
  int idxNum, const char *idxStr,
  int argc, sqlite3_value **argv
){
  hybridVtab_cursor *pCur = (hybridVtab_cursor *)pVtabCursor;
  vectorVtab *pVTab = (vectorVtab *)pVtabCursor->pVtab;
  sqlite3 *db = pVTab->db;
  char **pzErrMsg = &pVTab->base.zErrMsg;
  sqlite3_value *apArg[HYBRID_N_ARGS];
  double rrfK = HYBRID_RRF_K_DEFAULT;
  int i, j, k, rc = SQLITE_OK;

  pCur->nHits = 0;
  pCur->iRow = 0;

  /* idxNum has a bit for each argument given, in argument order */
  for(i=0, j=0; i<HYBRID_N_ARGS; i++){
    apArg[i] = (idxNum & (1 << i)) && j < argc ? argv[j++] : NULL;
  }
  for(i=0; i<HYBRID_N_REQUIRED; i++){
    if( apArg[i] == NULL ){
      *pzErrMsg = sqlite3_mprintf("hybrid_top_k: expected arguments (vector_index, fts_table, vector, query, k [, rrf_k])");
      return SQLITE_ERROR;
    }
  }
  if( sqlite3_value_type(apArg[HYBRID_COLUMN_IDX]) != SQLITE_TEXT ){
    *pzErrMsg = sqlite3_mprintf("hybrid_top_k: vector_index must be the name of a vector index");
    return SQLITE_ERROR;
  }
  if( sqlite3_value_type(apArg[HYBRID_COLUMN_FTS]) != SQLITE_TEXT ){
    *pzErrMsg = sqlite3_mprintf("hybrid_top_k: fts_table must be the name of an FTS5 table");
    return SQLITE_ERROR;
  }
  if( hybridIntArg(apArg[HYBRID_COLUMN_K], "k", &k, pzErrMsg) != SQLITE_OK ){
    return SQLITE_ERROR;
  }
  if( apArg[HYBRID_COLUMN_RRF_K] != NULL ){
    int eType = sqlite3_value_type(apArg[HYBRID_COLUMN_RRF_K]);
    rrfK = sqlite3_value_double(apArg[HYBRID_COLUMN_RRF_K]);
    if( (eType != SQLITE_INTEGER && eType != SQLITE_FLOAT) || !(rrfK >= 0) ){
      *pzErrMsg = sqlite3_mprintf("hybrid_top_k: rrf_k must be a non-negative number");
      return SQLITE_ERROR;
    }
  }
  if( k == 0 ){
    return SQLITE_OK;
  }

  if( sqlite3_value_type(apArg[HYBRID_COLUMN_VECTOR]) != SQLITE_NULL ){
    rc = hybridVectorRanking(pCur, db, apArg[HYBRID_COLUMN_IDX], apArg[HYBRID_COLUMN_VECTOR], k, pzErrMsg);
  }
  if( rc == SQLITE_OK && sqlite3_value_type(apArg[HYBRID_COLUMN_QUERY]) != SQLITE_NULL ){
    rc = hybridFtsRanking(
      pCur, db, (const char*)sqlite3_value_text(apArg[HYBRID_COLUMN_FTS]), apArg[HYBRID_COLUMN_QUERY], k, pzErrMsg
    );
  }
  if( rc != SQLITE_OK ){
    pCur->nHits = 0;
    return rc;
  }

  /* a row in both rankings appears twice: sort by id and fold each pair into one */
  qsort(pCur->aHits, pCur->nHits, sizeof(HybridHit), hybridCompareId);
  for(i=0, j=0; i<pCur->nHits; i++){
    if( j > 0 && pCur->aHits[j-1].id == pCur->aHits[i].id ){
      if( pCur->aHits[i].vectorRank ) pCur->aHits[j-1].vectorRank = pCur->aHits[i].vectorRank;
      if( pCur->aHits[i].ftsRank ) pCur->aHits[j-1].ftsRank = pCur->aHits[i].ftsRank;
    }else{
      pCur->aHits[j++] = pCur->aHits[i];
    }
  }
  pCur->nHits = j;
  for(i=0; i<pCur->nHits; i++){
    HybridHit *pHit = &pCur->aHits[i];
    pHit->score = 0.0;
    if( pHit->vectorRank ) pHit->score += 1.0 / (rrfK + pHit->vectorRank);
    if( pHit->ftsRank ) pHit->score += 1.0 / (rrfK + pHit->ftsRank);
  }
  qsort(pCur->aHits, pCur->nHits, sizeof(HybridHit), hybridCompareScore);
  if( pCur->nHits > k ){
    pCur->nHits = k;
  }
  return SQLITE_OK;
}

static int hybridVtabBestIndex(
  sqlite3_vtab *tab,
  sqlite3_index_info *pIdxInfo
){
  const struct sqlite3_index_constraint *pConstraint;
  int aUsable[HYBRID_N_ARGS];
  int unusable = 0;                  /* bitmask of arguments known only later */
  int i, iArg, nArg = 0;

  for(iArg=0; iArg<HYBRID_N_ARGS; iArg++){
    aUsable[iArg] = -1;
  }
  pIdxInfo->estimatedCost = (double)1;
  pIdxInfo->estimatedRows = 100;
  pIdxInfo->idxNum = 0;

  pConstraint = pIdxInfo->aConstraint;
  for(i=0; i<pIdxInfo->nConstraint; i++, pConstraint++){
    if( pConstraint->op != SQLITE_INDEX_CONSTRAINT_EQ ) continue;
    if( pConstraint->iColumn < 0 || pConstraint->iColumn >= HYBRID_N_ARGS ) continue;
    iArg = pConstraint->iColumn;
    if( pConstraint->usable ){
      aUsable[iArg] = i;
    }else{
      unusable |= 1 << iArg;
    }
  }
  /* as in vector_top_k: refuse a plan that lacks an argument a later table provides */
  for(iArg=0; iArg<HYBRID_N_ARGS; iArg++){
    if( aUsable[iArg] < 0 && (unusable & (1 << iArg)) ){
      return SQLITE_CONSTRAINT;
    }
  }
  for(iArg=0; iArg<HYBRID_N_ARGS; iArg++){
    if( aUsable[iArg] >= 0 ){
      pIdxInfo->aConstraintUsage[aUsable[iArg]].argvIndex = ++nArg;
      pIdxInfo->aConstraintUsage[aUsable[iArg]].omit = 1;
      pIdxInfo->idxNum |= 1 << iArg;
    }
  }
  return SQLITE_OK;
}

static sqlite3_module hybridModule = {
  /* iVersion    */ 0,
  /* xCreate     */ 0,
  /* xConnect    */ hybridVtabConnect,
  /* xBestIndex  */ hybridVtabBestIndex,
  /* xDisconnect */ vectorVtabDisconnect,
  /* xDestroy    */ 0,
  /* xOpen       */ hybridVtabOpen,
  /* xClose      */ hybridVtabClose,
  /* xFilter     */ hybridVtabFilter,
  /* xNext       */ hybridVtabNext,
  /* xEof        */ hybridVtabEof,
  /* xColumn     */ hybridVtabColumn,
  /* xRowid      */ hybridVtabRowid,
  /* xUpdate     */ 0,
  /* xBegin      */ 0,
  /* xSync       */ 0,
  /* xCommit     */ 0,
  /* xRollback   */ 0,
  /* xFindMethod */ 0,
  /* xRename     */ 0,
  /* xSavepoint  */ 0,
  /* xRelease    */ 0,
  /* xRollbackTo */ 0,
  /* xShadowName */ 0,
  /* xIntegrity  */ 0
};

int vectorVtabInit(sqlite3 *db){
  int rc = sqlite3_create_module(db, VECTOR_INDEX_VTAB_NAME, &vectorModule, 0);
  if( rc == SQLITE_OK ){
    rc = sqlite3_create_module(db, VECTOR_HYBRID_VTAB_NAME, &hybridModule, 0);
  }
  return rc;
}
#endif /* !defined(SQLITE_OMIT_VECTOR) && !defined(SQLITE_OMIT_VIRTUALTABLE) */

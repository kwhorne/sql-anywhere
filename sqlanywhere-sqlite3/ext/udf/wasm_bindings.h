/* SPDX-License-Identifier: MIT */
#ifdef SQLANYWHERE_ENABLE_WASM_RUNTIME

#ifndef SQLANYWHERE_WASM_BINDINGS_H
#define SQLANYWHERE_WASM_BINDINGS_H

typedef struct sqlanywhere_wasm_engine_t sqlanywhere_wasm_engine_t;
typedef struct sqlanywhere_wasm_module_t sqlanywhere_wasm_module_t;

typedef struct sqlanywhere_wasm_udf_api {
    int (*sqlanywhere_value_type)(sqlite3_value*);
    int (*sqlanywhere_value_int)(sqlite3_value*);
    double (*sqlanywhere_value_double)(sqlite3_value*);
    const unsigned char *(*sqlanywhere_value_text)(sqlite3_value*);
    const void *(*sqlanywhere_value_blob)(sqlite3_value*);
    int (*sqlanywhere_value_bytes)(sqlite3_value*);
    void (*sqlanywhere_result_error)(sqlite3_context*, const char*, int);
    void (*sqlanywhere_result_error_nomem)(sqlite3_context*);
    void (*sqlanywhere_result_int)(sqlite3_context*, int);
    void (*sqlanywhere_result_double)(sqlite3_context*, double);
    void (*sqlanywhere_result_text)(sqlite3_context*, const char*, int, void(*)(void*));
    void (*sqlanywhere_result_blob)(sqlite3_context*, const void*, int, void(*)(void*));
    void (*sqlanywhere_result_null)(sqlite3_context*);
    void *(*sqlanywhere_malloc)(int);
    void (*sqlanywhere_free)(void *);
} sqlanywhere_wasm_udf_api;

/*
** Runs a WebAssembly user-defined function.
** Additional data can be accessed via sqlite3_user_data(context)
*/
void sqlanywhere_run_wasm(struct sqlanywhere_wasm_udf_api *api, sqlite3_context *context,
    sqlanywhere_wasm_engine_t *engine, sqlanywhere_wasm_module_t *module, const char *func_name, int argc, sqlite3_value **argv);

/*
** Compiles a WebAssembly module. Can accept both .wat and binary Wasm format, depending on the implementation.
** err_msg_buf needs to be deallocated with sqlanywhere_free_wasm_module.
*/
sqlanywhere_wasm_module_t *sqlanywhere_compile_wasm_module(sqlanywhere_wasm_engine_t* engine, const char *pSrcBody, int nBody,
    void *(*alloc_err_buf)(unsigned long long), char **err_msg_buf);

/*
** Frees a module allocated with sqlanywhere_compile_wasm_module
*/
void sqlanywhere_free_wasm_module(void *module);

/*
** Creates a new wasm engine
*/
sqlanywhere_wasm_engine_t *sqlanywhere_wasm_engine_new();
void sqlanywhere_wasm_engine_free(sqlanywhere_wasm_engine_t *);

#endif //SQLANYWHERE_WASM_BINDINGS_H
#endif //SQLANYWHERE_ENABLE_WASM_RUNTIME

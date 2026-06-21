use std::ffi::{c_char, c_void, CStr};
use wasmtime::{Config, Engine, Linker, Module, Store, Val};
use wasmtime_wasi::WasiCtxBuilder;

const SQLANYWHERE_INTEGER: i8 = 1;
const SQLANYWHERE_FLOAT: i8 = 2;
const SQLANYWHERE_TEXT: i8 = 3;
const SQLANYWHERE_BLOB: i8 = 4;
const SQLANYWHERE_NULL: i8 = 5;

fn maybe_set_err_buf(
    err_buf: *mut *const u8,
    err_str: String,
    alloc_err: unsafe extern "C" fn(u64) -> *mut u8,
) {
    if !err_buf.is_null() {
        let err_ptr = unsafe { alloc_err(err_str.len() as u64 + 1) };
        unsafe { std::slice::from_raw_parts_mut(err_ptr, err_str.len()) }
            .copy_from_slice(err_str.as_bytes());
        unsafe { *err_buf = err_ptr as *const u8 };
    }
}

#[no_mangle]
pub fn sqlanywhere_compile_wasm_module(
    engine: *const wasmtime::Engine,
    p_src_body: *const u8,
    n_body: i32,
    alloc_err: unsafe extern "C" fn(u64) -> *mut u8,
    err_msg_buf: *mut *const u8,
) -> *const c_void {
    let src_body: &[u8] = unsafe { std::slice::from_raw_parts(p_src_body, n_body as usize) };

    let module = match Module::new(unsafe { &*engine }, src_body) {
        Ok(m) => m,
        Err(orig_e) => {
            // If compilation failed, let's assume it's unquoted .wat and retry
            let src_body_str: &str = match std::str::from_utf8(src_body) {
                Ok(src) => src,
                Err(e) => {
                    maybe_set_err_buf(
                        err_msg_buf,
                        format!(
                            "Failed to compile module: {}, and it's not valid .wat either: {}",
                            orig_e, e
                        ),
                        alloc_err,
                    );
                    return std::ptr::null() as *const c_void;
                }
            };
            if src_body_str.len() < 2 {
                maybe_set_err_buf(
                    err_msg_buf,
                    format!("Failed to compile module: {}", orig_e),
                    alloc_err,
                );
                return std::ptr::null() as *const c_void;
            }
            let src_body_dequoted =
                String::from(&src_body_str[1..src_body_str.len() - 2]).replace("''", "'");
            match Module::new(unsafe { &*engine }, src_body_dequoted.as_bytes()) {
                Ok(m) => m,
                Err(e) => {
                    maybe_set_err_buf(
                        err_msg_buf,
                        format!("Failed to compile .wat module: {}", e),
                        alloc_err,
                    );
                    return std::ptr::null();
                }
            }
        }
    };
    let module = Box::new(module);
    let module_ptr = &*module as *const Module as *const c_void;
    std::mem::forget(module);
    module_ptr
}

#[no_mangle]
pub fn sqlanywhere_wasm_engine_new() -> *const c_void {
    let engine = match Engine::new(&Config::new()) {
        Ok(eng) => eng,
        Err(_) => return std::ptr::null() as *const c_void,
    };

    Box::into_raw(Box::new(engine)) as *const c_void
}

#[no_mangle]
pub fn sqlanywhere_wasm_engine_free(engine: *mut c_void) {
    unsafe {
        let _ = Box::from_raw(engine as *mut Engine);
    }
}

#[repr(C)]
#[allow(non_camel_case_types)]
pub struct sqlanywhere_wasm_udf_api {
    sqlanywhere_value_type: unsafe extern "C" fn(*const c_void) -> i32,
    sqlanywhere_value_int: unsafe extern "C" fn(*const c_void) -> i32,
    sqlanywhere_value_double: unsafe extern "C" fn(*const c_void) -> f64,
    sqlanywhere_value_text: unsafe extern "C" fn(*const c_void) -> *const u8,
    sqlanywhere_value_blob: unsafe extern "C" fn(*const c_void) -> *const c_void,
    sqlanywhere_value_bytes: unsafe extern "C" fn(*const c_void) -> i32,
    sqlanywhere_result_error: unsafe extern "C" fn(*const c_void, *const u8, i32),
    sqlanywhere_result_error_nomem: unsafe extern "C" fn(*const c_void),
    sqlanywhere_result_int: unsafe extern "C" fn(*const c_void, i32),
    sqlanywhere_result_double: unsafe extern "C" fn(*const c_void, f64),
    sqlanywhere_result_text: unsafe extern "C" fn(*const c_void, *const u8, i32, *const c_void),
    sqlanywhere_result_blob: unsafe extern "C" fn(*const c_void, *const c_void, i32, *const c_void),
    sqlanywhere_result_null: unsafe extern "C" fn(*const c_void),
    sqlanywhere_malloc: unsafe extern "C" fn(i32) -> *mut c_void,
    sqlanywhere_free: unsafe extern "C" fn(*mut c_void),
}

fn alloc_slice(api: *const sqlanywhere_wasm_udf_api, s: &[u8]) -> *const c_void {
    let len = s.len();
    let ptr = unsafe { ((*api).sqlanywhere_malloc)(len as i32) };
    unsafe { std::slice::from_raw_parts_mut(ptr as *mut u8, len) }.copy_from_slice(s);
    ptr as *const c_void
}

#[no_mangle]
pub fn sqlanywhere_run_wasm(
    api: *const sqlanywhere_wasm_udf_api,
    sqlanywhere_ctx: *const c_void,
    engine: *mut Engine,
    module: *mut Module,
    func_name: *const u8,
    argc: i32,
    argv: *mut *mut c_void,
) {
    let engine = unsafe { &*engine };
    let module = unsafe { &*module };

    let run_wasm = |engine: &Engine, module: &Module| -> Result<(), String> {
        let mut linker = Linker::new(engine);
        wasmtime_wasi::add_to_linker(&mut linker, |s| s)
            .map_err(|e| format!("Add WASI failed: {}", e))?;
        let wasi = WasiCtxBuilder::new()
            .inherit_stdio()
            .args(&[])
            .map_err(|e| format!("Creating WasiCtx failed: {}", e))?
            .build();
        let mut store = Store::new(engine, wasi);

        let instance = linker
            .instantiate(&mut store, module)
            .map_err(|e| format!("Creating instance failed: {}", e))?;

        let func_name: &str = unsafe { CStr::from_ptr(func_name as *const c_char) }
            .to_str()
            .map_err(|e| format!("Function name is not valid utf-8: {}", e))?;

        let func = instance
            .get_func(&mut store, func_name)
            .ok_or_else(|| format!("Function {} not found in Wasm module", func_name))?;

        let memory = instance
            .get_memory(&mut store, "memory")
            .ok_or_else(|| format!("Memory \"memory\" not found in wasm module"))?;

        let mem_size = memory.size(&mut store) as usize;

        let mut vals: Vec<Val> = Vec::new();
        for i in 0..argc {
            let arg = unsafe { *argv.offset(i as isize) };
            match unsafe { ((*api).sqlanywhere_value_type)(arg) } as i8 {
                SQLANYWHERE_INTEGER => {
                    vals.push(Val::I64(unsafe { ((*api).sqlanywhere_value_int)(arg) } as i64))
                }
                SQLANYWHERE_FLOAT => vals.push(Val::F64(
                    unsafe { ((*api).sqlanywhere_value_double)(arg) }.to_bits(),
                )),
                SQLANYWHERE_TEXT => {
                    let text_len = unsafe { ((*api).sqlanywhere_value_bytes)(arg) } as usize;
                    let text: &[u8] = unsafe {
                        std::slice::from_raw_parts(((*api).sqlanywhere_value_text)(arg), text_len)
                    };

                    let func_name = "sqlanywhere_malloc";
                    let func_malloc =
                        instance.get_func(&mut store, func_name).ok_or_else(|| {
                            format!("Function {} not found in Wasm module", func_name)
                        })?;
                    let params = [Val::I32((text_len + 2) as i32)];
                    let mut result = Val::null();
                    func_malloc
                        .call(&mut store, &params, std::slice::from_mut(&mut result))
                        .map_err(|e| format!("Calling function {} failed: {}", func_name, e))?;

                    let mem_offset = result.i32().unwrap_or(mem_size as i32) as usize;

                    let data = memory.data_mut(&mut store);
                    data[mem_offset] = SQLANYWHERE_TEXT as u8;
                    data[mem_offset + 1..mem_offset + 1 + text_len].copy_from_slice(text);
                    data[mem_offset + 1 + text_len] = 0;

                    vals.push(Val::I32(mem_offset as i32));
                }
                SQLANYWHERE_BLOB => {
                    let blob_len = unsafe { ((*api).sqlanywhere_value_bytes)(arg) } as usize;
                    let blob: &[u8] = unsafe {
                        std::slice::from_raw_parts(
                            ((*api).sqlanywhere_value_blob)(arg) as *const u8,
                            blob_len,
                        )
                    };
                    let blob_len_i32 = blob_len as i32;

                    let func_name = "sqlanywhere_malloc";
                    let func_malloc =
                        instance.get_func(&mut store, func_name).ok_or_else(|| {
                            format!("Function {} not found in Wasm module", func_name)
                        })?;
                    let params = [Val::I32(blob_len_i32 + 5)];
                    let mut result = Val::null();
                    func_malloc
                        .call(&mut store, &params, std::slice::from_mut(&mut result))
                        .map_err(|e| format!("Calling function {} failed: {}", func_name, e))?;

                    let mem_offset = result.i32().unwrap_or(mem_size as i32) as usize;

                    let data = memory.data_mut(&mut store);
                    data[mem_offset] = SQLANYWHERE_BLOB as u8;
                    data[mem_offset + 1..mem_offset + 1 + 4]
                        .copy_from_slice(&blob_len_i32.to_be_bytes());
                    data[mem_offset + 1 + 4..mem_offset + 1 + 4 + blob_len].copy_from_slice(blob);

                    vals.push(Val::I32(mem_offset as i32));
                }
                SQLANYWHERE_NULL => {
                    let func_name = "sqlanywhere_malloc";
                    let func_malloc =
                        instance.get_func(&mut store, func_name).ok_or_else(|| {
                            format!("Function {} not found in Wasm module", func_name)
                        })?;
                    let params = [Val::I32(1)];
                    let mut result = Val::null();
                    func_malloc
                        .call(&mut store, &params, std::slice::from_mut(&mut result))
                        .map_err(|e| format!("Calling function {} failed: {}", func_name, e))?;

                    let mem_offset = result.i32().unwrap_or(mem_size as i32) as usize;

                    memory.data_mut(&mut store)[mem_offset] = SQLANYWHERE_NULL as u8;

                    vals.push(Val::I32(mem_offset as i32));
                }
                _ => {
                    return Err(format!("Unknown SqlAnywhere type"));
                }
            }
        }

        let mut result = Val::null();
        func.call(&mut store, &vals, std::slice::from_mut(&mut result))
            .map_err(|e| format!("Calling function {} failed: {}", func_name, e))?;

        match result {
            Val::I64(v) => unsafe { ((*api).sqlanywhere_result_int)(sqlanywhere_ctx, v as i32) },
            Val::F64(v) => unsafe { ((*api).sqlanywhere_result_double)(sqlanywhere_ctx, f64::from_bits(v)) },
            Val::I32(v) => {
                let v = v as usize;
                match memory.data(&store)[v] as i8 {
                    SQLANYWHERE_TEXT => {
                        let result_str = unsafe {
                            CStr::from_ptr(
                                (memory.data(&store).as_ptr() as *const c_char)
                                    .offset(v as isize + 1),
                            )
                        };
                        let result_ptr = alloc_slice(api, result_str.to_bytes_with_nul());
                        unsafe {
                            ((*api).sqlanywhere_result_text)(
                                sqlanywhere_ctx,
                                result_ptr as *const u8,
                                result_str.to_str().unwrap().len() as i32, // safe to unwrap, created in alloc_slice
                                (*api).sqlanywhere_free as *const c_void,
                            )
                        }
                    }
                    SQLANYWHERE_BLOB => {
                        let blob_len = i32::from_be_bytes(
                            memory.data(&store)[v + 1..v + 1 + 4].try_into().unwrap(), // safe to unwrap, slice size == 4
                        );
                        let result_ptr = alloc_slice(
                            api,
                            &memory.data(&store)[v + 1 + 4..v + 1 + 4 + blob_len as usize],
                        );
                        unsafe {
                            ((*api).sqlanywhere_result_blob)(
                                sqlanywhere_ctx,
                                result_ptr as *const c_void,
                                blob_len,
                                (*api).sqlanywhere_free as *const c_void,
                            )
                        }
                    }
                    SQLANYWHERE_NULL => unsafe { ((*api).sqlanywhere_result_null)(sqlanywhere_ctx) },
                    _ => return Err(format!("Malformed result type byte")),
                }
            }
            _ => return Err(format!("Malformed result type")),
        }
        Ok(())
    };

    match run_wasm(engine, module) {
        Ok(_) => {}
        Err(err) => unsafe {
            ((*api).sqlanywhere_result_error)(sqlanywhere_ctx, err.as_ptr() as *const u8, err.len() as i32);
        },
    }
}

#[no_mangle]
pub fn sqlanywhere_free_wasm_module(module: *mut *mut Module) {
    unsafe {
        let _ = Box::from_raw(*module);
    }
}

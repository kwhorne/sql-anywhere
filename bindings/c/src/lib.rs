#![allow(clippy::missing_safety_doc)]
#![allow(non_camel_case_types)]
#[macro_use]
extern crate lazy_static;

mod types;

use crate::types::sqlanywhere_config;
use http::Uri;
use sqlanywhere::{errors, Builder, LoadExtensionGuard};
use tokio::runtime::Runtime;
use types::{
    blob, sqlanywhere_connection, sqlanywhere_connection_t, sqlanywhere_database, sqlanywhere_database_t, sqlanywhere_row,
    sqlanywhere_row_t, sqlanywhere_rows, sqlanywhere_rows_future_t, sqlanywhere_rows_t, sqlanywhere_stmt, sqlanywhere_stmt_t,
    replicated, stmt,
};

lazy_static! {
    static ref RT: Runtime = tokio::runtime::Runtime::new().unwrap();
}

fn translate_string(s: String) -> *const std::ffi::c_char {
    match std::ffi::CString::new(s) {
        Ok(s) => s.into_raw(),
        Err(_) => std::ptr::null(),
    }
}

unsafe fn set_err_msg(msg: String, output: *mut *const std::ffi::c_char) {
    if !output.is_null() {
        *output = translate_string(msg);
    }
}

#[no_mangle]
pub unsafe extern "C" fn sqlanywhere_enable_internal_tracing() -> std::ffi::c_int {
    if tracing_subscriber::fmt::try_init().is_ok() {
        1
    } else {
        0
    }
}

#[no_mangle]
pub unsafe extern "C" fn sqlanywhere_sync(
    db: sqlanywhere_database_t,
    out_err_msg: *mut *const std::ffi::c_char,
) -> std::ffi::c_int {
    let db = db.get_ref();
    match RT.block_on(db.sync()) {
        Ok(_) => 0,
        Err(e) => {
            set_err_msg(format!("Error syncing database: {e}"), out_err_msg);
            1
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn sqlanywhere_sync2(
    db: sqlanywhere_database_t,
    out_replicated: *mut replicated,
    out_err_msg: *mut *const std::ffi::c_char,
) -> std::ffi::c_int {
    let db = db.get_ref();
    match RT.block_on(db.sync()) {
        Ok(replicated) => {
            if !out_replicated.is_null() {
                (*out_replicated).frame_no = replicated.frame_no().unwrap_or(0) as i32;
                (*out_replicated).frames_synced = replicated.frames_synced() as i32;
            }

            0
        }
        Err(e) => {
            set_err_msg(format!("Error syncing database: {e}"), out_err_msg);
            1
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn sqlanywhere_open_sync(
    db_path: *const std::ffi::c_char,
    primary_url: *const std::ffi::c_char,
    auth_token: *const std::ffi::c_char,
    read_your_writes: std::ffi::c_char,
    encryption_key: *const std::ffi::c_char,
    out_db: *mut sqlanywhere_database_t,
    out_err_msg: *mut *const std::ffi::c_char,
) -> std::ffi::c_int {
    let config = sqlanywhere_config {
        db_path,
        primary_url,
        auth_token,
        read_your_writes,
        encryption_key,
        sync_interval: 0,
        with_webpki: 0,
        offline: 0,
        remote_encryption_key: std::ptr::null(),
    };
    sqlanywhere_open_sync_with_config(config, out_db, out_err_msg)
}

#[no_mangle]
pub unsafe extern "C" fn sqlanywhere_open_sync_with_webpki(
    db_path: *const std::ffi::c_char,
    primary_url: *const std::ffi::c_char,
    auth_token: *const std::ffi::c_char,
    read_your_writes: std::ffi::c_char,
    encryption_key: *const std::ffi::c_char,
    out_db: *mut sqlanywhere_database_t,
    out_err_msg: *mut *const std::ffi::c_char,
) -> std::ffi::c_int {
    let config = sqlanywhere_config {
        db_path,
        primary_url,
        auth_token,
        read_your_writes,
        encryption_key,
        sync_interval: 0,
        with_webpki: 1,
        offline: 0,
        remote_encryption_key: std::ptr::null(),
    };
    sqlanywhere_open_sync_with_config(config, out_db, out_err_msg)
}

/// Returns a new URI with the offline query parameter removed or None if the URI does not contain the offline query parameter.
fn maybe_remove_offline_query_param(url: &str) -> anyhow::Result<Option<String>> {
    let uri: Uri = url.try_into()?;
    let Some(query) = uri.query() else {
        return Ok(None);
    };
    let query = query.to_owned();
    let query_segments = query.split('&').collect::<Vec<&str>>();
    let segments_count = query_segments.len();
    let query_segments = query_segments
        .into_iter()
        .filter(|s| s != &"offline" && !s.starts_with("offline="))
        .collect::<Vec<&str>>();
    if segments_count == query_segments.len() {
        return Ok(None);
    }
    let query = query_segments.join("&");
    let Some(query_idx) = url.find('?') else {
        return Ok(None);
    };
    if query.is_empty() {
        return Ok(Some(url[..query_idx].to_owned()));
    }

    Ok(Some(url[..query_idx].to_owned() + "?" + &query))
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_remove_offline_query_param() {
        let uri = "http://example.com";
        let new_uri = maybe_remove_offline_query_param(uri).unwrap();
        assert_eq!(new_uri, None);

        let uri = "http://example.com?";
        let new_uri = maybe_remove_offline_query_param(uri).unwrap();
        assert_eq!(new_uri, None);

        let uri = "http://example.com?foo=bar";
        let new_uri = maybe_remove_offline_query_param(uri).unwrap();
        assert_eq!(new_uri, None);

        let uri = "http://example.com?offline";
        let new_uri = maybe_remove_offline_query_param(uri).unwrap();
        assert_eq!(new_uri.as_deref(), Some("http://example.com"));

        let uri = "http://example.com?offline=bar";
        let new_uri = maybe_remove_offline_query_param(uri).unwrap();
        assert_eq!(new_uri.as_deref(), Some("http://example.com"));

        let uri = "http://example.com?offline&foo=bar";
        let new_uri = maybe_remove_offline_query_param(uri).unwrap();
        assert_eq!(new_uri.as_deref(), Some("http://example.com?foo=bar"));

        let uri = "http://example.com?offline=true&foo=bar";
        let new_uri = maybe_remove_offline_query_param(uri).unwrap();
        assert_eq!(new_uri.as_deref(), Some("http://example.com?foo=bar"));

        let uri = "http://example.com?foo=bar&offline";
        let new_uri = maybe_remove_offline_query_param(uri).unwrap();
        assert_eq!(new_uri.as_deref(), Some("http://example.com?foo=bar"));

        let uri = "http://example.com?foo=bar&offline=true";
        let new_uri = maybe_remove_offline_query_param(uri).unwrap();
        assert_eq!(new_uri.as_deref(), Some("http://example.com?foo=bar"));

        let uri = "http://example.com?foo=bar&offline&foo2=bar2";
        let new_uri = maybe_remove_offline_query_param(uri).unwrap();
        assert_eq!(
            new_uri.as_deref(),
            Some("http://example.com?foo=bar&foo2=bar2")
        );

        let uri = "http://example.com?foo=bar&offline=true&foo2=bar2";
        let new_uri = maybe_remove_offline_query_param(uri).unwrap();
        assert_eq!(
            new_uri.as_deref(),
            Some("http://example.com?foo=bar&foo2=bar2")
        );

        let uri = "http://example.com?offline&foo=bar&offline";
        let new_uri = maybe_remove_offline_query_param(uri).unwrap();
        assert_eq!(new_uri.as_deref(), Some("http://example.com?foo=bar"));

        let uri = "http://example.com?offline&foo=bar&offline&foo2=bar2";
        let new_uri = maybe_remove_offline_query_param(uri).unwrap();
        assert_eq!(
            new_uri.as_deref(),
            Some("http://example.com?foo=bar&foo2=bar2")
        );
    }
}

#[no_mangle]
pub unsafe extern "C" fn sqlanywhere_open_sync_with_config(
    config: sqlanywhere_config,
    out_db: *mut sqlanywhere_database_t,
    out_err_msg: *mut *const std::ffi::c_char,
) -> std::ffi::c_int {
    let db_path = unsafe { std::ffi::CStr::from_ptr(config.db_path) };
    let db_path = match db_path.to_str() {
        Ok(url) => url,
        Err(e) => {
            set_err_msg(format!("Wrong URL: {e}"), out_err_msg);
            return 1;
        }
    };
    let primary_url = unsafe { std::ffi::CStr::from_ptr(config.primary_url) };
    let primary_url = match primary_url.to_str() {
        Ok(url) => url,
        Err(e) => {
            set_err_msg(format!("Wrong URL: {e}"), out_err_msg);
            return 2;
        }
    };
    let auth_token = unsafe { std::ffi::CStr::from_ptr(config.auth_token) };
    let auth_token = match auth_token.to_str() {
        Ok(token) => token,
        Err(e) => {
            set_err_msg(format!("Wrong Auth Token: {e}"), out_err_msg);
            return 3;
        }
    };
    let primary_url_with_offline_removed = match maybe_remove_offline_query_param(&primary_url) {
        Ok(url) => url,
        Err(e) => {
            set_err_msg(format!("Wrong primary URL: {e}"), out_err_msg);
            return 100;
        }
    };
    let offline = config.offline != 0 || primary_url_with_offline_removed.is_some();
    if offline {
        let primary_url = primary_url_with_offline_removed.unwrap_or(primary_url.to_owned());
        let mut builder =
            Builder::new_synced_database(db_path, primary_url.to_owned(), auth_token.to_owned());
        if config.with_webpki != 0 {
            let https = hyper_rustls::HttpsConnectorBuilder::new()
                .with_webpki_roots()
                .https_or_http()
                .enable_http1()
                .build();
            builder = builder.connector(https);
        }
        if !config.remote_encryption_key.is_null() {
            let key = unsafe { std::ffi::CStr::from_ptr(config.remote_encryption_key) };
            let key = match key.to_str() {
                Ok(k) => k,
                Err(e) => {
                    set_err_msg(format!("Wrong encryption key: {e}"), out_err_msg);
                    return 5;
                }
            };
            if !key.is_empty() {
                builder = builder.remote_encryption(sqlanywhere::EncryptionContext {
                    key: sqlanywhere::EncryptionKey::Base64Encoded(key.to_string()),
                });
            }
        };
        match RT.block_on(builder.build()) {
            Ok(db) => {
                let db = Box::leak(Box::new(sqlanywhere_database { db }));
                *out_db = sqlanywhere_database_t::from(db);
                return 0;
            }
            Err(e) => {
                set_err_msg(
                    format!(
                        "Error opening offline db path {db_path}, primary url {primary_url}: {e}"
                    ),
                    out_err_msg,
                );
                return 101;
            }
        }
    }
    let mut builder = sqlanywhere::Builder::new_remote_replica(
        db_path,
        primary_url.to_string(),
        auth_token.to_string(),
    );
    if config.with_webpki != 0 {
        let https = hyper_rustls::HttpsConnectorBuilder::new()
            .with_webpki_roots()
            .https_or_http()
            .enable_http1()
            .build();
        builder = builder.connector(https);
    }
    if config.sync_interval > 0 {
        let interval = match config.sync_interval.try_into() {
            Ok(d) => d,
            Err(e) => {
                set_err_msg(format!("Wrong periodic sync interval: {e}"), out_err_msg);
                return 4;
            }
        };
        builder = builder.sync_interval(std::time::Duration::from_secs(interval));
    }
    builder = builder.read_your_writes(config.read_your_writes != 0);
    if !config.encryption_key.is_null() {
        let key = unsafe { std::ffi::CStr::from_ptr(config.encryption_key) };
        let key = match key.to_str() {
            Ok(k) => k,
            Err(e) => {
                set_err_msg(format!("Wrong encryption key: {e}"), out_err_msg);
                return 5;
            }
        };
        let key = bytes::Bytes::copy_from_slice(key.as_bytes());
        let config = sqlanywhere::EncryptionConfig::new(sqlanywhere::Cipher::Aes256Cbc, key);
        builder = builder.encryption_config(config)
    };
    if !config.remote_encryption_key.is_null() {
        let key = unsafe { std::ffi::CStr::from_ptr(config.remote_encryption_key) };
        let key = match key.to_str() {
            Ok(k) => k,
            Err(e) => {
                set_err_msg(format!("Wrong encryption key: {e}"), out_err_msg);
                return 5;
            }
        };
        builder = builder.remote_encryption(sqlanywhere::EncryptionContext {
            key: sqlanywhere::EncryptionKey::Base64Encoded(key.to_string()),
        });
    };
    match RT.block_on(builder.build()) {
        Ok(db) => {
            let db = Box::leak(Box::new(sqlanywhere_database { db }));
            *out_db = sqlanywhere_database_t::from(db);
            0
        }
        Err(e) => {
            set_err_msg(
                format!("Error opening db path {db_path}, primary url {primary_url}: {e}"),
                out_err_msg,
            );
            6
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn sqlanywhere_open_ext(
    url: *const std::ffi::c_char,
    out_db: *mut sqlanywhere_database_t,
    out_err_msg: *mut *const std::ffi::c_char,
) -> std::ffi::c_int {
    sqlanywhere_open_file(url, out_db, out_err_msg)
}

#[no_mangle]
pub unsafe extern "C" fn sqlanywhere_open_file(
    url: *const std::ffi::c_char,
    out_db: *mut sqlanywhere_database_t,
    out_err_msg: *mut *const std::ffi::c_char,
) -> std::ffi::c_int {
    let url = unsafe { std::ffi::CStr::from_ptr(url) };
    let url = match url.to_str() {
        Ok(url) => url,
        Err(e) => {
            set_err_msg(format!("Wrong URL: {e}"), out_err_msg);
            return 1;
        }
    };
    match RT.block_on(sqlanywhere::Builder::new_local(url).build()) {
        Ok(db) => {
            let db = Box::leak(Box::new(sqlanywhere_database { db }));
            *out_db = sqlanywhere_database_t::from(db);
            0
        }
        Err(e) => {
            set_err_msg(format!("Error opening URL {url}: {e}"), out_err_msg);
            1
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn sqlanywhere_open_remote(
    url: *const std::ffi::c_char,
    auth_token: *const std::ffi::c_char,
    out_db: *mut sqlanywhere_database_t,
    out_err_msg: *mut *const std::ffi::c_char,
) -> std::ffi::c_int {
    sqlanywhere_open_remote_internal(
        url,
        auth_token,
        std::ptr::null(),
        false,
        out_db,
        out_err_msg,
    )
}

#[no_mangle]
pub unsafe extern "C" fn sqlanywhere_open_remote_with_remote_encryption(
    url: *const std::ffi::c_char,
    auth_token: *const std::ffi::c_char,
    remote_encryption_key: *const std::ffi::c_char,
    out_db: *mut sqlanywhere_database_t,
    out_err_msg: *mut *const std::ffi::c_char,
) -> std::ffi::c_int {
    sqlanywhere_open_remote_internal(
        url,
        auth_token,
        remote_encryption_key,
        false,
        out_db,
        out_err_msg,
    )
}

#[no_mangle]
pub unsafe extern "C" fn sqlanywhere_open_remote_with_webpki(
    url: *const std::ffi::c_char,
    auth_token: *const std::ffi::c_char,
    out_db: *mut sqlanywhere_database_t,
    out_err_msg: *mut *const std::ffi::c_char,
) -> std::ffi::c_int {
    sqlanywhere_open_remote_internal(url, auth_token, std::ptr::null(), true, out_db, out_err_msg)
}

unsafe fn sqlanywhere_open_remote_internal(
    url: *const std::ffi::c_char,
    auth_token: *const std::ffi::c_char,
    remote_encryption_key: *const std::ffi::c_char,
    with_webpki: bool,
    out_db: *mut sqlanywhere_database_t,
    out_err_msg: *mut *const std::ffi::c_char,
) -> std::ffi::c_int {
    let url = unsafe { std::ffi::CStr::from_ptr(url) };
    let url = match url.to_str() {
        Ok(url) => url,
        Err(e) => {
            set_err_msg(format!("Wrong URL: {e}"), out_err_msg);
            return 1;
        }
    };
    let auth_token = unsafe { std::ffi::CStr::from_ptr(auth_token) };
    let auth_token = match auth_token.to_str() {
        Ok(token) => token,
        Err(e) => {
            set_err_msg(format!("Wrong Auth Token: {e}"), out_err_msg);
            return 2;
        }
    };
    let mut builder = sqlanywhere::Builder::new_remote(url.to_string(), auth_token.to_string());

    if !remote_encryption_key.is_null() {
        let key = unsafe { std::ffi::CStr::from_ptr(remote_encryption_key) };
        let key = match key.to_str() {
            Ok(k) => k,
            Err(e) => {
                set_err_msg(format!("Wrong encryption key: {e}"), out_err_msg);
                return 5;
            }
        };
        builder = builder.remote_encryption(sqlanywhere::EncryptionContext {
            key: sqlanywhere::EncryptionKey::Base64Encoded(key.to_string()),
        });
    };

    if with_webpki {
        let https = hyper_rustls::HttpsConnectorBuilder::new()
            .with_webpki_roots()
            .https_or_http()
            .enable_http1()
            .build();
        builder = builder.connector(https);
    }
    match RT.block_on(builder.build()) {
        Ok(db) => {
            let db = Box::leak(Box::new(sqlanywhere_database { db }));
            *out_db = sqlanywhere_database_t::from(db);
            0
        }
        Err(e) => {
            set_err_msg(format!("Error opening URL {url}: {e}"), out_err_msg);
            1
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn sqlanywhere_close(db: sqlanywhere_database_t) {
    if db.is_null() {
        return;
    }
    let _db = unsafe { Box::from_raw(db.get_ref_mut()) };
    // TODO close db
}

#[no_mangle]
pub unsafe extern "C" fn sqlanywhere_connect(
    db: sqlanywhere_database_t,
    out_conn: *mut sqlanywhere_connection_t,
    out_err_msg: *mut *const std::ffi::c_char,
) -> std::ffi::c_int {
    let db = db.get_ref();
    let conn = match db.connect() {
        Ok(conn) => conn,
        Err(err) => {
            set_err_msg(format!("Unable to connect: {}", err), out_err_msg);
            return 1;
        }
    };
    let conn = Box::leak(Box::new(sqlanywhere_connection { conn }));
    *out_conn = sqlanywhere_connection_t::from(conn);
    0
}

#[no_mangle]
pub unsafe extern "C" fn sqlanywhere_load_extension(
    conn: sqlanywhere_connection_t,
    path: *const std::ffi::c_char,
    entry_point: *const std::ffi::c_char,
    out_err_msg: *mut *const std::ffi::c_char,
) -> std::ffi::c_int {
    if path.is_null() {
        set_err_msg("Null path".to_string(), out_err_msg);
        return 1;
    }
    let path = unsafe { std::ffi::CStr::from_ptr(path) };
    let path = match path.to_str() {
        Ok(path) => path,
        Err(e) => {
            set_err_msg(format!("Wrong path: {}", e), out_err_msg);
            return 2;
        }
    };
    let mut entry_point_option = None;
    if !entry_point.is_null() {
        let entry_point = unsafe { std::ffi::CStr::from_ptr(entry_point) };
        entry_point_option = match entry_point.to_str() {
            Ok(entry_point) => Some(entry_point),
            Err(e) => {
                set_err_msg(format!("Wrong entry point: {}", e), out_err_msg);
                return 4;
            }
        };
    }
    if conn.is_null() {
        set_err_msg("Null connection".to_string(), out_err_msg);
        return 5;
    }
    let conn = conn.get_ref();
    match RT.block_on(async move {
        let _guard = LoadExtensionGuard::new(conn)?;
        conn.load_extension(path, entry_point_option)?;
        Ok::<(), errors::Error>(())
    }) {
        Ok(()) => {}
        Err(e) => {
            set_err_msg(format!("Error loading extension: {}", e), out_err_msg);
            return 6;
        }
    };
    0
}

#[no_mangle]
pub unsafe extern "C" fn sqlanywhere_set_reserved_bytes(
    conn: sqlanywhere_connection_t,
    reserved_bytes: i32,
    out_err_msg: *mut *const std::ffi::c_char,
) -> std::ffi::c_int {
    if conn.is_null() {
        set_err_msg("Null connection".to_string(), out_err_msg);
        return 1;
    }
    let conn = conn.get_ref();
    if let Err(err) = conn.set_reserved_bytes(reserved_bytes) {
        set_err_msg(err.to_string(), out_err_msg);
        return 1;
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn sqlanywhere_get_reserved_bytes(
    conn: sqlanywhere_connection_t,
    reserved_bytes: *mut i32,
    out_err_msg: *mut *const std::ffi::c_char,
) -> std::ffi::c_int {
    if conn.is_null() {
        set_err_msg("Null connection".to_string(), out_err_msg);
        return 1;
    }
    let conn = conn.get_ref();
    match conn.get_reserved_bytes() {
        Ok(v) => *reserved_bytes = v,
        Err(err) => {
            set_err_msg(err.to_string(), out_err_msg);
            return 1;
        }
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn sqlanywhere_reset(
    conn: sqlanywhere_connection_t,
    out_err_msg: *mut *const std::ffi::c_char,
) -> std::ffi::c_int {
    if conn.is_null() {
        set_err_msg("Null connection".to_string(), out_err_msg);
        return 1;
    }
    let conn = conn.get_ref();
    RT.block_on(conn.reset());
    0
}

#[no_mangle]
pub unsafe extern "C" fn sqlanywhere_disconnect(conn: sqlanywhere_connection_t) {
    if conn.is_null() {
        return;
    }
    let conn = unsafe { Box::from_raw(conn.get_ref_mut()) };
    RT.spawn_blocking(|| {
        drop(conn);
    });
}

#[no_mangle]
pub unsafe extern "C" fn sqlanywhere_prepare(
    conn: sqlanywhere_connection_t,
    sql: *const std::ffi::c_char,
    out_stmt: *mut sqlanywhere_stmt_t,
    out_err_msg: *mut *const std::ffi::c_char,
) -> std::ffi::c_int {
    let sql = unsafe { std::ffi::CStr::from_ptr(sql) };
    let sql = match sql.to_str() {
        Ok(sql) => sql,
        Err(e) => {
            set_err_msg(format!("Wrong SQL: {}", e), out_err_msg);
            return 1;
        }
    };
    if conn.is_null() {
        set_err_msg("Null connection".to_string(), out_err_msg);
        return 2;
    }
    let conn = conn.get_ref();
    match RT.block_on(conn.prepare(sql)) {
        Ok(stmt) => {
            let stmt = Box::leak(Box::new(sqlanywhere_stmt {
                stmt: stmt {
                    stmt,
                    params: vec![],
                },
            }));
            *out_stmt = sqlanywhere_stmt_t::from(stmt);
        }
        Err(e) => {
            set_err_msg(format!("Error preparing statement: {}", e), out_err_msg);
            return 3;
        }
    };
    0
}

#[no_mangle]
pub unsafe extern "C" fn sqlanywhere_bind_int(
    stmt: sqlanywhere_stmt_t,
    idx: std::ffi::c_int,
    value: std::ffi::c_longlong,
    out_err_msg: *mut *const std::ffi::c_char,
) -> std::ffi::c_int {
    let idx: usize = match idx.try_into() {
        Ok(x) => x,
        Err(e) => {
            set_err_msg(format!("Wrong param index: {}", e), out_err_msg);
            return 1;
        }
    };
    let stmt = stmt.get_ref_mut();
    if stmt.params.len() < idx {
        stmt.params.resize(idx, sqlanywhere::Value::Null);
    }
    stmt.params[idx - 1] = value.into();
    0
}

#[no_mangle]
pub unsafe extern "C" fn sqlanywhere_bind_float(
    stmt: sqlanywhere_stmt_t,
    idx: std::ffi::c_int,
    value: std::ffi::c_double,
    out_err_msg: *mut *const std::ffi::c_char,
) -> std::ffi::c_int {
    let idx: usize = match idx.try_into() {
        Ok(x) => x,
        Err(e) => {
            set_err_msg(format!("Wrong param index: {}", e), out_err_msg);
            return 1;
        }
    };
    let stmt = stmt.get_ref_mut();
    if stmt.params.len() < idx {
        stmt.params.resize(idx, sqlanywhere::Value::Null);
    }
    stmt.params[idx - 1] = value.into();
    0
}

#[no_mangle]
pub unsafe extern "C" fn sqlanywhere_bind_null(
    stmt: sqlanywhere_stmt_t,
    idx: std::ffi::c_int,
    out_err_msg: *mut *const std::ffi::c_char,
) -> std::ffi::c_int {
    let idx: usize = match idx.try_into() {
        Ok(x) => x,
        Err(e) => {
            set_err_msg(format!("Wrong param index: {}", e), out_err_msg);
            return 1;
        }
    };
    let stmt = stmt.get_ref_mut();
    if stmt.params.len() < idx {
        stmt.params.resize(idx, sqlanywhere::Value::Null);
    }
    stmt.params[idx - 1] = sqlanywhere::Value::Null;
    0
}

#[no_mangle]
pub unsafe extern "C" fn sqlanywhere_bind_string(
    stmt: sqlanywhere_stmt_t,
    idx: std::ffi::c_int,
    value: *const std::ffi::c_char,
    out_err_msg: *mut *const std::ffi::c_char,
) -> std::ffi::c_int {
    let idx: usize = match idx.try_into() {
        Ok(x) => x,
        Err(e) => {
            set_err_msg(format!("Wrong param index: {}", e), out_err_msg);
            return 1;
        }
    };
    let value = unsafe { std::ffi::CStr::from_ptr(value) };
    let value = match value.to_str() {
        Ok(v) => v,
        Err(e) => {
            set_err_msg(format!("Wrong param value: {}", e), out_err_msg);
            return 2;
        }
    };
    let stmt = stmt.get_ref_mut();
    if stmt.params.len() < idx {
        stmt.params.resize(idx, sqlanywhere::Value::Null);
    }
    stmt.params[idx - 1] = value.to_string().into();
    0
}

#[no_mangle]
pub unsafe extern "C" fn sqlanywhere_bind_blob(
    stmt: sqlanywhere_stmt_t,
    idx: std::ffi::c_int,
    value: *const std::ffi::c_uchar,
    value_len: std::ffi::c_int,
    out_err_msg: *mut *const std::ffi::c_char,
) -> std::ffi::c_int {
    let idx: usize = match idx.try_into() {
        Ok(x) => x,
        Err(e) => {
            set_err_msg(format!("Wrong param index: {}", e), out_err_msg);
            return 1;
        }
    };
    let value_len: usize = match value_len.try_into() {
        Ok(v) => v,
        Err(e) => {
            set_err_msg(format!("Wrong param value len: {}", e), out_err_msg);
            return 2;
        }
    };
    let value = unsafe { core::slice::from_raw_parts(value, value_len) };
    let value = Vec::from(value);
    let stmt = stmt.get_ref_mut();
    if stmt.params.len() < idx {
        stmt.params.resize(idx, sqlanywhere::Value::Null);
    }
    stmt.params[idx - 1] = value.into();
    0
}

#[no_mangle]
pub unsafe extern "C" fn sqlanywhere_query_stmt(
    stmt: sqlanywhere_stmt_t,
    out_rows: *mut sqlanywhere_rows_t,
    out_err_msg: *mut *const std::ffi::c_char,
) -> std::ffi::c_int {
    if stmt.is_null() {
        set_err_msg("Null statement".to_string(), out_err_msg);
        return 1;
    }
    let stmt = stmt.get_ref_mut();
    match RT.block_on(stmt.stmt.query(stmt.params.clone())) {
        Ok(rows) => {
            let rows = Box::leak(Box::new(sqlanywhere_rows { result: rows }));
            *out_rows = sqlanywhere_rows_t::from(rows);
            0
        }
        Err(e) => {
            set_err_msg(format!("Error executing statement: {}", e), out_err_msg);
            1
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn sqlanywhere_execute_stmt(
    stmt: sqlanywhere_stmt_t,
    out_err_msg: *mut *const std::ffi::c_char,
) -> std::ffi::c_int {
    if stmt.is_null() {
        set_err_msg("Null statement".to_string(), out_err_msg);
        return 1;
    }
    let stmt = stmt.get_ref_mut();
    match RT.block_on(stmt.stmt.execute(stmt.params.clone())) {
        Ok(_) => 0,
        Err(e) => {
            set_err_msg(format!("Error executing statement: {}", e), out_err_msg);
            2
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn sqlanywhere_reset_stmt(
    stmt: sqlanywhere_stmt_t,
    out_err_msg: *mut *const std::ffi::c_char,
) -> std::ffi::c_int {
    if stmt.is_null() {
        set_err_msg("Null statement".to_string(), out_err_msg);
        return 1;
    }
    let stmt = stmt.get_ref_mut();
    stmt.params.clear();
    stmt.stmt.reset();
    0
}

#[no_mangle]
pub unsafe extern "C" fn sqlanywhere_free_stmt(stmt: sqlanywhere_stmt_t) {
    if stmt.is_null() {
        return;
    }
    let _ = unsafe { Box::from_raw(stmt.get_ref_mut()) };
}

#[no_mangle]
pub unsafe extern "C" fn sqlanywhere_query(
    conn: sqlanywhere_connection_t,
    sql: *const std::ffi::c_char,
    out_rows: *mut sqlanywhere_rows_t,
    out_err_msg: *mut *const std::ffi::c_char,
) -> std::ffi::c_int {
    let sql = unsafe { std::ffi::CStr::from_ptr(sql) };
    let sql = match sql.to_str() {
        Ok(sql) => sql,
        Err(e) => {
            set_err_msg(format!("Wrong SQL: {}", e), out_err_msg);
            return 1;
        }
    };
    let conn = conn.get_ref();
    match RT.block_on(conn.query(sql, ())) {
        Ok(rows) => {
            let rows = Box::leak(Box::new(sqlanywhere_rows { result: rows }));
            *out_rows = sqlanywhere_rows_t::from(rows);
            0
        }
        Err(e) => {
            set_err_msg(format!("Error executing statement: {}", e), out_err_msg);
            1
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn sqlanywhere_execute(
    conn: sqlanywhere_connection_t,
    sql: *const std::ffi::c_char,
    out_err_msg: *mut *const std::ffi::c_char,
) -> std::ffi::c_int {
    let sql = unsafe { std::ffi::CStr::from_ptr(sql) };
    let sql = match sql.to_str() {
        Ok(sql) => sql,
        Err(e) => {
            set_err_msg(format!("Wrong SQL: {}", e), out_err_msg);
            return 1;
        }
    };
    let conn = conn.get_ref();
    match RT.block_on(conn.execute(sql, ())) {
        Ok(_) => 0,
        Err(e) => {
            set_err_msg(format!("Error executing statement: {}", e), out_err_msg);
            2
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn sqlanywhere_free_rows(res: sqlanywhere_rows_t) {
    if res.is_null() {
        return;
    }
    let _ = unsafe { Box::from_raw(res.get_ref_mut()) };
}

#[no_mangle]
pub unsafe extern "C" fn sqlanywhere_free_rows_future(res: sqlanywhere_rows_future_t) {
    if res.is_null() {
        return;
    }
    let mut res = unsafe { Box::from_raw(res.get_ref_mut()) };
    res.wait().unwrap();
}

#[no_mangle]
pub unsafe extern "C" fn sqlanywhere_wait_result(res: sqlanywhere_rows_future_t) {
    let res = res.get_ref_mut();
    res.wait().unwrap();
}

#[no_mangle]
pub unsafe extern "C" fn sqlanywhere_column_count(res: sqlanywhere_rows_t) -> std::ffi::c_int {
    let res = res.get_ref();
    res.column_count()
}

#[no_mangle]
pub unsafe extern "C" fn sqlanywhere_column_name(
    res: sqlanywhere_rows_t,
    col: std::ffi::c_int,
    out_name: *mut *const std::ffi::c_char,
    out_err_msg: *mut *const std::ffi::c_char,
) -> std::ffi::c_int {
    let res = res.get_ref();
    if col >= res.column_count() {
        set_err_msg(
            format!(
                "Column index too big - got index {} with {} columns",
                col,
                res.column_count()
            ),
            out_err_msg,
        );
        return 1;
    }
    let name = res
        .column_name(col)
        .expect("Column should have valid index");
    match std::ffi::CString::new(name) {
        Ok(name) => {
            *out_name = name.into_raw();
            0
        }
        Err(e) => {
            set_err_msg(format!("Invalid name: {}", e), out_err_msg);
            1
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn sqlanywhere_column_type(
    res: sqlanywhere_rows_t,
    row: sqlanywhere_row_t,
    col: std::ffi::c_int,
    out_type: *mut std::ffi::c_int,
    out_err_msg: *mut *const std::ffi::c_char,
) -> std::ffi::c_int {
    let res = res.get_ref();
    if col >= res.column_count() {
        set_err_msg(
            format!(
                "Column index too big - got index {} with {} columns",
                col,
                res.column_count()
            ),
            out_err_msg,
        );
        return 1;
    }
    let row = row.get_ref();
    match row.get_value(col) {
        Ok(sqlanywhere::Value::Null) => {
            *out_type = types::SQLANYWHERE_NULL as i32;
        }
        Ok(sqlanywhere::Value::Text(_)) => {
            *out_type = types::SQLANYWHERE_TEXT as i32;
        }
        Ok(sqlanywhere::Value::Integer(_)) => {
            *out_type = types::SQLANYWHERE_INT as i32;
        }
        Ok(sqlanywhere::Value::Real(_)) => {
            *out_type = types::SQLANYWHERE_FLOAT as i32;
        }
        Ok(sqlanywhere::Value::Blob(_)) => {
            *out_type = types::SQLANYWHERE_BLOB as i32;
        }
        Err(e) => {
            set_err_msg(format!("Error fetching value: {e}"), out_err_msg);
            return 2;
        }
    };
    0
}

#[no_mangle]
pub unsafe extern "C" fn sqlanywhere_changes(conn: sqlanywhere_connection_t) -> u64 {
    let conn = conn.get_ref();
    conn.changes()
}

#[no_mangle]
pub unsafe extern "C" fn sqlanywhere_last_insert_rowid(conn: sqlanywhere_connection_t) -> i64 {
    let conn = conn.get_ref();
    conn.last_insert_rowid()
}

#[no_mangle]
pub unsafe extern "C" fn sqlanywhere_next_row(
    res: sqlanywhere_rows_t,
    out_row: *mut sqlanywhere_row_t,
    out_err_msg: *mut *const std::ffi::c_char,
) -> std::ffi::c_int {
    if res.is_null() {
        *out_row = sqlanywhere_row_t::null();
        return 0;
    }
    let rows = res.get_ref_mut();
    let res = RT.block_on(rows.next());
    match res {
        Ok(Some(row)) => {
            let row = Box::leak(Box::new(sqlanywhere_row { result: row }));
            *out_row = sqlanywhere_row_t::from(row);
            0
        }
        Ok(None) => {
            *out_row = sqlanywhere_row_t::null();
            0
        }
        Err(e) => {
            *out_row = sqlanywhere_row_t::null();
            set_err_msg(format!("Error fetching next row: {}", e), out_err_msg);
            1
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn sqlanywhere_free_row(res: sqlanywhere_row_t) {
    if res.is_null() {
        return;
    }
    let _ = unsafe { Box::from_raw(res.get_ref_mut()) };
}

#[no_mangle]
pub unsafe extern "C" fn sqlanywhere_get_string(
    res: sqlanywhere_row_t,
    col: std::ffi::c_int,
    out_value: *mut *const std::ffi::c_char,
    out_err_msg: *mut *const std::ffi::c_char,
) -> std::ffi::c_int {
    let res = res.get_ref();
    match res.get_value(col) {
        Ok(sqlanywhere::Value::Text(s)) => {
            *out_value = translate_string(s);
            0
        }
        Ok(_) => {
            set_err_msg("Value not a string".into(), out_err_msg);
            1
        }
        Err(e) => {
            set_err_msg(format!("Error fetching value: {e}"), out_err_msg);
            2
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn sqlanywhere_free_string(ptr: *const std::ffi::c_char) {
    if !ptr.is_null() {
        let _ = unsafe { std::ffi::CString::from_raw(ptr as *mut _) };
    }
}

#[no_mangle]
pub unsafe extern "C" fn sqlanywhere_get_int(
    res: sqlanywhere_row_t,
    col: std::ffi::c_int,
    out_value: *mut std::ffi::c_longlong,
    out_err_msg: *mut *const std::ffi::c_char,
) -> std::ffi::c_int {
    let res = res.get_ref();
    match res.get_value(col) {
        Ok(sqlanywhere::Value::Integer(i)) => {
            *out_value = i;
            0
        }
        Ok(_) => {
            set_err_msg("Value not an integer".into(), out_err_msg);
            1
        }
        Err(e) => {
            set_err_msg(format!("Error fetching value: {e}"), out_err_msg);
            2
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn sqlanywhere_get_float(
    res: sqlanywhere_row_t,
    col: std::ffi::c_int,
    out_value: *mut std::ffi::c_double,
    out_err_msg: *mut *const std::ffi::c_char,
) -> std::ffi::c_int {
    let res = res.get_ref();
    match res.get_value(col) {
        Ok(sqlanywhere::Value::Real(f)) => {
            *out_value = f;
            0
        }
        Ok(_) => {
            set_err_msg("Value not a float".into(), out_err_msg);
            1
        }
        Err(e) => {
            set_err_msg(format!("Error fetching value: {e}"), out_err_msg);
            2
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn sqlanywhere_get_blob(
    res: sqlanywhere_row_t,
    col: std::ffi::c_int,
    out_blob: *mut blob,
    out_err_msg: *mut *const std::ffi::c_char,
) -> std::ffi::c_int {
    let res = res.get_ref();
    match res.get_value(col) {
        Ok(sqlanywhere::Value::Blob(v)) => {
            let len: i32 = v.len().try_into().unwrap();
            let buf = v.into_boxed_slice();
            let data = buf.as_ptr();
            std::mem::forget(buf);
            *out_blob = blob {
                ptr: data as *const std::ffi::c_char,
                len,
            };
            0
        }
        Ok(_) => {
            set_err_msg("Value not a float".into(), out_err_msg);
            1
        }
        Err(e) => {
            set_err_msg(format!("Error fetching value: {}", e), out_err_msg);
            2
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn sqlanywhere_free_blob(b: blob) {
    if !b.ptr.is_null() {
        let ptr =
            unsafe { std::slice::from_raw_parts_mut(b.ptr as *mut i8, b.len.try_into().unwrap()) };
        let _ = unsafe { Box::from_raw(ptr) };
    }
}

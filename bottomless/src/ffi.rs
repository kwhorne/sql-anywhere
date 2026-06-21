pub use sqlanywhere_sys::ffi::{
    sqlanywhere_wal_methods, sqlite3, sqlite3_file, sqlite3_vfs, PageHdrIter, PgHdr, Wal, WalIndexHdr,
    SQLITE_CANTOPEN, SQLITE_CHECKPOINT_TRUNCATE, SQLITE_IOERR_WRITE, SQLITE_OK,
};

#[repr(C)]
pub struct bottomless_methods {
    pub methods: sqlanywhere_wal_methods,
    pub underlying_methods: *const sqlanywhere_wal_methods,
}

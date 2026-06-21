pub const SQLANYWHERE_INT: i8 = 1;
pub const SQLANYWHERE_FLOAT: i8 = 2;
pub const SQLANYWHERE_TEXT: i8 = 3;
pub const SQLANYWHERE_BLOB: i8 = 4;
pub const SQLANYWHERE_NULL: i8 = 5;

#[derive(Clone, Debug)]
#[repr(C)]
pub struct sqlanywhere_config {
    pub db_path: *const std::ffi::c_char,
    pub primary_url: *const std::ffi::c_char,
    pub auth_token: *const std::ffi::c_char,
    pub read_your_writes: std::ffi::c_char,
    pub encryption_key: *const std::ffi::c_char,
    pub sync_interval: std::ffi::c_int,
    pub with_webpki: std::ffi::c_char,
    pub offline: std::ffi::c_char,
    pub remote_encryption_key: *const std::ffi::c_char,
}

#[derive(Clone, Debug)]
#[repr(C)]
pub struct blob {
    pub ptr: *const std::ffi::c_char,
    pub len: std::ffi::c_int,
}

pub struct sqlanywhere_database {
    pub(crate) db: sqlanywhere::Database,
}

#[derive(Clone, Debug)]
#[repr(transparent)]
pub struct sqlanywhere_database_t {
    ptr: *const sqlanywhere_database,
}

impl sqlanywhere_database_t {
    pub fn null() -> sqlanywhere_database_t {
        sqlanywhere_database_t {
            ptr: std::ptr::null(),
        }
    }

    pub fn is_null(&self) -> bool {
        self.ptr.is_null()
    }

    pub fn get_ref(&self) -> &sqlanywhere::Database {
        &unsafe { &*(self.ptr) }.db
    }

    #[allow(clippy::mut_from_ref)]
    pub fn get_ref_mut(&self) -> &mut sqlanywhere::Database {
        let ptr_mut = self.ptr as *mut sqlanywhere_database;
        &mut unsafe { &mut (*ptr_mut) }.db
    }
}

#[allow(clippy::from_over_into)]
impl From<&sqlanywhere_database> for sqlanywhere_database_t {
    fn from(value: &sqlanywhere_database) -> Self {
        Self { ptr: value }
    }
}

#[allow(clippy::from_over_into)]
impl From<&mut sqlanywhere_database> for sqlanywhere_database_t {
    fn from(value: &mut sqlanywhere_database) -> Self {
        Self { ptr: value }
    }
}

pub struct sqlanywhere_connection {
    pub(crate) conn: sqlanywhere::Connection,
}

#[derive(Clone, Debug)]
#[repr(transparent)]
pub struct sqlanywhere_connection_t {
    ptr: *const sqlanywhere_connection,
}

impl sqlanywhere_connection_t {
    pub fn null() -> sqlanywhere_connection_t {
        sqlanywhere_connection_t {
            ptr: std::ptr::null(),
        }
    }

    pub fn is_null(&self) -> bool {
        self.ptr.is_null()
    }

    pub fn get_ref(&self) -> &sqlanywhere::Connection {
        &unsafe { &*(self.ptr) }.conn
    }

    #[allow(clippy::mut_from_ref)]
    pub fn get_ref_mut(&self) -> &mut sqlanywhere::Connection {
        let ptr_mut = self.ptr as *mut sqlanywhere_connection;
        &mut unsafe { &mut (*ptr_mut) }.conn
    }
}

#[allow(clippy::from_over_into)]
impl From<&sqlanywhere_connection> for sqlanywhere_connection_t {
    fn from(value: &sqlanywhere_connection) -> Self {
        Self { ptr: value }
    }
}

#[allow(clippy::from_over_into)]
impl From<&mut sqlanywhere_connection> for sqlanywhere_connection_t {
    fn from(value: &mut sqlanywhere_connection) -> Self {
        Self { ptr: value }
    }
}

#[repr(C)]
pub struct replicated {
    pub frame_no: std::ffi::c_int,
    pub frames_synced: std::ffi::c_int,
}

pub struct stmt {
    pub stmt: sqlanywhere::Statement,
    pub params: Vec<sqlanywhere::Value>,
}

pub struct sqlanywhere_stmt {
    pub stmt: stmt,
}

#[derive(Clone, Debug)]
#[repr(transparent)]
pub struct sqlanywhere_stmt_t {
    ptr: *const sqlanywhere_stmt,
}

impl sqlanywhere_stmt_t {
    pub fn null() -> sqlanywhere_stmt_t {
        sqlanywhere_stmt_t {
            ptr: std::ptr::null(),
        }
    }

    pub fn is_null(&self) -> bool {
        self.ptr.is_null()
    }

    pub fn get_ref(&self) -> &stmt {
        &unsafe { &*self.ptr }.stmt
    }

    #[allow(clippy::mut_from_ref)]
    pub fn get_ref_mut(&self) -> &mut stmt {
        let ptr_mut = self.ptr as *mut sqlanywhere_stmt;
        &mut unsafe { &mut (*ptr_mut) }.stmt
    }
}

#[allow(clippy::from_over_into)]
impl From<&sqlanywhere_stmt> for sqlanywhere_stmt_t {
    fn from(value: &sqlanywhere_stmt) -> Self {
        Self { ptr: value }
    }
}

#[allow(clippy::from_over_into)]
impl From<&mut sqlanywhere_stmt> for sqlanywhere_stmt_t {
    fn from(value: &mut sqlanywhere_stmt) -> Self {
        Self { ptr: value }
    }
}

pub struct sqlanywhere_rows {
    pub(crate) result: sqlanywhere::Rows,
}

#[derive(Clone, Debug)]
#[repr(transparent)]
pub struct sqlanywhere_rows_t {
    ptr: *const sqlanywhere_rows,
}

impl sqlanywhere_rows_t {
    pub fn null() -> sqlanywhere_rows_t {
        sqlanywhere_rows_t {
            ptr: std::ptr::null(),
        }
    }

    pub fn is_null(&self) -> bool {
        self.ptr.is_null()
    }

    pub fn get_ref(&self) -> &sqlanywhere::Rows {
        &unsafe { &*(self.ptr) }.result
    }

    #[allow(clippy::mut_from_ref)]
    pub fn get_ref_mut(&self) -> &mut sqlanywhere::Rows {
        let ptr_mut = self.ptr as *mut sqlanywhere_rows;
        &mut unsafe { &mut (*ptr_mut) }.result
    }
}

#[allow(clippy::from_over_into)]
impl From<&sqlanywhere_rows> for sqlanywhere_rows_t {
    fn from(value: &sqlanywhere_rows) -> Self {
        Self { ptr: value }
    }
}

#[allow(clippy::from_over_into)]
impl From<&mut sqlanywhere_rows> for sqlanywhere_rows_t {
    fn from(value: &mut sqlanywhere_rows) -> Self {
        Self { ptr: value }
    }
}

pub struct sqlanywhere_rows_future {
    pub(crate) result: sqlanywhere::RowsFuture,
}

#[derive(Clone, Debug)]
#[repr(transparent)]
pub struct sqlanywhere_rows_future_t {
    ptr: *const sqlanywhere_rows_future,
}

impl sqlanywhere_rows_future_t {
    pub fn null() -> sqlanywhere_rows_future_t {
        sqlanywhere_rows_future_t {
            ptr: std::ptr::null(),
        }
    }

    pub fn is_null(&self) -> bool {
        self.ptr.is_null()
    }

    pub fn get_ref(&self) -> &sqlanywhere::RowsFuture {
        &unsafe { &*(self.ptr) }.result
    }

    #[allow(clippy::mut_from_ref)]
    pub fn get_ref_mut(&self) -> &mut sqlanywhere::RowsFuture {
        let ptr_mut = self.ptr as *mut sqlanywhere_rows_future;
        &mut unsafe { &mut (*ptr_mut) }.result
    }
}

#[allow(clippy::from_over_into)]
impl From<&sqlanywhere_rows_future> for sqlanywhere_rows_future_t {
    fn from(value: &sqlanywhere_rows_future) -> Self {
        Self { ptr: value }
    }
}

#[allow(clippy::from_over_into)]
impl From<&mut sqlanywhere_rows_future> for sqlanywhere_rows_future_t {
    fn from(value: &mut sqlanywhere_rows_future) -> Self {
        Self { ptr: value }
    }
}
pub struct sqlanywhere_row {
    pub(crate) result: sqlanywhere::Row,
}

#[derive(Clone, Debug)]
#[repr(transparent)]
pub struct sqlanywhere_row_t {
    ptr: *const sqlanywhere_row,
}

impl sqlanywhere_row_t {
    pub fn null() -> sqlanywhere_row_t {
        sqlanywhere_row_t {
            ptr: std::ptr::null(),
        }
    }

    pub fn is_null(&self) -> bool {
        self.ptr.is_null()
    }

    pub fn get_ref(&self) -> &sqlanywhere::Row {
        &unsafe { &*(self.ptr) }.result
    }

    #[allow(clippy::mut_from_ref)]
    pub fn get_ref_mut(&self) -> &mut sqlanywhere::Row {
        let ptr_mut = self.ptr as *mut sqlanywhere_row;
        &mut unsafe { &mut (*ptr_mut) }.result
    }
}

impl From<&sqlanywhere_row> for sqlanywhere_row_t {
    fn from(value: &sqlanywhere_row) -> Self {
        Self { ptr: value }
    }
}

impl From<&mut sqlanywhere_row> for sqlanywhere_row_t {
    fn from(value: &mut sqlanywhere_row) -> Self {
        Self { ptr: value }
    }
}

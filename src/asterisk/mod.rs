use std::{mem::MaybeUninit, os::raw::c_int, ptr, string::FromUtf8Error};

pub mod bindings;

mod astobj2;

pub mod chan_discord;
pub mod channel;
pub mod config;
pub mod formats;
mod logger;

pub use astobj2::Ao2;
use bindings::{__IncompleteArrayField, ast_str, ast_threadstorage};
pub use logger::AsteriskLogger;

pub enum AsteriskError {
    GenericFailure,
}

#[inline]
fn asterisk_call(res: c_int) -> Result<(), AsteriskError> {
    match res {
        0 => Ok(()),
        _ => Err(AsteriskError::GenericFailure),
    }
}

#[repr(C, packed(1))]
struct FixedLengthString<const LEN: usize> {
    header: ast_str,
    buffer: [MaybeUninit<u8>; LEN],
}

impl<const LEN: usize> FixedLengthString<LEN> {
    pub fn new() -> Self {
        Self {
            header: ast_str {
                len: LEN,
                ts: 2 as *mut ast_threadstorage,
                used: 0,
                str_: __IncompleteArrayField::new(),
            },
            buffer: unsafe { MaybeUninit::uninit().assume_init() },
        }
    }

    pub fn as_ast_str(&mut self) -> *mut ast_str {
        ptr::addr_of_mut!(self.header)
    }

    pub fn copy_contents(&self) -> Result<String, FromUtf8Error> {
        let initialized_chunk = unsafe {
            std::slice::from_raw_parts(ptr::addr_of!(self.buffer[0]).cast(), self.header.used)
        };

        let vec = initialized_chunk.to_vec();
        String::from_utf8(vec)
    }
}

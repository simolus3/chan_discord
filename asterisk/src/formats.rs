use std::ptr;

use asterisk_sys::bindings::{
    __ast_format_cap_alloc, __ast_format_cap_append, ast_format, ast_format_cap,
    ast_format_cap_flags_AST_FORMAT_CAP_FLAG_DEFAULT, ast_format_cap_get_names,
    ast_format_cap_iscompatible, ast_format_slin48, AST_FORMAT_CAP_NAMES_LEN,
};

use crate::{
    asterisk_call,
    astobj2::{Ao2, AsteriskWrapper},
    AsteriskError, FixedLengthString,
};

#[repr(transparent)]
pub struct FormatCapabilities(pub ast_format_cap);
unsafe impl AsteriskWrapper<ast_format_cap> for FormatCapabilities {}

#[repr(transparent)]
pub struct Format(pub ast_format);
unsafe impl AsteriskWrapper<ast_format> for Format {}

impl FormatCapabilities {
    pub fn new() -> Option<Ao2<Self>> {
        unsafe {
            let ptr = __ast_format_cap_alloc(
                ast_format_cap_flags_AST_FORMAT_CAP_FLAG_DEFAULT,
                c"ast_format_cap_alloc".as_ptr(),
                file!().as_ptr().cast(),
                line!() as i32,
                c"FormatCapabilities::new".as_ptr(),
            )
            .cast();

            Some(Ao2::try_from_raw(ptr)?)
        }
    }

    pub fn append(&mut self, format: &Format, framing_ms: u32) -> Result<(), AsteriskError> {
        asterisk_call(unsafe {
            __ast_format_cap_append(
                ptr::addr_of_mut!(self.0),
                ptr::addr_of!(format.0).cast_mut(),
                framing_ms,
                c"ast_format_cap_append".as_ptr(),
                file!().as_ptr().cast(),
                line!() as i32,
                c"FormatCapabilities::append".as_ptr(),
            )
        })
    }

    pub fn format_names(&self) -> Option<String> {
        const LEN: usize = AST_FORMAT_CAP_NAMES_LEN as usize;
        let mut str = FixedLengthString::<LEN>::new();
        let mut str_buf = str.as_ast_str();
        unsafe { ast_format_cap_get_names(ptr::addr_of!(self.0), ptr::addr_of_mut!(str_buf)) };

        str.copy_contents().ok()
    }

    pub fn compatible_with(&self, other: &FormatCapabilities) -> bool {
        1 == unsafe { ast_format_cap_iscompatible(ptr::addr_of!(self.0), ptr::addr_of!(other.0)) }
    }
}

impl Format {
    pub fn slin48() -> Ao2<Self> {
        unsafe { Ao2::clone_raw(ast_format_slin48.cast()) }
    }
}

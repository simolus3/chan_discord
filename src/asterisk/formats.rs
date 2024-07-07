use std::{ffi::CString, ptr};

use super::{
    asterisk_call,
    bindings::{
        __ast_format_cap_alloc, __ast_format_cap_append, ast_format, ast_format_cap,
        ast_format_cap_flags_AST_FORMAT_CAP_FLAG_DEFAULT, ast_format_cap_get_names,
        ast_format_cap_iscompatible, ast_format_slin48, AST_FORMAT_CAP_NAMES_LEN,
    },
    Ao2, AsteriskError, FixedLengthString,
};

pub struct FormatCapabilities(pub Ao2<ast_format_cap>);
pub struct Format(pub Ao2<ast_format>);

impl FormatCapabilities {
    pub fn new() -> Option<Self> {
        unsafe {
            let ptr = __ast_format_cap_alloc(
                ast_format_cap_flags_AST_FORMAT_CAP_FLAG_DEFAULT,
                c"ast_format_cap_alloc".as_ptr(),
                file!().as_ptr().cast(),
                line!() as i32,
                c"FormatCapabilities::new".as_ptr(),
            );

            Some(Self(Ao2::try_move_from_raw(ptr)?))
        }
    }

    pub fn append(&self, format: Format, framing_ms: u32) -> Result<(), AsteriskError> {
        asterisk_call(unsafe {
            __ast_format_cap_append(
                self.0.as_ptr(),
                format.0.into_raw(),
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
        unsafe { ast_format_cap_get_names(self.0.as_ptr(), ptr::addr_of_mut!(str_buf)) };

        str.copy_contents().ok()
    }

    pub fn compatible_with(&self, other: &FormatCapabilities) -> bool {
        1 == unsafe { ast_format_cap_iscompatible(self.0.as_ptr(), other.0.as_ptr()) }
    }
}

impl Format {
    pub fn slin48() -> Self {
        Self(unsafe { Ao2::clone_from_raw(ast_format_slin48.cast()) })
    }
}

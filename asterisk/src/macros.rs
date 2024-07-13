#[macro_export]
macro_rules! c_str {
    ($lit:expr) => {
        unsafe {
            let data = concat!($lit, "\0").as_ptr();
            let cstr = std::ffi::CStr::from_ptr(data as *const std::os::raw::c_char);

            cstr.as_ptr().cast()
        }
    };
}

#[macro_export]
macro_rules! c_file {
    () => {
        c_str!(file!())
    };
}

#[macro_export]
macro_rules! c_line {
    () => {
        line!() as i32
    };
}

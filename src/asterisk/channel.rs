use std::os::raw::c_void;

use super::{
    bindings::{
        ast_channel, ast_channel_nativeformats_set, ast_channel_set_readformat,
        ast_channel_set_writeformat, ast_channel_tech_pvt, ast_channel_tech_pvt_set,
        ast_control_frame_type, ast_queue_control, ast_queue_hangup,
    },
    formats::{Format, FormatCapabilities},
    Ao2,
};

#[derive(Clone)]
pub struct Channel(pub Ao2<ast_channel>);

impl Channel {
    pub fn set_readformat(&self, format: Format) {
        unsafe { ast_channel_set_readformat(self.0.as_ptr(), format.0.as_ptr()) }
    }

    pub fn set_writeformat(&self, format: Format) {
        unsafe { ast_channel_set_writeformat(self.0.as_ptr(), format.0.as_ptr()) }
    }

    pub fn set_native_formats(&self, caps: FormatCapabilities) {
        unsafe { ast_channel_nativeformats_set(self.0.as_ptr(), caps.0.as_ptr()) }
    }

    pub fn set_tech_data(&self, data: *mut c_void) {
        unsafe { ast_channel_tech_pvt_set(self.0.as_ptr(), data) }
    }

    pub fn get_tech_data(&self) -> *mut c_void {
        unsafe { ast_channel_tech_pvt(self.0.as_ptr()) }
    }

    pub fn queue_hangup(&self) {
        unsafe { ast_queue_hangup(self.0.as_ptr()) };
    }

    pub fn queue_control(&self, control: ast_control_frame_type) {
        unsafe { ast_queue_control(self.0.as_ptr(), control) };
    }
}

use std::{os::raw::c_void, ptr};

use asterisk_sys::bindings::{
    ast_channel, ast_channel_nativeformats, ast_channel_nativeformats_set,
    ast_channel_set_readformat, ast_channel_set_writeformat, ast_channel_stage_snapshot,
    ast_channel_stage_snapshot_done, ast_channel_tech_pvt, ast_channel_tech_pvt_set,
    ast_control_frame_type, ast_frame, ast_queue_control, ast_queue_frame, ast_queue_hangup,
};

use crate::{
    astobj2::{Ao2, AsteriskWrapper},
    formats::{Format, FormatCapabilities},
};

#[repr(transparent)]
pub struct Channel(pub ast_channel);
unsafe impl AsteriskWrapper<ast_channel> for Channel {}

impl Channel {
    pub fn set_readformat(&mut self, format: &Format) {
        unsafe {
            ast_channel_set_readformat(
                ptr::addr_of_mut!(self.0),
                ptr::addr_of!(format.0).cast_mut(),
            )
        }
    }

    pub fn set_writeformat(&mut self, format: &Format) {
        unsafe {
            ast_channel_set_writeformat(
                ptr::addr_of_mut!(self.0),
                ptr::addr_of!(format.0).cast_mut(),
            )
        }
    }

    pub fn set_native_formats(&mut self, caps: &FormatCapabilities) {
        unsafe {
            ast_channel_nativeformats_set(
                ptr::addr_of_mut!(self.0),
                ptr::addr_of!(caps.0).cast_mut(),
            )
        }
    }

    pub fn get_native_formats(&self) -> Ao2<FormatCapabilities> {
        FormatCapabilities::from_obj(unsafe {
            Ao2::clone_raw(ast_channel_nativeformats(ptr::addr_of!(self.0)))
        })
    }

    pub fn set_tech_data(&mut self, data: *mut c_void) {
        unsafe { ast_channel_tech_pvt_set(ptr::addr_of_mut!(self.0), data) }
    }

    pub fn get_tech_data(&self) -> *mut c_void {
        unsafe { ast_channel_tech_pvt(ptr::addr_of!(self.0)) }
    }

    pub fn queue_hangup(&self) {
        unsafe { ast_queue_hangup(ptr::addr_of!(self.0).cast_mut()) };
    }

    pub fn queue_control(&self, control: ast_control_frame_type) {
        unsafe { ast_queue_control(ptr::addr_of!(self.0).cast_mut(), control) };
    }

    pub fn queue_frame(&self, frame: &mut ast_frame) {
        unsafe { ast_queue_frame(ptr::addr_of!(self.0).cast_mut(), std::ptr::from_mut(frame)) };
    }

    pub fn stage_snapshot<'a>(&'a mut self) -> StagedSnapshot<'a> {
        unsafe { ast_channel_stage_snapshot(ptr::addr_of_mut!(*self.to_asterisk_mut())) }
        StagedSnapshot { channel: self }
    }
}

pub struct StagedSnapshot<'a> {
    pub channel: &'a mut Channel,
}

impl<'a> StagedSnapshot<'a> {
    pub fn done(self) {}
}

impl Drop for StagedSnapshot<'_> {
    fn drop(&mut self) {
        unsafe {
            ast_channel_stage_snapshot_done(ptr::addr_of_mut!(*self.channel.to_asterisk_mut()));
        }
    }
}

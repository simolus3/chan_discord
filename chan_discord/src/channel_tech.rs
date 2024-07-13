use std::{
    ffi::{c_char, c_int, CStr},
    os::raw::c_void,
    ptr::{self, null, null_mut},
};

use asterisk::{
    astobj2::{Ao2, AsteriskWrapper},
    c_file, c_line, c_str,
    channel::Channel,
    formats::{Format, FormatCapabilities},
};
use log::{debug, trace, warn};

use asterisk_sys::bindings::{
    __ast_channel_alloc, ama_flags_AST_AMA_NONE, ast_assigned_ids, ast_channel,
    ast_channel_state_AST_STATE_DOWN, ast_channel_tech, ast_format_cap, ast_frame, ast_null_frame,
};

use crate::{call::CallHandle, with_worker};

pub static mut DISCORD_TECH: ast_channel_tech = const {
    let mut tech = unsafe { std::mem::zeroed::<ast_channel_tech>() };
    tech.type_ = c"Discord".as_ptr();
    tech.description = c"Join discord voice channels from Asterisk".as_ptr();

    tech.requester = Some(requester);
    tech.call = Some(call);
    tech.hangup = Some(hangup);
    tech.fixup = Some(fixup);

    tech.read = Some(read);
    tech.write = Some(write);

    tech
};

unsafe extern "C" fn read(_chan: *mut ast_channel) -> *mut ast_frame {
    debug!("Should not call read for Discord channels, we're pushing frames into channel");
    ptr::addr_of_mut!(ast_null_frame)
}

unsafe extern "C" fn write(chan: *mut ast_channel, data: *mut ast_frame) -> c_int {
    // Note: We have an exclusive lock on the channel when write gets called.
    let chan = Channel::from_asterisk_mut(chan.as_mut().unwrap());
    let call = chan.get_tech_data().cast::<CallHandle>().as_mut().unwrap();

    match call.write_frame(data.as_ref().unwrap()) {
        Ok(()) => 0,
        Err(e) => {
            trace!("Could not write frame: {e}");
            1
        }
    }
}

unsafe extern "C" fn requester(
    _: *const c_char,
    cap: *mut ast_format_cap,
    ids: *const ast_assigned_ids,
    requestor: *const ast_channel,
    addr: *const c_char,
    _cause: *mut c_int,
) -> *mut ast_channel {
    let Some(destination) = CallHandle::parse_destination_addr(CStr::from_ptr(addr)) else {
        warn!(
            "Requested discord call with invalid destination {:?}, format is <server>/<channel>",
            CStr::from_ptr(addr)
        );
        return null_mut();
    };

    if !requestor.is_null() {
        let requestor = Channel::from_asterisk(requestor.as_ref().unwrap());
        trace!(
            "Requestor for discord channel has capabilities: {:?}",
            requestor.get_native_formats().format_names()
        );
    }

    let Some(capabilities) = FormatCapabilities::new() else {
        return null_mut();
    };
    if capabilities.as_mut().append(&Format::slin48(), 20).is_err() {
        return null_mut();
    }

    let cap = FormatCapabilities::from_asterisk(cap.as_ref().unwrap());
    if !cap.compatible_with(&capabilities) {
        warn!(
            "Requested incompatible channel! Discord supports {:?}, but requested was {:?}",
            capabilities.format_names(),
            cap.format_names()
        );
    }

    let Some(channel) = Ao2::try_from_raw(__ast_channel_alloc(
        1, // We need a frame queue because we're pushing frames into this channel
        ast_channel_state_AST_STATE_DOWN as c_int,
        null(), // CID -> number
        null(), // CID -> name
        null(), // Account code
        null(), // Extension
        null(), // context
        ids,
        requestor,
        ama_flags_AST_AMA_NONE,
        null_mut(),
        c_file!(),
        c_line!(),
        c_str!("requester"),
        c"Discord/%s".as_ptr(),
        addr,
    )) else {
        return null_mut();
    };
    let channel = Channel::from_obj(channel);
    // __ast_channel_alloc returns a locked channel -> move ownership of the lock into here
    let mut channel_lock = channel.move_lock();
    let snapshot = channel_lock.stage_snapshot();

    snapshot.channel.set_readformat(&Format::slin48());
    snapshot.channel.set_writeformat(&Format::slin48());
    snapshot.channel.set_native_formats(&capabilities);

    let Some(call) =
        with_worker(|discord| discord.prepare_call(channel.clone(), destination.0, destination.1))
    else {
        warn!("Worker not set up, can't start channel.");
        return null_mut();
    };

    let call = match call {
        Ok(call) => call,
        Err(e) => {
            warn!(
                "Could not prepare call to {}/{}: {}",
                destination.0, destination.1, e
            );
            return null_mut();
        }
    };

    snapshot
        .channel
        .set_tech_data(Box::into_raw(Box::new(call)).cast::<c_void>());
    snapshot.done();
    drop(channel_lock);
    Channel::into_raw(channel)
}

unsafe extern "C" fn call(chan: *mut ast_channel, _addr: *const c_char, _timeout: c_int) -> c_int {
    // Note: This is called with an exclusive lock on the channel, so we can use mut
    let chan = Channel::from_asterisk_mut(chan.as_mut().unwrap());
    let call = chan.get_tech_data().cast::<CallHandle>().as_ref().unwrap();

    match call.start_joining() {
        Ok(()) => 0,
        Err(e) => {
            debug!(
                "Could not join Discord channel with allocated worker: {}",
                e
            );
            1
        }
    }
}

unsafe extern "C" fn hangup(chan: *mut ast_channel) -> c_int {
    let chan = Channel::from_asterisk_mut(chan.as_mut().unwrap());
    let call = Box::from_raw(chan.get_tech_data().cast::<CallHandle>());
    trace!("hangup called on discord channel tech");

    let res = match call.hangup() {
        Ok(()) => 0,
        Err(e) => {
            debug!("Could not hang up: {e:?}");
            1
        }
    };
    chan.set_tech_data(null_mut());
    res
}

unsafe extern "C" fn fixup(_old: *mut ast_channel, new: *mut ast_channel) -> c_int {
    // We need to drop references to the old channel in our CallHandle structure
    let chan = Channel::from_obj(Ao2::clone_raw(new));
    let call = Box::from_raw(chan.get_tech_data().cast::<CallHandle>());
    match call.fixup(chan) {
        Ok(()) => 0,
        Err(e) => {
            debug!("Error during fixup: {e:?}");
            1
        }
    }
}

use std::{
    ffi::{c_char, c_int, CStr},
    os::raw::c_void,
    ptr::{self, null, null_mut},
};

use function_name::named;
use log::{debug, warn};

use crate::{
    asterisk::{
        bindings::{
            ama_flags_AST_AMA_NONE, ast_channel_state_AST_STATE_DOWN, ast_channel_tech,
            ast_null_frame,
        },
        channel::Channel,
        formats::{Format, FormatCapabilities},
        Ao2,
    },
    call::CallHandle,
    with_worker,
};

use super::bindings::{
    __ast_channel_alloc, ast_assigned_ids, ast_channel, ast_format_cap, ast_frame,
};

// Discord wants 48kHz
const SAMPLE_RATE: u16 = 48_000;

// 20ms of audio at 48kHz, 20 ms is apparently the most common frame size in Asterisk.
const NUM_SAMPLES: u16 = 960;

pub static mut DISCORD_TECH: ast_channel_tech = const {
    let mut tech = unsafe { std::mem::zeroed::<ast_channel_tech>() };
    tech.type_ = c"Discord".as_ptr();
    tech.description = c"Join discord voice channels from Asterisk".as_ptr();
    tech.read = Some(read);
    tech.requester = Some(requester);
    tech.call = Some(call);
    tech.hangup = Some(hangup);

    tech
};

unsafe extern "C" fn read(_chan: *mut ast_channel) -> *mut ast_frame {
    debug!("Should not call read for Discord channels, we're pushing frames into channel");
    ptr::addr_of_mut!(ast_null_frame)
}

#[named]
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

    let Some(capabilities) = FormatCapabilities::new() else {
        return null_mut();
    };
    if capabilities.append(Format::slin48(), 20).is_err() {
        return null_mut();
    }

    let cap = FormatCapabilities(Ao2::clone_from_raw(cap.cast()));
    if !cap.compatible_with(&capabilities) {
        warn!(
            "Requested incompatible channel! Discord supports {:?}, but requested was {:?}",
            capabilities.format_names(),
            cap.format_names()
        );
    }

    let Some(channel) = Ao2::try_move_from_raw(__ast_channel_alloc(
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
        c_file!().as_ptr(),
        line!() as i32,
        c_str!(function_name!()).as_ptr(),
        c"Discord/%s".as_ptr(),
        addr,
    )) else {
        return null_mut();
    };
    let channel = Channel(channel);
    channel.set_readformat(Format::slin48());
    channel.set_writeformat(Format::slin48());
    channel.set_native_formats(capabilities);

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

    channel.set_tech_data(Box::into_raw(Box::new(call)).cast::<c_void>());
    channel.0.into_raw()
}

unsafe extern "C" fn call(chan: *mut ast_channel, _addr: *const c_char, _timeout: c_int) -> c_int {
    let chan = Channel(Ao2::clone_from_raw(chan));
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
    let chan = Channel(Ao2::clone_from_raw(chan));
    let call = Box::from_raw(chan.get_tech_data().cast::<CallHandle>());

    match call.hangup() {
        Ok(()) => 0,
        Err(e) => {
            debug!("Could not hang up: {e}");
            1
        }
    }
}

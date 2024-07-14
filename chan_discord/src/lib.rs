use std::{
    ffi::c_int,
    ptr::{self, addr_of_mut, null, null_mut},
    sync::{OnceLock, RwLock},
};

use asterisk::{
    astobj2::{Ao2, AsteriskWrapper},
    config::AsteriskConfig,
    formats::{Format, FormatCapabilities},
    logger::AsteriskLogger,
};
use asterisk_sys::bindings::{
    ast_channel_register, ast_channel_unregister, ast_format_cap, ast_module_info,
    ast_module_load_result_AST_MODULE_LOAD_DECLINE, ast_module_load_result_AST_MODULE_LOAD_SUCCESS,
    ast_module_register, ast_module_support_level_AST_MODULE_SUPPORT_UNKNOWN,
    ast_module_unregister,
};
use channel_tech::DISCORD_TECH;
use ctor::{ctor, dtor};
use log::{info, warn};
use queue_thread::QueueThread;
use thread::DiscordThread;

mod call;
mod channel_tech;
mod queue_thread;
mod rtp_receiver;
mod thread;

static WORKER: OnceLock<RwLock<Option<DiscordThread>>> = OnceLock::new();
static QUEUE_THREAD: OnceLock<QueueThread> = OnceLock::new();

struct ModuleOptions {
    token: String,
}

impl ModuleOptions {
    fn from_config(config: &AsteriskConfig) -> Option<Self> {
        let category = config.category(c"general")?;
        let mut token: Option<String> = None;

        for variable in &category {
            let Ok(name) = variable.name().to_str() else {
                continue;
            };
            let Ok(value) = variable.value().to_str() else {
                warn!("Invalid config field {name}: Not valid utf8");
                return None;
            };

            if name == "token" {
                token = Some(value.to_string());
            } else {
                info!("Unknown variable {name} in configuration file");
            }
        }

        Some(ModuleOptions { token: token? })
    }
}

pub fn with_worker<F, R>(body: F) -> Option<R>
where
    F: FnOnce(&DiscordThread) -> R,
{
    let worker = WORKER.get()?;
    let locked = worker.read().ok()?;
    let discord = locked.as_ref()?;
    Some(body(discord))
}

pub fn queue_thread() -> QueueThread {
    let queue = QUEUE_THREAD.get_or_init(|| QueueThread::start());
    queue.clone()
}

unsafe extern "C" fn load_module() -> c_int {
    if cfg!(debug_assertions) {
        println!(
            "Asterisk PID (if you need to attach a debugger): {}",
            std::process::id()
        );
    }

    if log::set_logger(&AsteriskLogger)
        .map(|()| log::set_max_level(log::LevelFilter::Trace))
        .is_err()
    {
        println!("Warning: Logger for Rust could not be set");
        return ast_module_load_result_AST_MODULE_LOAD_DECLINE;
    }

    // Initialize channel technology
    let Some(capabilities) = FormatCapabilities::new() else {
        return ast_module_load_result_AST_MODULE_LOAD_DECLINE;
    };
    if capabilities.as_mut().append(&Format::slin48(), 20).is_err() {
        return ast_module_load_result_AST_MODULE_LOAD_DECLINE;
    }
    unsafe {
        DISCORD_TECH.capabilities = FormatCapabilities::into_raw(capabilities);
    }

    // Read token from option
    let Ok(config) = AsteriskConfig::load(c"discord.conf", &*ptr::addr_of_mut!(INFO)) else {
        info!("Could not load configuration file at discord.conf");
        return ast_module_load_result_AST_MODULE_LOAD_DECLINE;
    };
    let Some(options) = ModuleOptions::from_config(&config) else {
        info!("Missing token option in general section");
        return ast_module_load_result_AST_MODULE_LOAD_DECLINE;
    };

    // Try to spawn the worker
    let discord = match DiscordThread::start(options.token) {
        Ok(discord) => discord,
        Err(e) => {
            warn!("Could not start discord: {e}");
            return ast_module_load_result_AST_MODULE_LOAD_DECLINE;
        }
    };
    let _ = WORKER.set(RwLock::new(Some(discord)));

    // Register channel technology
    ast_channel_register(ptr::addr_of!(DISCORD_TECH));

    ast_module_load_result_AST_MODULE_LOAD_SUCCESS
}

unsafe extern "C" fn reload_module() -> c_int {
    0
}

unsafe extern "C" fn unload_module() -> c_int {
    if let Some(lock) = WORKER.get() {
        let mut write = lock.write().unwrap();
        write.take();
    }

    ast_channel_unregister(ptr::addr_of!(DISCORD_TECH));

    let old_capabilities = std::mem::replace(&mut DISCORD_TECH.capabilities, null_mut());
    if !old_capabilities.is_null() {
        // Drop reference
        Ao2::<ast_format_cap>::from_raw(old_capabilities.cast());
    }

    if let Some(worker) = WORKER.get() {
        let mut locked = worker.write().unwrap();
        locked.take();
    }

    0
}

static mut INFO: ast_module_info = const {
    ast_module_info {
        self_: null_mut(), // Will be set by the loader
        load: Some(load_module),
        reload: Some(reload_module),
        unload: Some(unload_module),
        name: c"chan_discord".as_ptr(),
        description: c"Support for Discord calls.".as_ptr(),
        key: c"This paragraph is copyright (c) 2006 by Digium, Inc. \
In order for your module to load, it must return this \
key via a function called \"key\".  Any code which \
includes this paragraph must be licensed under the GNU \
General Public License version 2 or later (at your \
option).  In addition to Digium's general reservations \
of rights, Digium expressly reserves the right to \
allow other parties to license this paragraph under \
different terms. Any use of Digium, Inc. trademarks or \
logos (including \"Asterisk\" or \"Digium\") without \
express written permission of Digium, Inc. is prohibited.\n"
            .as_ptr(),
        flags: 0,
        buildopt_sum: [0; 33],
        load_pri: 10,
        requires: null(),
        optional_modules: null(),
        enhances: null(),
        reserved1: null_mut(),
        reserved2: null_mut(),
        reserved3: null_mut(),
        reserved4: null_mut(),
        support_level: ast_module_support_level_AST_MODULE_SUPPORT_UNKNOWN,
    }
};

#[ctor]
unsafe fn register_module() {
    ast_module_register(addr_of_mut!(INFO));
}

#[dtor]
unsafe fn unregister_module() {
    ast_module_unregister(addr_of_mut!(INFO));
}

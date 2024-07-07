use std::{ffi::c_void, mem::size_of, ptr::NonNull};

use super::bindings::{
    ao2_alloc_opts_AO2_ALLOC_OPT_LOCK_MUTEX, ao2_alloc_opts_AO2_ALLOC_OPT_LOCK_NOLOCK,
    ao2_alloc_opts_AO2_ALLOC_OPT_LOCK_RWLOCK, rust_ao2_alloc, rust_ao2_ref,
};

/// An Asterisk-allocated object, for which Asterisk manages refcounts and destructor handling.
pub struct Ao2<T>(NonNull<T>);

enum AllocLockOptions {
    RecursiveMutex = ao2_alloc_opts_AO2_ALLOC_OPT_LOCK_MUTEX as isize,
    ReadWriteLock = ao2_alloc_opts_AO2_ALLOC_OPT_LOCK_RWLOCK as isize,
    NoLock = ao2_alloc_opts_AO2_ALLOC_OPT_LOCK_NOLOCK as isize,
}

unsafe impl<T> Send for Ao2<T> {}
unsafe impl<T> Sync for Ao2<T> {}

impl<T> Ao2<T> {
    pub unsafe fn try_move_from_raw(ptr: *mut T) -> Option<Self> {
        Some(Self(NonNull::new(ptr.cast())?))
    }

    pub unsafe fn move_from_raw(ptr: *mut c_void) -> Self {
        Self(NonNull::new(ptr.cast()).expect("ao2_alloc returns non-null pointer"))
    }

    pub unsafe fn clone_from_raw(ptr: *mut T) -> Self {
        rust_ao2_ref(ptr.cast(), 1);
        Self(NonNull::new(ptr.cast()).expect("ao2_alloc returns non-null pointer"))
    }

    pub fn new(value: T) -> Self {
        Self::new_with_options(value, AllocLockOptions::NoLock)
    }

    pub fn new_with_options(value: T, options: AllocLockOptions) -> Self {
        let ptr = unsafe { Self::new_uninit(options) };
        unsafe {
            std::ptr::write(ptr.0.as_ptr(), value);
        }
        ptr
    }

    pub unsafe fn new_uninit(options: AllocLockOptions) -> Self {
        let ptr = rust_ao2_alloc(size_of::<T>(), Some(Self::destruct), options as u32);
        Self::move_from_raw(ptr)
    }

    pub fn as_ptr(&self) -> *mut T {
        self.0.as_ptr()
    }

    pub fn into_raw(mut self) -> *mut T {
        let ptr = unsafe { self.0.as_mut() };
        std::mem::forget(self);
        ptr
    }

    unsafe extern "C" fn destruct(ptr: *mut c_void) {
        ptr.cast::<T>().drop_in_place();
    }
}

impl<T> Clone for Ao2<T> {
    fn clone(&self) -> Self {
        unsafe {
            rust_ao2_ref(self.0.as_ptr().cast(), 1);
        }
        return Self(self.0);
    }
}

use std::{
    ffi::c_void,
    fmt::Debug,
    mem::size_of,
    ops::{Deref, DerefMut},
    ptr::{null, NonNull},
};

use asterisk_sys::bindings::{
    __ao2_alloc, __ao2_lock, __ao2_ref, __ao2_unlock, ao2_alloc_opts,
    ao2_alloc_opts_AO2_ALLOC_OPT_LOCK_MUTEX, ao2_alloc_opts_AO2_ALLOC_OPT_LOCK_NOLOCK,
    ao2_alloc_opts_AO2_ALLOC_OPT_LOCK_RWLOCK, ao2_lock_req,
};
use bitflags::bitflags;
use log::trace;

use crate::{asterisk_call, AsteriskError};

/// Utilities for transparent newtypes, e.g. `struct Format(ast_format)`.
pub unsafe trait AsteriskWrapper<A>: Sized {
    fn to_asterisk(&self) -> &A {
        unsafe { std::mem::transmute::<&Self, &A>(self) }
    }
    fn from_asterisk(raw: &A) -> &Self {
        unsafe { std::mem::transmute::<&A, &Self>(raw) }
    }

    fn to_asterisk_mut(&mut self) -> &mut A {
        unsafe { std::mem::transmute::<&mut Self, &mut A>(self) }
    }
    fn from_asterisk_mut(raw: &mut A) -> &mut Self {
        unsafe { std::mem::transmute::<&mut A, &mut Self>(raw) }
    }

    fn from_obj(obj: Ao2<A>) -> Ao2<Self> {
        unsafe { Ao2::from_raw(obj.into_raw().cast()) }
    }

    fn into_raw(obj: Ao2<Self>) -> *mut A {
        obj.into_raw().cast()
    }
}

/// An Asterisk-allocated object, for which Asterisk manages refcounts and destructor handling.
pub struct Ao2<T>(NonNull<T>);

pub struct Ao2Lock<'a, T> {
    object: &'a Ao2<T>,
    needs_unlock: bool,
}

unsafe impl<T> Send for Ao2<T> {}
unsafe impl<T> Sync for Ao2<T> {}

bitflags! {
    pub struct AllocLockOptions: ao2_alloc_opts {
        const RECURSIVE_MUTEX = ao2_alloc_opts_AO2_ALLOC_OPT_LOCK_MUTEX;
        const READ_WRITE_LOCK = ao2_alloc_opts_AO2_ALLOC_OPT_LOCK_RWLOCK;
        const NO_LOCK = ao2_alloc_opts_AO2_ALLOC_OPT_LOCK_NOLOCK;
    }
}

impl<T> Ao2<T> {
    pub fn new(value: T) -> Self {
        Self::new_with_options(value, AllocLockOptions::NO_LOCK)
    }

    pub fn new_with_options(value: T, options: AllocLockOptions) -> Self {
        let ptr = unsafe { Self::new_uninit(options) };
        unsafe {
            std::ptr::write(ptr.0.as_ptr(), value);
        }
        ptr
    }

    pub unsafe fn new_uninit(options: AllocLockOptions) -> Self {
        let ptr = __ao2_alloc(
            size_of::<T>(),
            Some(Self::destruct),
            options.bits(),
            null(),
            c_file!(),
            c_line!(),
            null(),
        );
        Self::from_raw(ptr.cast())
    }

    pub fn as_ptr(&self) -> *mut T {
        self.0.as_ptr()
    }

    pub unsafe fn from_raw(ptr: *mut c_void) -> Self {
        Self(NonNull::new(ptr.cast()).expect("ao2_alloc returns non-null pointer"))
    }

    pub unsafe fn try_from_raw(ptr: *mut T) -> Option<Self> {
        Some(Self(NonNull::new(ptr)?))
    }

    pub unsafe fn clone_raw(ptr: *mut T) -> Self {
        __ao2_ref(ptr.cast(), 1, null(), c_file!(), c_line!(), null());
        Self(NonNull::new(ptr).expect("ao2_alloc returns non-null pointer"))
    }

    pub fn into_raw(mut self) -> *mut T {
        let ptr = unsafe { self.0.as_mut() };
        std::mem::forget(self);
        ptr
    }

    unsafe extern "C" fn destruct(ptr: *mut c_void) {
        ptr.cast::<T>().drop_in_place();
    }

    pub unsafe fn lock<'a>(&'a self, req: ao2_lock_req) -> Result<Ao2Lock<'a, T>, AsteriskError> {
        asterisk_call(__ao2_lock(
            self.as_ptr().cast(),
            req,
            c_file!(),
            c"Ao2::lock".as_ptr(),
            c_line!(),
            c"self".as_ptr(),
        ))?;

        Ok(Ao2Lock {
            object: self,
            needs_unlock: true,
        })
    }

    pub unsafe fn move_lock<'a>(&'a self) -> Ao2Lock<'a, T> {
        Ao2Lock {
            object: self,
            needs_unlock: true,
        }
    }

    pub unsafe fn as_mut<'a>(&'a self) -> Ao2Lock<'a, T> {
        Ao2Lock {
            object: self,
            needs_unlock: false,
        }
    }

    pub unsafe fn unlock(&self) {
        __ao2_unlock(
            self.0.as_ptr().cast(),
            c_file!(),
            c"Ao2::unlock".as_ptr(),
            line!() as i32,
            c"self".as_ptr(),
        );
    }
}

impl<T> Clone for Ao2<T> {
    fn clone(&self) -> Self {
        unsafe { Self::clone_raw(self.as_ptr()) }
    }
}

impl<T> Drop for Ao2<T> {
    fn drop(&mut self) {
        let ptr = self.0.as_ptr();
        unsafe {
            __ao2_ref(ptr.cast(), -1, null(), c_file!(), c_line!(), null());
        }
    }
}

impl<T> Debug for Ao2<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Ao2<{}>({:p})", std::any::type_name::<T>(), &self.0)
    }
}

impl<T> Deref for Ao2<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { self.0.as_ref() }
    }
}

impl<'a, T> Deref for Ao2Lock<'a, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        Deref::deref(&self.object)
    }
}

impl<'a, T> DerefMut for Ao2Lock<'a, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        let mut ptr = self.object.0;
        unsafe { ptr.as_mut() }
    }
}

impl<T> Drop for Ao2Lock<'_, T> {
    fn drop(&mut self) {
        if self.needs_unlock {
            unsafe { self.object.unlock() }
        }
    }
}

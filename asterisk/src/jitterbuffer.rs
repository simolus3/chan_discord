use std::{
    ffi::c_long,
    marker::PhantomData,
    mem::MaybeUninit,
    num::NonZero,
    ptr::{self, null_mut},
    time::{Duration, Instant},
};

use asterisk_sys::bindings::{
    jb_conf, jb_destroy, jb_frame, jb_frame_type, jb_frame_type_JB_TYPE_CONTROL,
    jb_frame_type_JB_TYPE_SILENCE, jb_frame_type_JB_TYPE_VIDEO, jb_frame_type_JB_TYPE_VOICE,
    jb_get, jb_getall, jb_new, jb_next, jb_put, jb_return_code, jb_return_code_JB_DROP,
    jb_return_code_JB_EMPTY, jb_return_code_JB_INTERP, jb_return_code_JB_NOFRAME,
    jb_return_code_JB_OK, jb_return_code_JB_SCHED, jb_setconf, jitterbuf,
};
use thiserror::Error;

pub struct JitterBuffer<T> {
    buf: *mut jitterbuf,
    entries: PhantomData<Box<T>>,
    reference_time: Instant,
}

unsafe impl<T> Send for JitterBuffer<T> {}

#[derive(Debug)]
pub enum JitterFrameType {
    Control,
    Voice,
    Video,
    Silence,
}

#[derive(Debug)]
pub struct JitterFrame<T> {
    pub data: Box<T>,
    pub duration: Duration,
    pub ts: c_long,
    pub frame_type: JitterFrameType,
}

#[derive(Error, Debug)]
pub enum JitterBufferErr<T> {
    #[error("JB_EMPTY: This jitterbuffer is empty")]
    Empty,
    #[error("JB_NOFRAME: There's no frame scheduled for this time")]
    NoFrame,
    #[error("JB_INTERP: Please interpolate an interpl-length frame for this time (either we need to grow, or there was a lost frame)")]
    Interpolate,
    #[error("JB_DROP: Here's an audio frame you should just drop")]
    Drop { frame: JitterFrame<T> },
    #[error("JB_SCHED: Frame added - call jb_next to get a new time for the next frame.")]
    Scheduled,
}

impl<T> JitterBuffer<T> {
    pub fn new(config: &mut jb_conf) -> Self {
        let mut buf = Self {
            buf: unsafe { jb_new() },
            entries: PhantomData,
            reference_time: Instant::now(),
        };
        buf.setconf(config);

        buf
    }

    pub fn get_unconditionally(&mut self) -> Result<JitterFrame<T>, JitterBufferErr<T>> {
        let mut frame = MaybeUninit::uninit();
        let code = unsafe { jb_getall(self.buf, frame.as_mut_ptr()) };
        Self::interpret_frame_result(frame, code)
    }

    pub fn get(
        &mut self,
        expected_frame_length: Duration,
    ) -> Result<JitterFrame<T>, JitterBufferErr<T>> {
        let mut frame = MaybeUninit::uninit();
        let code = unsafe {
            jb_get(
                self.buf,
                frame.as_mut_ptr(),
                self.receiver_timestamp(Instant::now()),
                expected_frame_length.as_millis() as i64,
            )
        };
        Self::interpret_frame_result(frame, code)
    }

    pub fn put(
        &mut self,
        data: Box<T>,
        frame_type: JitterFrameType,
        length: Duration,
        ts: i64,
    ) -> Result<(), JitterBufferErr<T>> {
        let raw_data = Box::into_raw(data);
        let ms = length.as_millis() as i64;
        let now = self.receiver_timestamp(Instant::now());
        let frame_type = frame_type.into();

        let code = unsafe {
            jb_put(
                self.buf,
                raw_data.cast(),
                frame_type,
                length.as_millis() as i64,
                ts,
                now,
            )
        };

        if jb_return_code_JB_OK == code {
            return Ok(());
        }

        // We need to construct a fake frame so that we can properly free it if the code is
        // JB_DROP.
        let frame = MaybeUninit::new(jb_frame {
            data: raw_data.cast(),
            ts,
            ms,
            type_: frame_type,
            next: null_mut(),
            prev: null_mut(),
        });

        Self::interpret_frame_result(frame, code)?;
        unreachable!("Already checked for OK");
    }

    /// Returns the timestamp at which the next frame for this buffer is due to be sent.
    pub fn next_frame(&self) -> Option<Instant> {
        let time = NonZero::new(unsafe { jb_next(self.buf) })?;
        Some(self.reference_time + Duration::from_millis(time.get() as u64))
    }

    fn setconf(&mut self, config: &mut jb_conf) {
        unsafe { jb_setconf(self.buf, ptr::addr_of_mut!(*config)) };
    }

    fn receiver_timestamp(&self, time: Instant) -> i64 {
        time.duration_since(self.reference_time).as_millis() as i64
    }

    fn interpret_frame(frame: jb_frame) -> JitterFrame<T> {
        JitterFrame {
            data: unsafe { Box::from_raw(frame.data.cast()) },
            duration: Duration::from_millis(frame.ms as u64),
            ts: frame.ts,
            frame_type: JitterFrameType::from(frame.type_),
        }
    }

    fn interpret_frame_result(
        frame: MaybeUninit<jb_frame>,
        code: jb_return_code,
    ) -> Result<JitterFrame<T>, JitterBufferErr<T>> {
        #[allow(non_upper_case_globals)]
        match code {
            jb_return_code_JB_OK => Ok(Self::interpret_frame(unsafe { frame.assume_init() })),
            jb_return_code_JB_EMPTY => Err(JitterBufferErr::Empty),
            jb_return_code_JB_NOFRAME => Err(JitterBufferErr::NoFrame),
            jb_return_code_JB_INTERP => Err(JitterBufferErr::Interpolate),
            jb_return_code_JB_DROP => Err(JitterBufferErr::Drop {
                frame: Self::interpret_frame(unsafe { frame.assume_init() }),
            }),
            jb_return_code_JB_SCHED => Err(JitterBufferErr::Scheduled),
            _ => unreachable!("Invalid return code {code}"),
        }
    }
}

impl<T> Drop for JitterBuffer<T> {
    fn drop(&mut self) {
        // Drop all frames in the buffer, otherwise we leak the data buffers.
        while let Ok(frame) = self.get_unconditionally() {
            drop(frame);
        }

        unsafe { jb_destroy(self.buf) }
    }
}

impl From<jb_frame_type> for JitterFrameType {
    fn from(value: jb_frame_type) -> Self {
        #[allow(non_upper_case_globals)]
        match value {
            jb_frame_type_JB_TYPE_CONTROL => Self::Control,
            jb_frame_type_JB_TYPE_VOICE => Self::Voice,
            jb_frame_type_JB_TYPE_VIDEO => Self::Video,
            jb_frame_type_JB_TYPE_SILENCE => Self::Silence,
            _ => unreachable!(),
        }
    }
}

impl Into<jb_frame_type> for JitterFrameType {
    fn into(self) -> jb_frame_type {
        match self {
            JitterFrameType::Control => jb_frame_type_JB_TYPE_CONTROL,
            JitterFrameType::Voice => jb_frame_type_JB_TYPE_VOICE,
            JitterFrameType::Video => jb_frame_type_JB_TYPE_VIDEO,
            JitterFrameType::Silence => jb_frame_type_JB_TYPE_SILENCE,
        }
    }
}

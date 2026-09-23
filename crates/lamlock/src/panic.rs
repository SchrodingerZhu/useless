//! Panic transport uses the application's standard panic runtime. The no_std
//! path keeps direct unwinding until a compatible runtime adapter is provided.

use core::panic::UnwindSafe;

#[cfg(feature = "std")]
pub type Payload = std::boxed::Box<dyn core::any::Any + Send + 'static>;
#[cfg(not(feature = "std"))]
pub type Payload = core::convert::Infallible;

#[inline]
pub fn catch<F: FnOnce() -> R + UnwindSafe, R>(f: F) -> Result<R, Payload> {
    #[cfg(feature = "std")]
    {
        std::panic::catch_unwind(f)
    }
    #[cfg(not(feature = "std"))]
    {
        Ok(f())
    }
}

#[cfg(feature = "std")]
#[cold]
pub fn resume(payload: Payload) -> ! {
    std::panic::resume_unwind(payload)
}

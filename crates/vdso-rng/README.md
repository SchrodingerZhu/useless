# vDSO-RNG

A Rust wrapper around the Linux vDSO random number generator.

Unfortunately, [`rustix`](https://github.com/bytecodealliance/rustix/issues/1185) does not currently expose sufficient APIs to access `getrandom` via vDSO.  
This crate provides an alternative, dependency-free wrapper for such functionality, without relying on `libc`.

## Overview

This crate provides two key data structures:

- [`Pool`]: A synchronous, thread-safe state pool for managing shared vDSO random state.
- [`LocalState`]: A thread-local state object rented from the pool, used for generating random bytes.

## Example: Global Setup

To set up the random generator globally, you can use the following pattern:

```rust
use std::cell::RefCell;
use vdso_rng::{Pool, LocalState, Error};

fn global_pool() -> &'static Pool {
    static GLOBAL_STATE: std::sync::LazyLock<Pool> =
        std::sync::LazyLock::new(|| Pool::new().expect("Failed to create global pool"));
    &GLOBAL_STATE
}

fn fill(buf: &mut [u8], flag: u32) -> Result<(), Error> {
    std::thread_local! {
        static LOCAL_STATE: RefCell<LocalState<'static>> =
            RefCell::new(LocalState::new(global_pool()).expect("Failed to create local state"));
    }
    LOCAL_STATE.with(|local_state| {
        let mut state = local_state.borrow_mut();
        state.fill(buf, flag)
    })
}

fn main() {
    std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for _ in 0..16 {
            handles.push(scope.spawn(|| {
                for _ in 0..16 {
                    let mut buf = [0u8; 64];
                    let res = fill(&mut buf, 0);
                    assert!(res.is_ok(), "Failed to fill global state: {:?}", res);
                    assert!(buf.iter().any(|&x| x != 0), "Buffer should not be empty");
                }
            }));
        }
        // Explicit joins wait for thread-local state cleanup.
        for handle in handles {
            handle.join().unwrap();
        }
    });
}
```

## Safety

For safety considerations, including async-signal-safety and fork behavior, please refer to the documentation for:

- [`Pool`]
- [`LocalState`]

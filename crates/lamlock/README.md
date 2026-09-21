# Lamlock

**Ship your critical section, not your data.**

`lamlock` provides an MCS-lock–style flat-combining lock. Instead of running each thread’s critical section independently after acquiring the lock, the head of the waiting queue collects pending tasks and runs them on behalf of other threads.

---

## What is an MCS Lock?

An MCS lock is a queue-based lock designed to be cache-friendly. Each thread spins only on its local node, minimizing cache traffic between threads.  
The thread-local node does not need to live in thread-local storage — it can be stack-allocated. This works because, while the node is alive, the thread is actively blocked in the lock routine, so its stack frame is valid.

```text
 ┌──────┐        ┌────────────────────┐      ┌────────────────────┐
 │ Tail │        │  Thread 1 (Stack)  │      │  Thread 2 (Stack)  │
 └──┬───┘        ├────────────────────┤      ├────────────────────┤
    │            │  ┌──────────────┐  │ next │                    │
    └────────────┼─>│    Node N    │<─┼──┐   │     ..........     │
                 │  └──────────────┘  │  │   │   ┌─────────────┐  │ next
                 │     ..........     │  └───┼───┤  Node N-1   │<─┼─── ....
                 │                    │      │   └─────────────┘  │
                 └────────────────────┘      └────────────────────┘
```

The queue is processed in FIFO order. To submit a task, a thread swaps its local node with the global tail to join the queue, then waits for its predecessor to signal it. When signaled, it runs its task and signals the next node.

---

## What is Flat Combining?

A standard mutex guarantees exclusivity by protecting access with a lock word. Threads poll or park on this lock word until they can acquire it and then run their own critical section.  
Under heavy contention, however, the shared data bounces between thread caches, causing significant overhead.

**Flat combining** reduces this overhead. The idea is that when a thread acquires the lock and runs its critical section, its cache already “owns” the data. Instead of handing off the lock immediately, the thread can execute pending tasks for other threads while still holding the lock, keeping the data hot in its cache. Once done, it signals the next thread.

---

## How does `lamlock` work?

`lamlock` extends the MCS queue to describe jobs to run. Each node contains extra fields — for example, a function pointer to the critical section.  
When a thread reaches the head of the queue, it walks the linked nodes and executes each critical section in turn.

Exclusivity is guaranteed because the combiner thread always holds the global lock. When it finishes, it passes the lock to the next combiner if needed.  
Instead of requiring each thread to signal the next, the combiner handles notifications for waiting threads once their tasks have been executed.

---

## Does it handle panics?

Yes. If a panic occurs during a critical section, the combiner marks the lock as poisoned. All waiting threads are notified.  
With the default `std` feature, the combiner catches unwinding panics and transfers each payload to the thread that submitted the failing task. That thread resumes the panic from `Lock::run()`, without invoking the panic hook again. The hook and the failing closure's destructors run on the combiner thread.

Tasks that already completed keep their results, including the combiner's own task. Tasks that have not started return `LockPoisoned`; their closures are dropped on their requesting threads. Aborting panics cannot be caught.

Poisoning is permanent: subsequent calls to `Lock::run()` return `LockPoisoned` without running the closure.
With exclusive access to the lock, `Lock::get_mut()` still provides access to the data for cleanup. It does not clear poison or repair data left inconsistent by a panic.

Disable default features to use `lamlock` without `std`. In that configuration, unwinding panics propagate on the executing thread, which may be another task's requester. Panic transport for custom `no_std` runtimes is not yet supported.

---

## Is it fast?

It depends. Large, realistic benchmarks are still in progress. A similar flat-combining design in `snmalloc` significantly improved allocator startup time under heavy thread contention.

Early microbenchmarks suggest `lamlock` can match or outperform a standard mutex in some cases:

```text
integer add (lamlock)   time:   [2.1744 ms 2.1947 ms 2.2163 ms]
integer add (mutex)     time:   [2.2152 ms 2.2356 ms 2.2575 ms]

string concat (lamlock) time:   [2.3837 ms 2.4045 ms 2.4263 ms]
string concat (mutex)   time:   [2.3982 ms 2.4184 ms 2.4376 ms]

hashtable (lamlock)     time:   [31.299 ms 31.361 ms 31.422 ms]
hashtable (mutex)       time:   [40.370 ms 40.473 ms 40.587 ms]

initialization (lamlock) time:   [32.982 ms 33.063 ms 33.142 ms]
initialization (mutex)   time:   [42.345 ms 42.418 ms 42.496 ms]
```

Enabling the `nightly` feature (which does node prefetching) and LTO can further improve performance.

---

## Should I use it?

**It depends!** Always benchmark your workload.

Here’s an example where `lamlock` performs poorly:

```rust
fn integer_add_bench_bad() {
    use lamlock::Lock; 
    let lock = Lock::new(0);
    std::thread::scope(|scope| {
        for _ in 0..128 {
            let lock = &lock;
            scope.spawn(move || {
                for i in 0..1000 {
                    lock.run(|data| {
                        *data += i;
                    });
                }
            });
        }
    });
}
```

Results:
```text
integer add bad (lamlock) time:   [8.7588 ms 10.712 ms 12.721 ms]
integer add bad (mutex)   time:   [3.1807 ms 3.2499 ms 3.3180 ms]
```

In this scenario, the critical section is extremely short and each thread repeatedly acquires the lock in a tight loop.  
With an MCS lock, acquisitions can interleave, leading to poor batching. A regular mutex handles this better by letting each thread’s loop run continuously.

---

**Use `lamlock` when:**
- Your critical sections are moderate to heavy.
- You have many threads.
- You can benefit from batching work.

When in doubt: measure!

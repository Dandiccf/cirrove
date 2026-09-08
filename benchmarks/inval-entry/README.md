# `notify_inval_entry` cost

A hard prerequisite for shedding namespace views. Shedding asks the kernel to drop
dentries so that FORGET reclaims the views behind them, and the only cost evidence
Cirrove has is a cgroup experiment where the kernel shed dentries off its own LRU,
taking `d_lock` spinlocks with no FUSE upcall. `fuse_reverse_inval_entry` is a
different operation: it takes the parent's `i_rwsem` exclusively while
`lookup_slow` holds it shared across a whole round trip, against a writer-preferring
rwsem and a single-threaded dispatcher.

    cargo run --release -- <mount-point> [rate/s] [server-delay-ms] [stat-workers]

The bar: a 500,000-file traversal resolves about 750,000 views in roughly 530
seconds, so a ceiling that binds during traversal must shed at about 1,400 per
second. Pass means reaching that with concurrent-lookup p99 under 100 ms. Below
it, shedding is a soft ceiling with a measured overshoot, not a bound -- and on
the giant-directory topology, where entry density is highest, it may not be usable
at all. That is the outcome worth knowing before writing a shed loop, not after.

Run it at 1000, 1400, 4000 and 12000 per second, with server delays of 0, 1 and 50
milliseconds. Record the achieved rate, not only the requested one: the interesting
failure is the rate silently not being reached.

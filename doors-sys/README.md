# doors-sys

Raw FFI bindings for the [illumos][1] [Doors API][2].

This is the bottom of the [`rusty-doors`][3] workspace: the C surface
and nothing else. No safety, no ergonomics, no allocation. If you want
an API that keeps the invariants for you, use [`doors`][4].

## What is here

- Every function from `<door.h>`, plus `fattach`/`fdetach` from
  `<stropts.h>` and the `<thread.h>` and `<pthread.h>` calls the safe
  layer needs.
- Every type and constant from `<sys/door.h>`, laid out to match the C
  ABI exactly and checked by assertions that run at compile time.

Two of those layouts are worth knowing about, because getting them
wrong fails silently: `door_desc_t` and `door_info_t` are wrapped in
`#pragma pack(4)` on amd64, so both are 4-aligned and their 8-byte
members sit at offsets that are not. `door_info_t.di_proc` is at offset
4.

## Two departures from "no wrappers"

- `door_return` returns `Errno` rather than `c_int`, because it only
  ever returns on failure — zero is not a possible result.
- `errno()` exists at all, because illumos reaches errno through a
  per-thread accessor rather than a global.

## Platform

illumos only.

<!-- REFERENCES -->
[1]: https://illumos.org/
[2]: https://illumos.org/man/3C/door_create
[3]: https://github.com/robertdfrench/rusty-doors
[4]: https://crates.io/crates/doors

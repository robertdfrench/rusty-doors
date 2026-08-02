# Experiments

Small C programs that answer questions the man pages and the source do
not settle on their own. They are C rather than Rust on purpose: they
have to be trustworthy *before* the Rust layer exists, and a bug in our
own bindings must not be able to affect the answer.

Build and run them on an illumos host (see `GOALS.md` §8):

```sh
gcc -m64 -Wall -o matrix matrix.c && ./matrix release_emfile
```

All results below were measured on OmniOS `r151058`, amd64,
`SunOS 5.11 omnios-r151058-516f7694c9`.

## `layout.c` — the C ABI

Prints `sizeof`, `_Alignof` and `offsetof` for every doors type. Its
output is the source of the constants asserted in
`doors-sys/src/layout.rs`.

The two results worth stating plainly, because both are easy to get
wrong and neither fails loudly:

- `door_desc_t` is **24 bytes, 4-aligned** — not 16, and not 8-aligned.
  The `d_resv[5]` union arm is what sizes it, and `#pragma pack(4)`
  puts the 8-byte `d_id` at offset 8 rather than a naturally aligned
  slot.
- `door_info_t` is **48 bytes, 4-aligned**, which places the 8-byte
  `di_proc` at **offset 4**.

Reproducing either with a plain `#[repr(C)]` misplaces every field
after the first.

## `matrix.c`, `emfile2.c`, `big.c` — GOALS.md §9.3

The question: **does a failed `door_return` consume the descriptors it
was handed?** Trampoline rule 4.2.4 lets the trampoline re-wrap reply
descriptors as `OwnedFd` and drop them when `door_return` returns. If
the kernel had already closed them, that would be a double close.

Each case sends one descriptor and, if `door_return` comes back, asks
whether the server's own descriptor is still open *and still refers to
the same file* — comparing `st_dev`, `st_ino` and `st_rdev`, so a
recycled descriptor number cannot pass for a survivor.

| case | how it fails | `door_return` | `errno` | fd survived | client saw |
|---|---|---|---|---|---|
| `release_emfile` | client fd table full, `DOOR_RELEASE` | `-1` | **0, unset** | **yes, same file** | success, `desc_num=0` |
| `shared_emfile` | client fd table full, no `DOOR_RELEASE` | `-1` | **0, unset** | **yes, same file** | success, `desc_num=0` |
| `bigdata` | 512 KiB reply under a 1 KiB `DOOR_PARAM_DATA_MAX` | did not return | — | — | success, 512 KiB, remapped |
| `ok` | control, nothing wrong | did not return | — | — | success, `desc_num=1` |

`big.c` walks the reply size up to 256 MiB. Every size succeeded.

### Conclusions

**Rule 4.2.4 holds.** In every failure we could provoke from the safe
API, the descriptor survived and still referred to the same file. The
trampoline may re-wrap and drop reply descriptors after a failed
`door_return`. This was verified more strictly than §9.3 asked for:
file identity, not just `fcntl(fd, F_GETFD) != -1`.

**Three things in the spec did not survive contact with the kernel.**

1. §9.3 step 3 expects `EMFILE`. `door_return` does return `-1`, but
   **leaves `errno` untouched**. We set `errno = 0` immediately before
   the call and read `0` back afterwards, in both `emfile` cases.

2. §9.3's `E2BIG` case cannot be built the way it is written.
   `DOOR_PARAM_DATA_MAX` caps **request** data, not replies, so a reply
   above it is not an error — a 512 KiB reply under a 1 KiB cap went
   through untouched. Nor is `E2BIG` reachable by size: replies up to
   256 MiB succeed, the kernel remapping the client's buffer as
   needed. Reply-size overflow is reported to the *client's*
   `door_call`, never back to the server, which matches the kernel
   source: `door_results()` hands `EOVERFLOW` to `ct->d_error`.

3. §2.1's wrapper contract — "reads errno and returns it, never zero" —
   is not achievable. `errno` really can be zero here, so a
   `NonZeroI32` return has to fall back to something. `doors-sys`
   returns `EINVAL` in that case and says so.

### An illumos bug this turned up

When the kernel cannot deliver a descriptor to the client, it **tells
the client the call succeeded, with `desc_num=0`**, while telling the
server `-1`. The descriptor is silently dropped: the client is never
informed that it was supposed to receive one. Both sides are left with
a plausible-looking result and no error between them.

This is why `Client<NoDescriptors>` closing anything it receives is not
sufficient on its own, and why a caller that *needs* a descriptor must
check the count rather than trust success.

## `xcreate*.c`, `servercreate.c` — why the crate does not use `door_xcreate`

`GOALS.md` §5.2 says the server thread stack size comes from
`door_xcreate(3C)`. It cannot, on OmniOS r151058.

`door_xcreate` takes a per-door thread-creation function. The argument
it hands that function points **into `door_xcreate`'s own stack
frame**. So the obvious implementation — start a thread, return 0 —
races: `door_xcreate` returns, its frame is reused, and the new thread
reads whatever landed there.

It does not fail cleanly. It segfaults inside libc:

```
privdoor_data_hold (5251504f4e4d4c4b) + 8
door_xcreate_startf (fffffc7fffdfa330) + 29
_thrp_setup
_lwp_start
```

`0x5251504f4e4d4c4b` is ASCII `KLMNOPQR` — leftover stack bytes being
used as a pointer.

`xcreate2.c` rules out the attribute mask: every combination crashes,
with and without `DOOR_PRIVATE`. `xcreate3.c` isolates the contract:

| creation function | result |
|---|---|
| start the thread, return immediately | **SIGSEGV inside libc** |
| start the thread, wait until it has copied the argument | `EINVAL` |
| refuse to create a thread (return -1) | `EPIPE` |

Synchronising removes the crash, so the dangling argument really is
the cause — but `door_xcreate` then rejects the door anyway, and there
is no documented handshake that satisfies it. There is no
`door_xcreate(3C)` man page on this system; `door_create.3c` does not
mention it.

**A library call should not crash the caller for using it the obvious
way.** Passing a pointer to a dying stack frame, with no documented
requirement to consume it first, is an illumos bug.

### What the crate does instead

`door_create(3C)` plus `door_server_create(3C)`, the older documented
mechanism. `servercreate.c` confirms it delivers exactly what §5.2
wanted: a server thread with the stack size we chose, read back on the
server thread with `thr_stksegment(3C)`:

```
door_create OK fd=3
door_call OK, reply: stack=262144
we asked for stack=262144
```

The cost is that `door_server_create` installs **one** creation
function per process rather than one per door. The crate keeps a small
table from door cookie to stack size, and the callback looks the cookie
up in the `door_info_t` it is handed. The cookie is used as the key
because it is the only value known before `door_create` returns, and
the first server thread can be created during that call.

## `private_pool.c` — does a `DOOR_PRIVATE` door need `door_bind`?

The question: a door made with `DOOR_PRIVATE` has a pool of server
threads of its own. **Does a thread have to call `door_bind(3C)` to
join that pool, or is parking in `door_return(NULL, 0, NULL, 0)`
enough?**

It matters because the crate used to park without binding. If binding
is required, then every private door in the crate was served by no
threads at all, and the symptom would be a door that answers the calls
already in flight and then stops.

The program makes one `DOOR_PRIVATE` door, starts its server threads
through `door_server_create`, and then makes 20 concurrent
`door_call`s. The only difference between the two runs is one line in
the server thread. A door that cannot be served blocks its callers for
ever, so the whole run carries a 10-second `alarm`.

| mode | server thread does | result |
|---|---|---|
| `nobind` | park only | `rc=142` — killed by the alarm |
| `bind` | `door_bind`, then park | `calls=20 answered=20 returned=20`, `rc=0` |

```sh
gcc -m64 -Wall -o private_pool private_pool.c -lpthread
```

```
mode=nobind  rc=142                            killed by a 10s alarm
mode=bind    calls=20 answered=20 returned=20  rc=0
```

`rc=142` is `128 + 14`, the shell's way of saying `SIGALRM`. The
`nobind` line stops after the mode, because the process never reached
the line that prints the counts.

### Conclusions

**`door_bind` is required.** Without it not one call of the twenty came
back, and not one reached the server procedure. `answered` never rose
above zero, so the calls were not slow — the kernel had no thread to
give them.

A thread that parks without binding joins the **process-wide** pool.
A `DOOR_PRIVATE` door is served *only* by threads bound to it, so it
never sees that thread.

This is what finding 1 in `DOORS-CRATE-FINDINGS.md` rests on, and why
the crate's thread-creation callback now calls `door_bind` before it
parks, for private doors only. Binding on a shared door would be the
opposite mistake: it would take the thread out of the general pool and
starve every other door in the process.

One awkward consequence, which the crate has to work around: for a
`DOOR_PRIVATE` door the creation function can run **during**
`door_create`, before `door_create` has returned the descriptor there
is to bind to. So the new thread has to wait for the builder to publish
it.

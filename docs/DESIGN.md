# Design: full-coverage `doors` crates

Status: **draft for discussion**
Date: 2026-08-01

## 1. What this document is

The `doors` crate today covers a slice of the API: `door_create`,
`door_call`, `door_return`, `door_info`, `door_revoke`, `door_ucred`,
`fattach`, `fdetach`.

This document proposes a rewrite that covers the whole API. It also
proposes types and macros that make the tricky parts of the C interface
correct *by construction*.

The proposals here are grounded in a survey of illumos-gate. Every claim
about "how people really use doors" cites a file and line.

## 2. Answers to the three opening questions

**Q: Should there be a base crate for door things not in `libc`?**
Yes. Split into `doors-sys`. Three crates in total, not four. See
[§7](#7-crate-layout).

**Q: Do proc macros have to be in their own crate?**
Yes, you are right. A crate with `proc-macro = true` can export *only*
proc macros. It cannot export types or functions. So `door-macros` must
stay separate. The repo already has this split.

**Q: Does `#[doors::server_procedure(refuse_desc)]` make sense?**
The idea is right. The exact spelling needs work. See
[§9](#9-the-server-procedure-macro).

## 3. The survey

I read the door consumers in illumos-gate. Numbers below cover
`usr/src/lib` and `usr/src/cmd` only.

| Thing | Count |
| --- | --- |
| Files that call `door_call` | 52 |
| ...of those, files that never call `munmap` at all | 25 |
| `door_create` call sites | ~53 |
| ...that pass `DOOR_REFUSE_DESC` | 40 |
| ...that pass `DOOR_NO_CANCEL` | 33 |

Two things stand out.

1. Almost every real door refuses descriptors and disables
   cancellation. Those should be the **defaults** in Rust, not opt-ins.
2. Half the client files never unmap anything. Most of them are
   gambling that the reply always fits.

## 4. Findings

Each finding is a C convention that is easy to get wrong. Each one has
evidence, then a proposed Rust remedy.

---

### F1. `door_return` sometimes returns, and nobody agrees what to do

`door_return(3C)` says: on success it does not return. On failure it
returns `-1`. Failure is real: `E2BIG` (reply too big for the client),
`EMFILE` (client is out of descriptors), `EFAULT`, `EINVAL`.

The gate has **four different idioms** for this, and two of them are
latent crashes.

**Idiom A — call `door_return` again (correct).**
`cmd/svc/configd/maindoor.c:134`:

```c
(void) door_return((char *)&reply, sizeof (reply), &reply_desc, ...);
(void) door_return(NULL, 0, NULL, 0);
```

The second call parks the thread with no reply. Same in
`cmd/svc/configd/client.c:2437`.

**Idiom B — exit the thread (correct).**
`cmd/zonestat/zonestatd/zonestatd.c:4334`:

```c
(void) door_return(NULL, 0, NULL, 0);
thr_exit(NULL);
```

**Idiom C — fall through (a bug).**
`lib/libc/port/gen/klpdlib.c:68`:

```c
if (argp == DOOR_UNREF_DATA) {
        (void) p->kd_callback(p->kd_user_cookie, NULL, NULL);
        (void) door_return(NULL, 0, NULL, 0);
}

klh = (void *)argp;        /* argp is (void *)1 here! */
ka = KLH_ARG(klh);
```

There is no `return` and no `else`. If `door_return` fails, control
falls into code that dereferences address `1`.

The same shape appears in `cmd/nscd/nscd_frontend.c:941`,
`cmd/ldapcachemgr/cachemgr.c:719`, and `cmd/zoneadmd/zoneadmd.c:1243`.
All three do `if (ptr == NULL) { (void) door_return(NULL,0,0,0); }` and
then keep going with the null pointer.

**Idiom D — plain `return` from the server procedure (undefined).**
`cmd/hotplugd/hotplugd_door.c:175`:

```c
(void) door_return(NULL, 0, NULL, 0);
return;
```

Returning from a server procedure is not defined. The kernel enters it
on a fresh stack with the frame pointer zeroed:

```c
lwp_setsp(ttolwp(curthread), newsp);
lwptoregs(ttolwp(curthread))->r_fp = 0; /* stack ends here */
```
— `uts/intel/os/door_support.c:47`, `:48`

`door_layout()` (`uts/common/fs/doorfs/door_sys.c:1135`) lays out
descriptors, data, `door_info_t` and `door_results` on that stack. It
never plants a return address. There is nothing to return to.

**Remedy.**

- Users never call `door_return`. Only the macro-generated trampoline
  does.
- The trampoline ends in a fixed, correct sequence that cannot fall
  through:

  ```rust
  // 1. deliver the reply; returns only on failure
  door_return(ptr, len, descs, ndescs);
  // 2. park the thread with no reply; returns only on failure
  door_return(null(), 0, null(), 0);
  // 3. both failed. never fall off the end.
  std::process::abort();
  ```

- In `doors-sys`, `door_return` is typed to say what it does:

  ```rust
  /// Returns *only* on failure. On success, control never comes back.
  pub unsafe fn door_return(...) -> Errno;
  ```

  Not `!`, because it does return. Not `c_int`, because `0` is
  impossible. Returning the error directly makes misuse hard.

---

### F2. Everything must be dropped *before* `door_return`

Because `door_return` does not come back, no destructor after it ever
runs. Anything still owned at that point leaks.

C authors know this. Look at what it costs them.

**hotplugd invented a second protocol just to free one buffer.**
`cmd/hotplugd/hotplugd_door.c` keeps a process-wide list of reply
buffers, each tagged with a sequence number:

```c
add_buffer(seqnum, buf);
nvlist_free(results);
(void) door_return(buf, len, NULL, 0);
```

The client then makes a **second door call** whose only job is to say
"you can free that now" (`lib/libhotplug/common/libhotplug.c:1254`):

```c
if ((results != NULL) &&
    (nvlist_lookup_uint64(results, HPD_SEQNUM, &seqnum) == 0)) {
        door_arg.data_ptr = (char *)(uintptr_t)&seqnum;
        door_arg.data_size = sizeof (seqnum);
        ...
        (void) door_call(door_fd, &door_arg);
}
```

Two round trips per request, plus a global mutex, plus a linked list —
all to free a `malloc`. And if the client dies between the two calls,
the buffer leaks anyway.

**Remedy — two nested functions.**

The user writes a normal Rust function. A thin generated wrapper (the
*trampoline*) is the real server procedure. The inner function returns
normally, so everything it owns is dropped. Only the trampoline calls
`door_return`.

```rust
// inner: an ordinary Rust function. Nothing special about it.
fn hello(state: &Greeter, req: &[u8]) -> Vec<u8> {
    let scratch = expensive_thing();     // dropped on return
    let guard = state.lock();            // dropped on return
    build_reply(&scratch)
}

// outer: the real server procedure, generated by the macro
extern "C" fn trampoline(cookie, argp, arg_size, dp, n_desc) {
    let reply = hello(state, req);   // <- everything inside hello drops here
    door_return(reply.as_ptr(), reply.len(), null(), 0);
    // control never gets here on success
}
```

This is the right shape and it solves almost all of the problem. But
**one thing is left over**: `reply` itself.

`reply` is a `Vec`. It is owned by the trampoline. `door_return` never
comes back, so `reply`'s `Drop` never runs. That is one leaked heap
allocation per call, forever.

So the trampoline needs one more step: get the bytes somewhere that does
not need dropping, drop the `Vec`, and only then call `door_return`.

```rust
extern "C" fn trampoline(cookie, argp, arg_size, dp, n_desc) {
    let mut out = ReplyBuf::new();       // no heap ownership

    {   // scope 1: every Drop in the program runs inside here
        let reply = catch_unwind(|| hello(state, req));
        out.fill_from(reply);            // memcpy the bytes out
    }   // <- `reply` (and everything it owned) is dropped here

    // scope 2: nothing with a destructor is alive any more
    door_return(out.as_ptr(), out.len(), out.fds(), out.nfds());
    door_return(null(), 0, null(), 0);
    abort();
}
```

`ReplyBuf` is the thing I called an "arena". A worse name than it
deserved. It is just **a place to put the reply bytes that does not need
to be freed**. Three ways to build it, all fine:

| Where the bytes go | Cost | Limit |
| --- | --- | --- |
| A fixed array in the trampoline's own stack frame | nothing | size fixed at compile time |
| One buffer per server thread, reused every call | one buffer per thread | grows to the largest reply that thread ever sent |
| A buffer the caller passes in (see below) | nothing | caller's choice |

Why the stack works: the kernel copies the reply out while the thread is
still inside the `door_return` syscall. The trampoline's frame is alive
for that whole time. This is exactly what configd does
(`maindoor.c:134` returns `&reply`, a stack local).

**The zero-copy version.** If the `memcpy` bothers you, let the inner
function write into the buffer directly:

```rust
fn hello(state: &Greeter, req: &[u8], out: &mut ReplyBuf) -> Result<(), E> {
    write!(out, "{}, {}!", state.greeting, req_name)?;
    Ok(())
}
```

Now there is no returned `Vec` at all, and nothing to copy. Less
convenient, but the option should exist. The macro can accept either
shape.

**Two other things the nesting does not solve**, which is why the
trampoline is generated rather than hand-written:

- **Panics.** `catch_unwind` has to be in the trampoline (F3), inside
  scope 1, so the panic payload is dropped before `door_return`.
- **Descriptors in the reply.** `OwnedFd` values must be turned into raw
  `int`s before `door_return`, because `DOOR_RELEASE` makes the kernel
  close them. Their `Drop` must not run, and also must not be waiting to
  run. Same scope-1 / scope-2 split.

The point is that hotplugd's whole seqnum protocol exists only because C
has no way to say "drop this before the call that never returns". Rust
does. It is a pair of braces.

---

### F3. Rust panics must not cross the `extern "C"` boundary

A panic that unwinds out of an `extern "C"` function is undefined
behaviour. Today's macro (`macros/src/lib.rs:117`) calls the user block
with no `catch_unwind`.

**Remedy.** The trampoline wraps the call in `catch_unwind`. On panic it
sends a protocol-level error reply, or parks the thread, depending on
the macro's configuration. This must happen inside scope 1 above, so the
panic payload is dropped before `door_return`.

---

### F4. `DOOR_REFUSE_DESC` is a real static guarantee — but only server-side

`door_create(3C)` says of `DOOR_REFUSE_DESC`:

> When this flag is set, the door's server procedure will always be
> invoked with an `n_desc` argument of 0.

The kernel enforces it (`uts/common/fs/doorfs/door_sys.c:489`):

```c
if (da->desc_num > 0 && (dp->door_flags & DOOR_REFUSE_DESC))
        return (ENOTSUP);
```

So a server that sets the flag *cannot* receive descriptors. Yet the
gate still writes defensive checks, and they are weak ones.

`cmd/svc/configd/maindoor.c:74`:

```c
/*
 * No file descriptors allowed
 */
assert(n_desc == 0);
```

That is an `assert`. It compiles out under `NDEBUG`. `client.c:2320`
uses `uu_die` instead, which is better, but it is still a runtime check
for something the kernel already promised.

Your point about the client side is exactly right: the client holds a
plain `int`. Anything in the program can `door_call` on it with
descriptors attached. There is no type that says "this door refuses
descriptors".

**Remedy — typestate on both sides.**

Server side: the macro knows the flag, so it changes the generated
signature. With `refuse_desc`, the `Request` type has **no descriptor
field at all**. The defensive check disappears because the data does not
exist.

Client side: `Client` never exposes its raw fd, and carries a marker
type.

**Descriptors are opt-in** (decision D1). Sending them is the case that
needs extra cleanup, so it is the case you have to ask for.

```rust
pub struct Client<D = NoDescriptors> { fd: OwnedFd, _d: PhantomData<D> }

impl Client<NoDescriptors> {
    /// The common case. Nothing here can send a descriptor.
    pub fn open<P: AsRef<Path>>(path: P) -> io::Result<Self>;

    /// Opt in. Reads door_info(2) and fails if this door has
    /// DOOR_REFUSE_DESC set, so the mistake is caught once, at open
    /// time, instead of ENOTSUP on every call.
    pub fn with_descriptors(self) -> Result<Client<Descriptors>, Error>;
}

impl Client<Descriptors> {
    pub fn call_with_descriptors(&self, data: &[u8], fds: &[SentFd])
        -> Result<Reply, CallError>;
}
```

`call_with_descriptors` exists only on `Client<Descriptors>`. There is no
`AsRawFd` or `IntoRawFd` on `Client`, so no other part of the program can
bypass it.

The reply direction follows the same rule. A `Client<NoDescriptors>`
closes any descriptor the server sends back, right away, and reports it
as a protocol error. It never hands the caller something to clean up.
`Client<Descriptors>` gets them as `ReceivedFd` values.

This is the direct answer to *"nothing stops a client from passing
descriptors to a door server even if they know when the door is opened
that it refuses descriptors."*

---

### F5. The reply buffer: the biggest source of quiet leaks

`door_call(3C)`:

> If the results of a door invocation exceed the size of the buffer
> specified by `rsize`, the system automatically allocates a new buffer
> in the caller's address space and updates the `rbuf` and `rsize`
> members. In this case, the caller is responsible for reclaiming this
> area using `munmap(rbuf, rsize)`.

25 of the 52 gate files that call `door_call` never call `munmap`
anywhere. Three examples:

`lib/libnwam/common/libnwam_util.c:96` aliases the reply onto the
request buffer and never checks:

```c
door_args.rbuf = (void *)request;
door_args.rsize = request_size;
...
if (door_call(*door_fdp, &door_args) == -1)
        return (errno);
return (0);
```

If the server ever replies with more than `request_size` bytes, the
caller leaks a mapping *and* reads the stale request instead of the
reply.

`lib/libvscan/common/libvscan.c:1372` and
`lib/libzonecfg/common/libzonecfg.c:7797` have the same shape.

Even the careful code is awkward.
`lib/libscf/common/lowlevel.c:641`:

```c
if (arg.data_ptr != res && arg.data_size > 0)
        (void) memmove(res, arg.data_ptr, MIN(arg.data_size, res_sz));
if (arg.rbuf != res)
        (void) munmap(arg.rbuf, arg.rsize);
```

Note the two different comparisons — `data_ptr` against `res`, then
`rbuf` against `res`. `data_ptr` points *into* `rbuf`. Getting this
wrong in either direction is either a leak or a wild `munmap`.

**Remedy.**

- One owning type, `Reply`, that always unmaps on `Drop`. The current
  `DoorArgument::OwnedRbuf` is the seed of this; make it the only
  option, not a variant the caller picks.
- `Reply::data()` returns a borrowed slice. The borrow checker ties it
  to the mapping, so a slice can never outlive the `munmap`.
- For the "I never want new mappings" case you raised: a strict mode.

  ```rust
  impl Client<D> {
      /// Reply must fit in `buf`. If the server overflows it, the
      /// overflow mapping is unmapped immediately and this returns
      /// `Err(ReplyTooBig { needed })`. The caller never sees a mapping.
      pub fn call_into<'b>(&self, req: &Request, buf: &'b mut [u8])
          -> Result<&'b [u8], CallError>;
  }
  ```

  We cannot stop the kernel from making the mapping. We *can* guarantee
  it is gone before control returns to the caller, and that the caller
  is told. That is the strongest promise available.

- Sizing help. `door_getparam` lets a client ask the server for its
  limits, so it can size `buf` up front. No gate consumer does this. It
  is cheap to expose.

---

### F6. Descriptor ownership is conditional on the errno

`door_call(3C)`, on `DOOR_RELEASE`:

> The descriptor will be closed in the caller's address space after it
> is passed to the target. **The descriptor will be closed even if
> `door_call()` returns an error, unless that error is `EFAULT` or
> `EBADF`.**

So ownership of a released descriptor transfers on success *and* on most
errors, but comes back on exactly two errors. That is impossible to
remember and impossible to see in C.

`door_return(3C)` adds another case:

> If there is not a client associated with the `door_return()`, the
> calling thread discards the results, **releases any passed descriptors
> with the `DOOR_RELEASE` attribute**, and blocks waiting for the next
> door invocation.

**Remedy.** Make the call signature say it.

```rust
pub enum SentFd {
    /// DOOR_DESCRIPTOR only. Both sides keep access.
    Shared(BorrowedFd<'_>),
    /// DOOR_DESCRIPTOR | DOOR_RELEASE. Ownership moves to the server.
    Released(OwnedFd),
}

pub enum CallError {
    /// Descriptors were *not* consumed. They are handed back.
    Rejected { returned: Vec<SentFd>, errno: Errno },
    /// Descriptors were consumed by the kernel. Nothing to hand back.
    Consumed(Errno),
}
```

`Released` takes an `OwnedFd`, so the compiler tracks the move. On
`EFAULT`/`EBADF` the fds come back inside the error, so they are not
leaked. On every other error they are gone, and the type says so.

---

### F7. `d_id` vs `d_descriptor`: a live bug in libscf

`door_desc_t` has a tagged union (`uts/common/sys/door.h:123`):

```c
typedef struct door_desc {
        door_attr_t     d_attributes;   /* Tag for union */
        union {
                struct {
                        int             d_descriptor;
                        door_id_t       d_id;   /* unique id */
                } d_desc;
                int     d_resv[5];
        } d_data;
} door_desc_t;
```

`lib/libscf/common/lowlevel.c:634`, in `make_door_call`:

```c
if (arg.desc_num > 0) {
        while (arg.desc_num > 0) {
                if (arg.desc_ptr->d_attributes & DOOR_DESCRIPTOR) {
                        int cfd = arg.desc_ptr->d_data.d_desc.d_id;
                        (void) close(cfd);
                }
                ...
```

It reads **`d_id`**, not `d_descriptor`. `d_id` is a `door_id_t`, a
system-wide unique 64-bit number, truncated here to `int`. So this
cleanup path closes a meaningless number. The real descriptor leaks, and
if the truncated id happens to collide with a live fd, it closes an
unrelated file.

The next function down, `make_door_call_retfd`
(`lowlevel.c:715`), gets it right:

```c
int cfd = arg.desc_ptr->d_data.d_desc.d_descriptor;
```

Same file, same author, same loop — one has the bug and one does not.

**Remedy.** Never expose the union. `doors-sys` keeps the raw
`door_desc_t` for FFI, but nothing above it is a union.

```rust
pub struct ReceivedFd {
    fd: OwnedFd,          // from d_descriptor
    door_id: Option<DoorId>, // from d_id, only when this is a door
    attributes: DescAttributes,
}
```

`ReceivedFd` closes its fd on `Drop`. `door_id` is a distinct type with
no `as` conversion to `RawFd`. The confusion is not expressible.

---

### F8. Cookies: pointer versus handle

You proposed treating cookies as objects, "drawn from a pool" so raw
pointers do not leak. The gate independently arrived at both designs,
and the better-engineered daemon picked the pool.

**Pointer cookies.** Common and fragile:

- `lib/libc/port/gen/klpdlib.c:113` — `door_create(cb, p, ...)` where
  `p` is a `malloc`'d struct.
- `lib/varpd/libvarpd/common/libvarpd_door.c:414` — cookie is `vip`.
- `lib/libsysevent/libsysevent.c:1991` — cookie is `shp`.

The teardown path in klpdlib (`klpdlib.c:156`) is:

```c
err = syscall(SYS_privsys, PRIVSYS_KLPD_UNREG, p->kd_doorfd, ...);
if (close(p->kd_doorfd) != 0)
        err = -1;
free(p);
```

Two problems.

1. It uses `close`, not `door_revoke`. `door_revoke(3C)` says revoke
   "performs an implicit call to `close(2)`" and that "door invocations
   that are in progress during a `door_revoke()` invocation are allowed
   to complete normally". A bare `close` does not revoke: other
   processes holding a descriptor to that door can still call it.
2. It `free`s the cookie with no drain. A server thread may be inside
   `klpd_door_callback` right now, holding `p`. That is a
   use-after-free.

**Handle cookies.** `cmd/svc/configd/client.c:2303` — the SMF repository
daemon, which creates one door per client:

```c
uint32_t id = (uint32_t)cookie;
...
cp = client_lookup(id);
...
client_release(cp);
```

The cookie is an **integer id**, not a pointer. `client_lookup` takes a
reference under a lock and can return `NULL` if the client is gone.
`client_release` drops it. When the door goes unreferenced, the handler
calls `client_destroy(id)` (`client.c:2325`). Nothing is dereferenced
without a lookup.

**Remedy.** Do what configd does, and do it only that way.

```rust
/// The cookie is a slot index into a process-global slab, packed with
/// a generation counter. Resolving takes the slab lock, checks the
/// generation, and clones an `Arc<T>` out, all under that one lock, so
/// the state stays alive for the whole invocation even if the door is
/// revoked concurrently. A stale cookie resolves to `None`.
pub(crate) unsafe fn resolve<T>(cookie: *mut c_void) -> Option<Arc<T>>;
```

There is no pointer-cookie alternative, because one cannot be written
soundly. Resolving a leaked `Arc<T>` pointer means turning a raw
pointer back into a `+1` reference count, and that is only valid while
the allocation is still alive — which the pointer cannot tell you.
`Arc` deliberately offers no such operation. The slab gets away with it
only because its lock makes "check that it is alive" and "take a
reference" a single step. An earlier draft of this crate had a `Pinned`
strategy that leaned on `revoke()` draining in-flight calls first; that
is not a substitute for the check, so it was deleted.

Dereferencing a freed cookie is the thing klpdlib gets wrong today, and
one design with a mandatory lookup is what makes it unwritable here.

There is also a third option worth knowing about: a server procedure can
call `door_info(DOOR_QUERY, &info)` to learn which door the current
thread is bound to, and use `di_uniquifier` as the key. That needs no
cookie at all. Useful as a fallback.

---

### F9. `DOOR_UNREF` is a second, invisible entry point

With `DOOR_UNREF`, the server procedure is called with
`argp == DOOR_UNREF_DATA`, which is `(void *)1`
(`uts/common/sys/door.h:77`). `arg_size`, `dp` and `n_desc` are all
zero.

`(void *)1` is not a valid pointer. Today's macro
(`macros/src/lib.rs:101`) does:

```rust
std::slice::from_raw_parts::<u8>(argp as *const u8, arg_size)
```

With `argp == 1` and `arg_size == 0` that happens not to be UB — length
zero, and `u8` needs no alignment. But it silently turns the unref
notification into an ordinary empty request. A server that treats "no
data" as a valid command will run that command when the last client goes
away.

The kernel confirms unref is a genuinely distinct path
(`door_sys.c:2147`):

```c
static door_arg_t unref_args = { DOOR_UNREF_DATA, 0, 0, 0, 0, 0 };
```

and it is exempted from the `DOOR_PARAM_DATA_MIN` check
(`door_sys.c:483`):

```c
if (da->data_size < dp->door_data_min &&
    !(upcall && da->data_ptr == DOOR_UNREF_DATA))
        return (ENOBUFS);
```

Two of the gate's unref handlers are the fall-through bug from F1
(`klpdlib.c:68`, and the equivalent shape in `nscd_frontend.c`).

**Remedy.** Make it a separate function, not a magic argument.

```rust
#[doors::server_procedure(unref)]
impl Greeter {
    fn call(&self, req: Request<'_>) -> Reply { ... }

    /// Generated only when `unref` is set. Cannot be reached by a
    /// normal call, and a normal call can never reach `call` with
    /// DOOR_UNREF_DATA.
    fn on_unreferenced(&self) { ... }
}
```

Without `unref`, no `on_unreferenced` is generated and the trampoline
does not need the check. With `unref`, the trampoline dispatches on
`argp == 1` before it builds any slice.

Also note `DOOR_UNREF` vs `DOOR_UNREF_MULTI`. With `MULTI`, another
reference may have arrived by the time the notification is delivered, so
the handler must re-check `DOOR_IS_UNREF` via `door_info`. That is a
footgun worth encoding: give `on_unreferenced` a parameter that has
already done the re-check.

---

### F10. Cancellation is a Rust safety problem, not a style problem

`door_call(3C)`:

> If the client aborts in the middle of a `door_call()` and the door was
> not created with the `DOOR_NO_CANCEL` flag, the server thread is
> notified using the POSIX thread cancellation mechanism.

The kernel sends `SIGCANCEL` to the server thread
(`door_sys.c:740`):

```c
if (!(dp->door_flags & DOOR_NO_CANCEL)) {
        DOOR_T_HOLD(st);
        ...
        sigtoproc(p, server_thread, SIGCANCEL);
        ...
}
```

In C this runs cleanup handlers. In Rust there are no cleanup handlers.
A cancelled thread does not run `Drop`, does not release locks, and does
not unwind in any way Rust models. Locks stay poisoned or simply stay
held.

libc's own default server threads disable it
(`lib/libc/port/threads/door_calls.c:817`):

```c
static void *
door_create_func(void *arg)
{
        (void) pthread_setcancelstate(PTHREAD_CANCEL_DISABLE, NULL);
        (void) door_return(NULL, 0, NULL, 0);
        ...
```

And 33 of ~53 gate `door_create` sites pass `DOOR_NO_CANCEL` explicitly.

**Remedy.** `DOOR_NO_CANCEL` is **on by default and not removable**
through the safe API. If someone truly needs cancellation, they build
the door through `doors-sys` and accept `unsafe`. Threads we create also
call `pthread_setcancelstate(DISABLE)`, as libc does.

---

### F11. Request size is bounded by the server thread's stack

`door_layout` puts the request data, the descriptors, the `door_info_t`
and the `door_results` struct **on the server thread's stack**
(`door_sys.c:1197`):

```c
if (ssize != 0 && (base_sp - finalsp) > ssize)
        return (E2BIG);         /* doesn't fit in stack */
```

That is the `E2BIG` in `door_call(3C)`: "Arguments were too big for
server thread stack."

So thread stack size, `DOOR_PARAM_DATA_MAX` and `DOOR_PARAM_DESC_MAX`
are one coupled decision. `door_server_create(3C)` says as much:

> The overall amount of data and argument descriptors that can be sent
> through a door is limited by both the server thread's stack size and
> by the parameters of the door itself.

Only three gate programs call `door_setparam` at all: configd, syslogd,
and smserverd. Everyone else leaves `DATA_MAX` at `SIZE_MAX` and hopes.

**Remedy.** Make the limits part of building a door, with a checked
relationship.

```rust
let door = Door::builder(Greeter::new())
    .request_size(0..=64 * 1024)   // sets DATA_MIN and DATA_MAX
    .max_descriptors(0)            // sets DESC_MAX
    .thread_stack_size(256 * 1024) // via door_xcreate
    .build()?;
```

`build()` rejects a stack too small for the declared request size.

---

### F12. Sentinels and error paths

Small things, but they are the ones a type system removes for free.

`lib/varpd/libvarpd/common/libvarpd_door.c` uses `-1` as the "no door"
sentinel at creation (`:409`, `:416`) but `0` at teardown (`:462`):

```c
void
libvarpd_door_server_destroy(varpd_handle_t *vhp)
{
        ...
        if (vip->vdi_doorfd != 0) {
                if (door_revoke(vip->vdi_doorfd) != 0)
                        libvarpd_panic("failed to revoke door: %d", errno);
```

`vdi_doorfd` is initialised to `-1` (`libvarpd.c:115`), and stays `-1`
if `door_create` fails. So calling `destroy` after a failed `create`
calls `door_revoke(-1)`, which fails, which **panics the library**.

Also in the same function (`:421`):

```c
if ((fd = open(path, O_CREAT | O_RDWR, 0666)) == -1) {
        ret = errno;
        if (door_revoke(vip->vdi_doorfd) != 0)
                libvarpd_panic(...);
        mutex_exit(&vip->vdi_lock);
        return (errno);        /* not `ret` */
}
```

`ret` is saved and then not used. It returns `errno`, which
`door_revoke` and `mutex_exit` may have overwritten.

**Remedy.** `Option<Door>` instead of a sentinel int. `Result<T, E>`
instead of `errno`. These cost nothing to get right in Rust.

---

### F13. `fork(2)` breaks a door server, and RAII makes it worse

A door belongs to the process that created it. After `fork`, the child
holds an inherited descriptor to a door it does not own.

What the interfaces say:

- `door_revoke(3C)` fails with `EPERM` if "the door descriptor was not
  created by this process". So a child cannot revoke the parent's door.
- `door_bind(3C)`: "If a process containing threads that have been bound
  to a door calls `fork(2)`, the threads in the child process will be
  bound to an invalid door, and any calls to `door_return(3C)` will
  result in an error."
- The kernel implements that by flagging the child's threads
  (`uts/common/fs/doorfs/door_sys.c:2136`):

  ```c
  if (pt != NULL && (st->d_pool != NULL || st->d_invbound)) {
          /* parent thread is bound to a door */
          dt = child->t_door = kmem_zalloc(sizeof (door_data_t), KM_SLEEP);
          DOOR_SERVER(dt)->d_invbound = 1;
  }
  ```

- `door_call(3C)` returns `EINTR` if "a signal was caught in the client,
  the client called `fork(2)`, or the server exited during invocation".

Plain `fork(2)` clones only the calling thread, so the child has no
server threads at all. `forkall(2)` clones them, but the kernel marks
them invalid, so the child still cannot serve. **A door server does not
survive `fork` either way.**

libc works around this in `door_create` by tracking pids across a
critical section (`lib/libc/port/threads/door_calls.c:177`), and its
unref thread checks the pid on every loop (`door_calls.c:140`):

```c
while (getpid() == mypid && __door_unref() && errno == EINTR)
        continue;
```

**A forked child exiting is safe for the door itself.** The kernel's
`door_exit` (`door_sys.c:2097`) walks `p->p_door_list`, the doors *this
process created*. That list is not copied by `fork` — nothing in
`uts/common/os/fork.c` touches `p_door_list`. So the child's list is
empty and its exit revokes nothing of the parent's.

**But the child must still close the inherited descriptors.** Two
reasons, and only the second is obvious.

1. **It changes `DOOR_UNREF` in the parent.** `door_create(3C)` delivers
   the unref invocation when "the number of descriptors that refer to
   this door drops to one". The child holds an extra descriptor for
   every door. The kernel's test is in `door_vnops.c:126`:

   ```c
   if (count == 2 && vp->v_count == 1 &&
       (dp->door_flags & (DOOR_UNREF | DOOR_UNREF_MULTI))) {
   ```

   A child that keeps the fd delays the parent's unref notification
   until the child exits.

2. **Door descriptors survive `exec`.** They are ordinary file
   descriptors. libc already guards its own client doors against this —
   `getxby_door.c:280`, `lowlevel.c:1279`, `call_labeld.c:152` and
   `ns_cache_door.c:137` all do
   `fcntl(doorfd, F_SETFD, FD_CLOEXEC)`.

**What C does about it: `closefrom`.** nscd forks to run a per-user
server at lower privilege — exactly the "fork and drop privileges" case.
Its child (`cmd/nscd/nscd_selfcred.c:824`) is:

```c
if ((cid = fork1()) == 0) {
        _whoami = NSCD_CHILD;
        ...
        /* close all except the log file */
        ...
        closefrom(0);

        (void) setgid(set2gid);
        (void) setuid(set2uid);

        /* set up the door and server thread pool */
        if ((_doorfd = _nscd_setup_child_server(_doorfd)) == -1)
```

And `_nscd_setup_child_server` (`nscd_frontend.c:1288`) re-establishes
the thread pool with `door_server_create` and creates a **brand new**
door. It never tries to reuse the parent's.

That is the whole design, and it validates the assumption in this
section: a forked child never wants to serve its parent's doors. It
either wants no doors, or its own.

**Why this is worse in Rust than in C.**

C leaks. The current crate leaks too — `Door::install`
(`doors/src/server.rs:94`) creates the jamb file and never removes it.

Any RAII fix for that leak creates a new hazard. Consider a server that
forks a helper:

1. Parent creates a `Door` attached at `/var/run/foo.door`.
2. Parent calls `fork`.
3. The child inherits the value.
4. The child exits, or just drops it.
5. `Drop` runs `fdetach` and `unlink` on `/var/run/foo.door`.

The child has just torn down the parent's live service. `door_revoke`
would have failed with `EPERM` and done no harm, but `fdetach` and
`unlink` have no such protection — the child has the same credentials,
so they succeed.

C never had this bug because C never had the destructor.

**Remedy — a fork handler, not a pid check.**

An earlier draft proposed a separate `Jamb` guard plus a pid stamp on
both it and `Door`. Both parts were wrong.

*Drop `Jamb`.* Two guards means two values a child can wrongly drop, and
two lifetimes to keep in step. Fold the path into `Door`:

```rust
impl<S> Door<S> {
    /// Records the path. Teardown happens in Door's own Drop.
    pub fn attach<P: AsRef<Path>>(&mut self, path: P) -> Result<(), Error>;
    pub fn detach<P: AsRef<Path>>(&mut self, path: P) -> Result<(), Error>;
}
```

One object, one lifetime, one destructor.

*Register a `pthread_atfork` handler.* A pid check makes `Drop` a no-op
in the child, but it cannot close the inherited descriptors, so it does
not fix `DOOR_UNREF`. A fork handler fixes both, and it fires even when
the fork comes from code we do not control — `libc::fork`, a dependency,
anything.

The classic objection is that a child handler may only call
async-signal-safe functions. That is satisfiable here:

```
prepare (runs in the parent, still multithreaded):  lock the registry
parent:                                             unlock
child:  for each registered door:
            inner.disowned.store(true, Release)     // atomic store: safe
            close(inner.fd)                         // close(2): safe
        unlock
```

Taking the lock in `prepare` is what makes the child handler legal. The
child inherits the registry already locked and consistent, so it never
has to acquire anything. No allocation, no `malloc` lock, no deadlock.
This is the standard `pthread_atfork` pattern.

Then:

```rust
impl<S> Drop for Door<S> {
    fn drop(&mut self) {
        if self.inner.disowned.load(Ordering::Acquire) {
            return;   // forked child. fd already closed, path not ours.
        }
        let _ = door_revoke(self.inner.fd);
        for p in &self.inner.paths {
            let _ = fdetach(p);
            let _ = std::fs::remove_file(p);
        }
    }
}
```

An atomic load, not a `getpid` syscall. Keep a `getpid` comparison as a
cheap backstop for `vfork` and `forkall`, which do not run atfork
handlers the same way.

The **explicit** forms behave differently from `Drop`: `Door::revoke()`
and `Door::detach()` return `Err(Error::Disowned)` in a child. Silence
is right for a destructor; an error is right for a call the user wrote
on purpose.

**Three layers, because each catches a different fork.**

| Layer | Catches |
| --- | --- |
| `FD_CLOEXEC` by default | every *implicit* fork, because they all `exec` immediately — `std::process::Command` uses `posix_spawn` or `fork`+`exec` |
| `doors::fork()` | the deliberate bare `fork()`, written by the user |
| `pthread_atfork` backstop | a bare `libc::fork()` from code that did not use `doors::fork()` |

The first two cover everything a normal program does. The handler is a
backstop, not the main mechanism.

Caveat, and the reason it is only a backstop: POSIX has no way to
*unregister* an atfork handler. If `doors` ends up in a shared object
that gets `dlclose`d, the handler dangles. Register it once, lazily, on
the first `Door::build`. Document the `cdylib` case in the crate docs;
do not design around it and do not add a feature flag to switch the
handler off. Doors are built into applications, and a `dlclose`d door
server is an odd thing to want.

There is no Sun-threads alternative here: `thr_atfork` does not exist.
See F14.

**Servers and clients get opposite treatment.**

| | on `fork` | on `exec` |
| --- | --- | --- |
| Server `Door` | disowned and closed by the fork handler | n/a, already gone |
| `Client` | kept — a child may legitimately keep calling | closed, via `FD_CLOEXEC` by default |

`Client` is a descriptor to somebody *else's* door. Case (a), "programs
that place door calls as part of normal business", wants the child to
keep it. So the fork handler leaves clients alone. `FD_CLOEXEC` is still
the default, matching what libc does for its own client doors, with an
opt-out for the rare program that wants to hand a door through `exec`.

**What the child should then do** — the nscd recipe, in Rust:

```rust
match unsafe { doors::fork()? } {
    ForkResult::Child => {
        // every server Door is already closed and disowned.
        // Clients are still usable.
        drop_privileges()?;
        let mut door = Door::builder(ChildState::new()).build()?;
        door.attach("/var/run/child.door")?;      // its own door
        ...
    }
    ForkResult::Parent { child } => { ... }
}
```

`doors::fork()` is a convenience, not a requirement. The atfork handler
runs whichever way the process forked. That is the main reason to prefer
it over an "call this first thing in the child" helper.

- **Do not retry `EINTR` by default.** This is the part most gate
  consumers get wrong. `libscf` (`lowlevel.c:622`), `libdlbridge`, and
  `libvarpd` all loop on `EINTR`:

  ```c
  while ((r = door_call(h->rh_doorfd, &arg)) < 0) {
          if (errno != EINTR)
                  break;
  }
  ```

  But `EINTR` does not mean "nothing happened". The server may have
  already run the procedure and only the reply was lost. Retrying
  re-executes it. The man page says as much: "`door_call()` is not a
  restartable system call [...] If the door invocation is not idempotent
  the caller should mask any signals."

  So: `CallError::Interrupted` is its own variant that the caller must
  handle. For idempotent protocols, offer an explicit opt-in whose name
  states the requirement:

  ```rust
  /// Retries on EINTR. Only correct if the server procedure is
  /// idempotent: an interrupted call may already have run.
  pub fn call_idempotent(&self, data: &[u8]) -> Result<Reply, CallError>;
  ```

- Document plainly that `Door` does not survive `fork`. If a program
  needs a door in a child, the child must create its own.

---

### F14. Sun threads vs POSIX threads: the same threads

Portability is not a goal here, so it is fair to ask whether to build on
`<thread.h>` (`thr_create`, `thr_keycreate`, `thr_stksegment`) instead of
`<pthread.h>`.

**On illumos they are one implementation, not two.**

- `thread_t` is `unsigned int` (`head/thread.h:51`). `pthread_t` is
  `uint_t`, and the header says so outright
  (`uts/common/sys/types.h:430`):

  ```c
  typedef	uint_t	pthread_t;	/* = thread_t in thread.h */
  ```

- Both creation calls funnel into the same function. `thr_create`
  (`thr.c:727`) and `pthread_create` (`pthread.c:107`) each call
  `_thrp_create` (`thr.c:561`).

- The flags are the same numbers (`head/thread.h:107`):

  ```c
  #define	THR_BOUND	0x00000001	/* = PTHREAD_SCOPE_SYSTEM */
  #define	THR_DETACHED	0x00000040	/* = PTHREAD_CREATE_DETACHED */
  ```

So switching does not buy a different runtime, different `fork`
behaviour, or a different scheduler.

**`door_server_create(3C)`'s threading advice is now vacuous.** It says:

> The specified server creation function should create user level
> threads using `thr_create()` with the `THR_BOUND` flag, or in the case
> of POSIX threads, `pthread_create()` with the `PTHREAD_SCOPE_SYSTEM`
> attribute.

That mattered when illumos had an M:N thread model. It does not now.
`_thrp_create` records `THR_BOUND` in `ul_usropts` (`thr.c:635`) and
never branches on it. Every thread is a bound LWP.

This matters for us: **Rust's `std::thread` creates threads with default
scope, and they are perfectly good door server threads.** Reading the
man page would suggest otherwise.

**Three things Sun threads still have that pthreads does not.**

1. **`THR_DAEMON`.** No POSIX equivalent. `thr_create(3C)`:

   > Daemon threads do not interfere with the exit conditions for a
   > process. A process will terminate when the last non-daemon thread
   > exits or the process calls `exit(2)`.

   `_thrp_create` turns it into `LWP_DAEMON` in the kernel
   (`thr.c:607`). libc uses it for the door unref-delivery thread
   (`door_calls.c:209`), precisely so an unref door does not keep the
   process alive. Any background thread the crate owns wants this.

2. **`thr_stksegment()`.** Returns the calling thread's stack base and
   size. libc's `door_return` uses it to compute the stack reservation
   (`door_calls.c:365`). We need the same information to check a door's
   `DOOR_PARAM_DATA_MAX` against the actual server thread stack (F11).
   There is no clean POSIX way to ask this about *yourself*.

3. **Fewer moving parts.** `thr_create` takes stack address, stack size
   and flags as arguments. No `pthread_attr_t` to build and destroy.

**And one thing it does not have: a fork hook.** There is no
`thr_atfork`. `pthread_atfork` is the only registration API in illumos.
So moving to Sun threads removes the fork handler option rather than
replacing it.

**Decision.** Mixed, split along the obvious line.

| Job | API | Why |
| --- | --- | --- |
| Door server threads | `std::thread::Builder` | `stack_size` (needed for F11), guard page, Rust TLS, normal Rust panics. Scope is a non-issue. |
| Any background thread the crate owns | `thr_create` with `THR_DAEMON` | pthreads cannot mark a thread as a daemon |
| Asking about the current stack | `thr_stksegment` | no POSIX equivalent |
| Fork handler | `pthread_atfork` | nothing else exists |

The cost of `thr_create` threads is that they are not Rust std threads.
On illumos that is mild — `std::thread::current()` works on foreign
threads and `thread_local!` uses ELF TLS — but we would hand-roll the
stack guard and panic boundary that `std::thread::Builder` gives free.
So use it only where it earns its place.

---

### F15. Descriptors on the `door_return` failure path — resolved

This was an open question. The man page covers only the "no client"
case. Reading the kernel settles it.

**`door_return` returns to userland in exactly three places.**

1. `st->d_invbound` → `EINVAL` (`door_sys.c:1392`), before anything is
   examined.
2. No caller, and `door_release_fds` fails to copy in the descriptor
   array → `EFAULT` (`door_sys.c:2974`).
3. `door_results()` returns non-zero and the error is **not**
   `EOVERFLOW` (`door_sys.c:1442`).

`EOVERFLOW` never causes a return. It is stored in `ct->d_error`, handed
to the *client's* `door_call`, and the server thread parks normally.
Worth knowing: from the server's side `EOVERFLOW` is not a failure at
all.

**Where each error leaves the descriptors.**

| Error | Site | Descriptors |
| --- | --- | --- |
| `EINVAL` invalid bound door | `door_sys.c:1392` | untouched |
| `EFAULT` no caller, desc copyin | `door_sys.c:2974` | untouched |
| `E2BIG` client expects no results | `door_sys.c:2754` | untouched |
| `E2BIG` `desc_num > door_max_desc` | `door_sys.c:2757` | untouched |
| `EMFILE` upcall descriptor limit | `door_sys.c:2772` | untouched |
| `E2BIG` upcall data / reply limits | `door_sys.c:2775`, `:2787` | untouched |
| `EFAULT` data copyin | `door_sys.c:2795`, `:2813` | untouched |
| `EMFILE` client fd table full | `door_sys.c:2854` | untouched |
| `EMFILE` client fd table full, overflow path | `door_sys.c:2333` | untouched |
| `EFAULT` descriptor array copyin | `door_sys.c:2858` | untouched |
| **`EINVAL` bad entry in the descriptor loop** | `door_sys.c:2877` | **partial** |

The `ufcanalloc` check is the important one. It is the `EMFILE` case
that actually happens in practice — the client's descriptor table is
full — and it runs *before* a single descriptor is touched:

```c
/* First, check if we would overflow client */
if (!ufcanalloc(ttoproc(caller), desc_num))
        return (EMFILE);
```

The one partial case is a malformed descriptor array. The loop
translates entries one at a time, closing `DOOR_RELEASE` ones as it
goes, and on a bad entry unwinds asymmetrically:

```c
if (!(didpp->d_attributes & DOOR_DESCRIPTOR) ||
    (fp = getf(fd)) == NULL) {
        /* close translated references */
        door_fp_close(ct->d_fpp, fpp - ct->d_fpp);
        /* close untranslated references */
        door_fd_rele(didpp, desc_num - i, 0);
        kmem_free(start, dsize);
        return (EINVAL);
}
```

Entries before `i` have already had `closeandsetf` applied if they were
`DOOR_RELEASE`. Entries from `i` on are released by `door_fd_rele`. C
callers cannot tell which of their descriptors survived.

**But that case is unreachable from the safe API.** It needs either a
missing `DOOR_DESCRIPTOR` attribute or an invalid fd. `SentFd` is built
only from `OwnedFd` or `BorrowedFd`, and always sets
`DOOR_DESCRIPTOR`. Both preconditions are types.

**Decision.** On every `door_return` failure the safe API can reach, no
descriptor has been consumed. So the trampoline can hold plain `int`s
across `door_return` (it must — F2 forbids live destructors there), and
if `door_return` returns, re-wrap them as `OwnedFd` and drop them. No
double close, and no leak.

```rust
// scope 2: raw ints only, no destructors alive
let e = door_return(out.as_ptr(), out.len(), out.fds(), out.nfds());
// only here on failure, and nothing was consumed
for fd in out.take_raw_fds() {
    drop(unsafe { OwnedFd::from_raw_fd(fd) });   // safe: not consumed
}
door_return(null(), 0, null(), 0);
abort();
```

**Confirm it rather than trust the reading.** Line-reading is fallible,
so this belongs in the illumos CI suite:

- Client sets `RLIMIT_NOFILE` low and fills its descriptor table.
- Client calls a door whose server returns one `DOOR_RELEASE`
  descriptor.
- `door_return` should fail with `EMFILE`.
- The server then checks `fcntl(fd, F_GETFD) != -1` on the descriptor
  it tried to send.

If the descriptor is still open, non-consumption is confirmed on the
one error path that occurs in practice. Make the same assertion for
`E2BIG` by having the client pass a door with `DOOR_PARAM_DATA_MAX` set
below the reply size.

---

## 5. Summary table

| # | C convention | Rust remedy |
| --- | --- | --- |
| F1 | `door_return` may return | Users never call it; trampoline ends in a non-falling-through sequence |
| F2 | Nothing after `door_return` is dropped | Inner fn returns normally; trampoline copies into a `ReplyBuf`, drops, then returns |
| F3 | Panics cross `extern "C"` | `catch_unwind` inside the trampoline |
| F4 | `DOOR_REFUSE_DESC` unenforceable at the client | `Client` refuses descriptors by default; no `Request` descriptor field on the server |
| F5 | `rbuf` overflow leaks a mapping | Owning `Reply` type; `call_into` strict mode; `door_getparam` sizing |
| F6 | `DOOR_RELEASE` ownership depends on errno | `OwnedFd` in, fds handed back inside `CallError::Rejected` |
| F7 | `d_id` / `d_descriptor` union confusion | No union above `doors-sys`; distinct `DoorId` type |
| F8 | Pointer cookies, freed without a drain | One design only: a slab index with a locked lookup that clones an `Arc`. No pointer cookie exists to free |
| F9 | `DOOR_UNREF_DATA` is a magic pointer | Separate generated `on_unreferenced` method |
| F10 | Cancellation kills a thread with no `Drop` | `DOOR_NO_CANCEL` always on in the safe API |
| F11 | Request size bounded by thread stack | Builder ties stack size to `DATA_MAX` / `DESC_MAX` |
| F12 | Sentinel fds and clobbered `errno` | `Option<Door>` and `Result` |
| F13 | `fork` leaves a dead door, and `Drop` then wrecks the parent's | `FD_CLOEXEC` + `doors::fork()` + a `pthread_atfork` backstop; `EINTR` surfaced, never retried silently |
| F14 | `door_server_create(3C)` demands `THR_BOUND` threads | Vacuous on illumos; `std::thread` is fine. Use `thr_*` only for `THR_DAEMON` and `thr_stksegment` |
| F15 | Descriptor fate on `door_return` failure is undocumented | Resolved by reading the kernel: nothing is consumed on any reachable error. Re-wrap and drop |

## 6. API coverage gap

Present in `doors/src/illumos/door_h.rs` today:
`door_create`, `door_call`, `door_return`, `door_info`, `door_revoke`,
`door_ucred`, plus `fattach` and `fdetach`.

Missing, and needed for full coverage:

| Function | Why it matters |
| --- | --- |
| `door_bind` / `door_unbind` | Required for `DOOR_PRIVATE`. Used by picld and smserverd. |
| `door_getparam` / `door_setparam` | The only way to bound request size (F11). |
| `door_server_create` | Control thread creation for the whole process. |
| `door_xcreate` | Per-door thread pool, thread count, stack size. Needed for F11. |
| `door_cred` | Cheaper than `door_ucred` when you only need uid/gid/pid. |

Also worth wrapping properly: `DOOR_QUERY` (`door_info(-2, ...)`) to ask
which door the current thread serves.

## 7. Crate layout

**Three crates.** An earlier draft proposed a fourth, `doors-macros-core`,
to hold the `syn`/`quote` logic so it could be unit tested. I checked,
and it is not needed. See "Why not a fourth crate" below.

### `doors-sys`

Raw FFI only. The Rust spelling of `<door.h>` and the door bits of
`<stropts.h>`.

- Every function, struct, constant, and type alias.
- No safety, no ergonomics, no allocation.
- Every item `unsafe` where the C item is.
- Depends on `libc` only.
- Roughly today's `doors/src/illumos/`, completed.

### `door-macros`

`proc-macro = true`. Holds all the macro logic.

Nothing else can live here. That answers your question: yes, the split
is forced. Verified with a scratch crate:

```
error: `proc-macro` crate types currently cannot export any items other
       than functions tagged with `#[proc_macro]`, `#[proc_macro_derive]`,
       or `#[proc_macro_attribute]`
```

### `doors`

The safe API. Depends on `doors-sys` and `door-macros`, and re-exports
the macros so users write `#[doors::server_procedure]`.

```
doors-sys
     ^
     |
   doors  <---------  door-macros
```

### Why not a fourth crate

The usual reason to add a `-core` crate is that you cannot unit test a
proc-macro crate. That turns out to be false, as long as you follow one
rule:

**Write the logic against `proc_macro2::TokenStream`. Keep the
`#[proc_macro_attribute]` function to three lines.**

`proc_macro::TokenStream` only works while the compiler is running, so
anything using it is untestable. `proc_macro2::TokenStream` works
standalone. So:

```rust
// all the real work, testable
fn expand(item: proc_macro2::TokenStream) -> proc_macro2::TokenStream { ... }

// the only thing that touches `proc_macro`
#[proc_macro_attribute]
pub fn server_procedure(
    attr: proc_macro::TokenStream,
    item: proc_macro::TokenStream,
) -> proc_macro::TokenStream {
    expand(item.into()).into()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn expansion_works_outside_the_compiler() {
        let out = expand(quote! { fn hello() { let x = 1; } });
        assert_eq!(out.to_string(), "extern \"C\" fn hello () { }");
    }
}
```

I built and ran exactly this. `cargo test` passes inside the proc-macro
crate.

A `-core` crate would only earn its place if something other than the
macro needed the same logic — a build script, or a standalone code
generator. We have no such need. Skip it.

Note: the generated code refers to items in `doors`, so a user of the
macro must have `doors` in scope. That is already true today
(`macros/src/lib.rs:94` emits `doors::illumos::door_h::door_desc_t`).
Better: generate references to a hidden `doors::__private` module, so
the public API can change without breaking old expansions.

### Testing

Doors only exist on illumos. That is presumably why the integration
tests were removed in `aa45f10`.

Suggestion:

- `doors-sys` and `doors` compile on any platform but gate the `unsafe`
  bodies behind `#[cfg(target_os = "illumos")]`, so `cargo check` works
  on the Mac.
- Macro expansion tests (`trybuild`) run anywhere. They are the bulk of
  the interesting logic.
- Real behaviour tests run in CI on illumos only. The F15 descriptor
  experiment belongs here.

## 8. Core types

Sketches, not final.

```rust
// ---- server ----

pub struct Door<S> {
    /* fd + cookie registration + drain counter + attached paths */
    /// Set by the pthread_atfork child handler. See F13.
    disowned: AtomicBool,
}

impl<S> Door<S> {
    pub fn builder(state: S) -> DoorBuilder<S>;

    /// Revoke, wait for in-flight calls to finish, then drop the state.
    /// Err(Disowned) if called from a forked child.
    pub fn revoke(self) -> Result<S, RevokeError>;

    pub fn info(&self) -> Result<DoorInfo, Error>;

    /// Records the path. Teardown happens in Door's own Drop, which
    /// runs door_revoke, fdetach and unlink. Today `Door::install`
    /// leaves the file behind forever.
    ///
    /// There is no separate `Jamb` guard: one object, one lifetime,
    /// one destructor. See F13 for why two would be worse.
    pub fn attach<P: AsRef<Path>>(&mut self, path: P) -> Result<(), Error>;
    pub fn detach<P: AsRef<Path>>(&mut self, path: P) -> Result<(), Error>;
}

// ---- request / reply, server side ----

/// With `refuse_desc`, the `descriptors` field does not exist.
pub struct Request<'a> {
    pub data: &'a [u8],
    pub descriptors: &'a [ReceivedFd],   // absent under refuse_desc
}

impl Request<'_> {
    /// Only callable during an invocation, because the borrow of
    /// `Request` proves we are inside one.
    pub fn peer(&self) -> Result<UCred<'_>, Error>;
}

// ---- client ----

pub struct Client<D = NoDescriptors> { fd: OwnedFd, _d: PhantomData<D> }

impl<D> Client<D> {
    pub fn info(&self) -> Result<DoorInfo, Error>;
    pub fn limits(&self) -> Result<DoorParams, Error>;   // door_getparam

    pub fn call(&self, data: &[u8]) -> Result<Reply, CallError>;
    pub fn call_into<'b>(&self, data: &[u8], buf: &'b mut [u8])
        -> Result<&'b [u8], CallError>;

    /// Retries on EINTR. Only correct if the server procedure is
    /// idempotent: an interrupted call may already have run. See F13.
    pub fn call_idempotent(&self, data: &[u8]) -> Result<Reply, CallError>;
}

/// Owns any mapping the kernel made. Unmaps on Drop, always.
pub struct Reply { /* ... */ }
impl Reply {
    pub fn data(&self) -> &[u8];
    pub fn take_descriptors(&mut self) -> Vec<ReceivedFd>;
}
```

Two notes on `UCred`.

The current `UCred::new()` (`doors/src/illumos/mod.rs:416`) is callable
from anywhere. `door_ucred` is only meaningful inside an invocation on
the current thread; elsewhere it fails with `EINVAL`. Tying it to
`&Request` makes that a compile-time fact.

`door_ucred(3C)` also has a reuse contract: pass a non-null pointer and
it reuses the allocation. The current `renew` does this correctly. Keep
it, but express it as `&mut self` rather than by-value.

## 9. The server procedure macro

Your sketch:

```rust
#[doors::server_procedure(refuse_desc)]
pub fn hello(&self, Vec<u8>) -> Result<Vec<u8>> {
    ...
}
```

Three problems with the exact spelling.

1. `&self` on a free function is not valid Rust. `self` needs an `impl`
   block.
2. `Vec<u8>` with no name is not valid. Parameters need patterns.
3. Taking `Vec<u8>` means a copy on entry. The request data is already
   a live slice on the server thread's stack. A `&[u8]` is free.

Also, returning `Result<Vec<u8>>` is fine under F2's `ReplyBuf`, but the
error type needs a rule: it must be encodable into bytes, or the macro
needs to know how to turn it into a reply.

### Proposal: one attribute on an `impl` block

```rust
struct Greeter { greeting: String }

#[doors::server]
impl Greeter {
    /// Attributes are per-door, so they go on the entry point.
    #[door(refuse_desc, unref, request_size = ..=8192)]
    fn hello(&self, req: &[u8]) -> Result<Vec<u8>, MyError> {
        Ok(format!("{}, {}!", self.greeting,
                   std::str::from_utf8(req)?).into_bytes())
    }

    fn on_unreferenced(&self) {
        // generated hook, present because `unref` was set
    }
}

let mut door = Door::builder(Greeter { greeting: "Hello".into() })
    .build_hello()?;          // generated, named after the method
door.attach("/var/run/greeter.door")?;
```

`&self` comes from the cookie (F8). `refuse_desc` removes the descriptor
parameter (F4). `unref` adds `on_unreferenced` (F9). `request_size` sets
`door_setparam` (F11). `DOOR_NO_CANCEL` is always on (F10).

### Why one macro with options, not several macros

You asked about "different proc macros for different kinds of server
procedures". I think that is the wrong axis. The flags **compose**:

```
refuse_desc + unref + private-pool + request_size
```

Rust attribute macros do not compose well. Two attribute macros on one
function means the outer one sees the inner one's expansion, which gets
ugly fast. One macro that reads a list of options handles every
combination.

But there is a second axis where separate macros *do* make sense: the
**shape of the function**. That is a real difference, not a flag.

| Macro | Signature | For |
| --- | --- | --- |
| `#[server_procedure]` | `fn(Request) -> Reply` | Full control. Byte level. |
| `#[server_rpc]` | `fn(Req) -> Result<Resp, E>` | Serde or similar on both sides. |
| `#[server_raw]` | the C signature | Escape hatch. Only checks the signature. |

Each of those takes the same option list. That gives you "different
kinds of server procedure" without the composition problem. A zero-copy
shape is also worth supporting, where the function writes into a
`&mut ReplyBuf` and returns nothing.

## 10. Decisions and open questions

### Decided

**D1. `Client` refuses descriptors by default.** Sending descriptors is
opt-in, because it is the case that needs extra cleanup on both sides.

```rust
let c = Client::open("/var/run/foo.door")?;   // Client<NoDescriptors>
c.call(b"hello")?;                            // always available

// opt in. Checks door_info and fails if the door has DOOR_REFUSE_DESC.
let c = c.with_descriptors()?;                // Client<Descriptors>
c.call_with_descriptors(b"hello", &[fd])?;
```

There is no `Client<DescriptorsUnknown>` and no `assert_*` call in the
common path. A plain `Client` simply has no method that can send a
descriptor.

The reply side follows the same rule. A `Client<NoDescriptors>` closes
any descriptor the server sends back, immediately, and reports it as a
protocol error. It never hands the caller something to clean up.

**D2. Three crates, not four.** `doors-macros-core` is dropped. Proc
macro logic goes in `door-macros`, written against `proc_macro2` so it
can be unit tested in place. See [§7](#why-not-a-fourth-crate).

**D3. No separate `Jamb`.** The attached path lives inside `Door`. One
object, one destructor.

**D4. Fork is handled in three layers**: `FD_CLOEXEC` by default,
`doors::fork()` for the deliberate case, and a `pthread_atfork` backstop.
`Client` is left alone on `fork` but closed on `exec`. `EINTR` from
`door_call` is never retried silently. See
[F13](#f13-fork2-breaks-a-door-server-and-raii-makes-it-worse).

**D5. Door server threads use `std::thread::Builder`.** Sun threads and
POSIX threads are the same implementation on illumos, so there is
nothing to gain by switching wholesale. Reach for `thr_*` only for
`THR_DAEMON` and `thr_stksegment`, which have no POSIX equivalent. See
[F14](#f14-sun-threads-vs-posix-threads-the-same-threads).

**D6. The trampoline re-wraps and drops descriptors after a failed
`door_return`.** No reachable failure consumes them. See
[F15](#f15-descriptors-on-the-door_return-failure-path--resolved).

### Still open

1. **`ReplyBuf` sizing.** A stack array is free but fixed at compile
   time. A per-thread buffer handles any size but grows to the largest
   reply that thread ever sent. Proposal: a small inline array (say
   2 KiB) that spills to a per-thread buffer, with a configurable hard
   cap and `Err(ReplyTooBig)` above it. Does the cap belong on the
   `Door`, or on each server procedure?

2. **The revoke drain.** `door_revoke` lets in-flight calls finish, so
   `revoke()` must wait for them before it can hand back an owned `S`.
   What if a server procedure hangs? A timeout, or block forever?
   (Settled for the blocking question in Appendix C.2. No longer a
   soundness question: see Appendix E.1 item 1.)

3. **Should `doors` re-export `doors-sys`?** Today `doors::illumos` is
   public. That is convenient but freezes the raw layer as public API.

## 11. Suggested order of work

1. `doors-sys`: complete the FFI. Mechanical, and unblocks everything.
2. `Reply` and `Client` with guaranteed `munmap` (F5). Biggest safety
   win for the least design risk.
3. The trampoline: `catch_unwind`, `ReplyBuf`, non-falling-through
   `door_return` (F1, F2, F3, F15).
4. The fork handler, `FD_CLOEXEC` defaults, and `EINTR` as its own
   error variant (F13). This has to land before `Door` grows a
   teardown `Drop`, or we introduce a bug C never had.
5. `Door::builder` with `door_setparam` and `door_xcreate` (F11).
6. Cookie strategies and `revoke()` drain (F8).
7. Descriptors opt-in on the client, `ReceivedFd` / `SentFd` (F4, F6,
   F7).
8. Unref as a separate hook (F9).
9. The macro rewrite on top of all of the above.

Steps 1–4 already remove most of the ways to be wrong. Step 7 is the
part most worth arguing about first, because it shapes the public
API.

## Appendix A: evidence index

| Claim | Location |
| --- | --- |
| Double `door_return` idiom | `cmd/svc/configd/maindoor.c:134`, `:136` |
| `thr_exit` after `door_return` | `cmd/zonestat/zonestatd/zonestatd.c:4334` |
| Fall-through after `door_return` | `lib/libc/port/gen/klpdlib.c:68` |
| Same, with a null pointer | `cmd/zoneadmd/zoneadmd.c:1243` |
| `return` from a server procedure | `cmd/hotplugd/hotplugd_door.c:175` |
| No return address on the door stack | `uts/intel/os/door_support.c:48`, `uts/common/fs/doorfs/door_sys.c:1135` |
| Second protocol to free a reply buffer | `cmd/hotplugd/hotplugd_door.c:240`, `lib/libhotplug/common/libhotplug.c:1254` |
| `assert(n_desc == 0)` under `DOOR_REFUSE_DESC` | `cmd/svc/configd/maindoor.c:74` |
| Kernel enforces `DOOR_REFUSE_DESC` | `uts/common/fs/doorfs/door_sys.c:489` |
| Reply aliased onto the request, never checked | `lib/libnwam/common/libnwam_util.c:96` |
| Same shape | `lib/libvscan/common/libvscan.c:1372`, `lib/libzonecfg/common/libzonecfg.c:7797` |
| `d_id` closed instead of `d_descriptor` | `lib/libscf/common/lowlevel.c:634` |
| The correct version, in the next function | `lib/libscf/common/lowlevel.c:715` |
| Pointer cookie, `close` + `free`, no drain | `lib/libc/port/gen/klpdlib.c:113`, `:163` |
| Handle cookie with lookup and refcount | `cmd/svc/configd/client.c:2303` |
| `DOOR_UNREF_DATA` is `(void *)1` | `uts/common/sys/door.h:77` |
| Unref exempt from `DATA_MIN` | `uts/common/fs/doorfs/door_sys.c:483` |
| `SIGCANCEL` to the server thread | `uts/common/fs/doorfs/door_sys.c:740` |
| libc disables cancellation on its threads | `lib/libc/port/threads/door_calls.c:817` |
| `E2BIG` from the stack layout check | `uts/common/fs/doorfs/door_sys.c:1197` |
| Forked child threads flagged invalid | `uts/common/fs/doorfs/door_sys.c:2136` |
| libc pid tracking around `door_create` | `lib/libc/port/threads/door_calls.c:177` |
| libc unref thread stops in the child | `lib/libc/port/threads/door_calls.c:140` |
| Blind `EINTR` retry loop | `lib/libscf/common/lowlevel.c:622` |
| Unref fires on descriptor count | `uts/common/fs/doorfs/door_vnops.c:126` |
| `door_exit` only revokes doors this process created | `uts/common/fs/doorfs/door_sys.c:2097` |
| `fork` does not copy `p_door_list` | `uts/common/os/fork.c` (no reference) |
| `closefrom` + fresh door in a forked child | `cmd/nscd/nscd_selfcred.c:824`, `cmd/nscd/nscd_frontend.c:1288` |
| `FD_CLOEXEC` on client door fds in libc | `lib/libc/port/gen/getxby_door.c:280`, `lib/libscf/common/lowlevel.c:1279` |
| `pthread_t` is `thread_t` | `uts/common/sys/types.h:430` |
| `thr_create` and `pthread_create` share `_thrp_create` | `lib/libc/port/threads/thr.c:727`, `pthread.c:107` |
| `THR_BOUND` recorded but never acted on | `lib/libc/port/threads/thr.c:635` |
| `THR_DAEMON` has no pthread equivalent | `head/thread.h:111`, `thr.c:607` |
| libc uses `THR_DAEMON` for the unref thread | `lib/libc/port/threads/door_calls.c:209` |
| `thr_stksegment` used to size `door_return` | `lib/libc/port/threads/door_calls.c:365` |
| no `thr_atfork` exists | grep of `head/`, `lib/libc/` |
| `door_return` returns only in three places | `uts/common/fs/doorfs/door_sys.c:1392`, `:1442`, `:2974` |
| `ufcanalloc` runs before any descriptor is touched | `uts/common/fs/doorfs/door_sys.c:2854` |
| Asymmetric unwind on a bad descriptor | `uts/common/fs/doorfs/door_sys.c:2877` |

Line numbers are from the illumos-gate checkout at
`/Users/robert/Projects/illumos-gate`. It is not a git working copy, so
I could not date it. Treat the numbers as approximate and the code
quotes as exact.

## Appendix B: bugs found in illumos-gate

Everything the survey turned up, sorted by how confident I am. None of
these were the goal of the research — they are what fell out of reading
the door consumers closely.

Before filing anything upstream, re-check against current illumos: this
checkout could not be dated, so some may already be fixed.

### B1. libscf closes `d_id` instead of `d_descriptor`

**Confirmed. Descriptor leak, and can close an unrelated file.**

`lib/libscf/common/lowlevel.c:634`, in `make_door_call`:

```c
if (arg.desc_ptr->d_attributes & DOOR_DESCRIPTOR) {
        int cfd = arg.desc_ptr->d_data.d_desc.d_id;
        (void) close(cfd);
}
```

`d_id` is a `door_id_t` — a system-wide unique 64-bit number — truncated
to `int`. The descriptor the server actually sent is never closed, and if
the truncated id collides with a live fd, an unrelated file is. The next
function down, `make_door_call_retfd` at `:715`, has the same loop and
uses `d_descriptor` correctly.

### B2. klpdlib falls through when `door_return` fails

**Confirmed. Dereferences address 1.**

`lib/libc/port/gen/klpdlib.c:68`:

```c
if (argp == DOOR_UNREF_DATA) {
        (void) p->kd_callback(p->kd_user_cookie, NULL, NULL);
        (void) door_return(NULL, 0, NULL, 0);
}

klh = (void *)argp;        /* argp is (void *)1 here */
ka = KLH_ARG(klh);
```

No `return`, no `else`. `door_return` returns on `E2BIG`, `EMFILE`,
`EFAULT` and `EINVAL`, and `DOOR_UNREF_DATA` is `((void *)1)`.

### B3. klpdlib frees the cookie with no drain, and uses `close` not `door_revoke`

**Confirmed. Use-after-free window, plus the door stays callable.**

`lib/libc/port/gen/klpdlib.c:156`:

```c
err = syscall(SYS_privsys, PRIVSYS_KLPD_UNREG, p->kd_doorfd, ...);
if (close(p->kd_doorfd) != 0)
        err = -1;
free(p);
```

Two problems. `close` is not `door_revoke` — any other process holding a
descriptor to that door can still call in. And `p` is the door cookie: a
server thread may be inside `klpd_door_callback` holding it when it is
freed.

### B4. libvarpd panics on teardown after a failed create

**Confirmed. Sentinel mismatch, `-1` versus `0`.**

`vdi_doorfd` is initialised to `-1` (`libvarpd.c:115`) and stays `-1` if
`door_create` fails. But `libvarpd_door.c:462` tests against `0`:

```c
if (vip->vdi_doorfd != 0) {
        if (door_revoke(vip->vdi_doorfd) != 0)
                libvarpd_panic("failed to revoke door: %d", errno);
```

So calling `libvarpd_door_server_destroy` after a failed create runs
`door_revoke(-1)`, which fails, which panics the library.

### B5. libvarpd returns a clobbered `errno`

**Confirmed. Wrong error code reported.**

`lib/varpd/libvarpd/common/libvarpd_door.c:421`:

```c
if ((fd = open(path, O_CREAT | O_RDWR, 0666)) == -1) {
        ret = errno;
        if (door_revoke(vip->vdi_doorfd) != 0)
                libvarpd_panic(...);
        mutex_exit(&vip->vdi_lock);
        return (errno);        /* not `ret` */
}
```

`ret` is saved and never used. The function returns `errno`, which
`door_revoke` and `mutex_exit` may have overwritten in between.

### B6. Three more fall-throughs after `door_return`

**Confirmed. Same shape as B2, each then using a null pointer.**

| Site | Code |
| --- | --- |
| `cmd/nscd/nscd_frontend.c:940` | `if (argp == NULL) { (void) door_return(NULL, 0, 0, 0); }` then uses `phdr` |
| `cmd/ldapcachemgr/cachemgr.c:718` | `if (ptr == NULL) { (void) door_return(NULL, 0, 0, 0); }` then continues |
| `cmd/zoneadmd/zoneadmd.c:1242` | `if (zargp == NULL) { (void) door_return(NULL, 0, 0, 0); }` then uses `zargp` |

### B7. hotplugd returns from its server procedure

**Confirmed as undefined behaviour, though it needs `door_return` to
fail first.**

`cmd/hotplugd/hotplugd_door.c:175` does `(void) door_return(NULL, 0,
NULL, 0); return;`, and the function also falls off its end at `:259`.
The kernel enters a server procedure on a freshly laid-out stack with
`r_fp = 0` (`uts/intel/os/door_support.c:48`) and never plants a return
address (`door_layout`, `door_sys.c:1135`). There is nothing to return
to.

### B8. Clients that never `munmap` an overflow reply

**Latent. Fires only when a reply exceeds the caller's buffer.**

25 of the 52 files in `lib/` and `cmd/` that call `door_call` never call
`munmap` anywhere. The worst is `lib/libnwam/common/libnwam_util.c:96`,
which aliases the reply buffer onto the request:

```c
door_args.rbuf = (void *)request;
door_args.rsize = request_size;
```

If the server ever replies with more than `request_size` bytes, the
kernel maps a fresh area and updates `door_args.rbuf`. `nwam_make_door_call`
returns 0 without looking, and `send_msg_to_nwam` then reads
`request->nwda_status` — the *original, unmodified* buffer. So it leaks
a mapping **and** acts on stale data, silently.

`lib/libvscan/common/libvscan.c:1372` and
`lib/libzonecfg/common/libzonecfg.c:7797` have the same shape without the
aliasing.

### B9. Blind `EINTR` retry on non-idempotent door calls

**Latent, and a whole class rather than one site.**

`lib/libscf/common/lowlevel.c:622`, and the same pattern in
`libdlbridge`, `libvarpd`, `libtsol` and `librcm`:

```c
while ((r = door_call(h->rh_doorfd, &arg)) < 0) {
        if (errno != EINTR)
                break;
}
```

`EINTR` does not mean nothing happened. The server may have run the
procedure and only the reply was lost. Retrying re-executes it. The man
page says so: "`door_call()` is not a restartable system call [...] If
the door invocation is not idempotent the caller should mask any signals
that may be generated." Whether these particular protocols are idempotent
is not documented at any of the call sites.

### B10. `assert` used to check a kernel-enforced invariant

**A robustness smell rather than a defect.**

`cmd/svc/configd/maindoor.c:74`:

```c
/*
 * No file descriptors allowed
 */
assert(n_desc == 0);
```

The door is created with `DOOR_REFUSE_DESC`, so the kernel already
guarantees this (`door_sys.c:489`). The `assert` therefore adds nothing
— and it compiles out under `NDEBUG`, so it would not catch a regression
either. `client.c:2320` does the same check with `uu_die`, which at least
survives the optimiser.

### B11. `door_xcreate` crashes libc when the thread it asked for starts late

**Confirmed by running it. A segfault inside libc, from using a
documented interface the obvious way. The most serious item in this
list, and the only one measured rather than read.**

Reproducer: `experiments/xcreate3.c`. Measured on OmniOS r151058,
amd64.

`door_xcreate(3C)` takes a per-door thread creation function:

```c
typedef int door_xcreate_server_func_t(door_info_t *,
    void *(*)(void *), void *, void *);
```

The contract looks obvious. You are handed a start routine and its
argument; you create a thread that runs `f(arg)` and return 0:

```c
static int
create_thread(door_info_t *info, void *(*f)(void *), void *arg, void *ck)
{
        pthread_t t;
        if (pthread_create(&t, &attr, f, arg) != 0)
                return (-1);
        return (0);
}
```

That segfaults. `arg` points **into `door_xcreate`'s own stack frame**.
`door_xcreate` returns as soon as the creation function does, its frame
is reused, and the new thread then reads whatever landed there:

```
 fffffc7fef101e88 privdoor_data_hold (5251504f4e4d4c4b) + 8
 fffffc7fef102709 door_xcreate_startf (fffffc7fffdfa330) + 29
 fffffc7fef117997 _thrp_setup (fffffc7fef230240) + 77
 fffffc7fef117ce0 _lwp_start ()
```

`0x5251504f4e4d4c4b` is ASCII `KLMNOPQR` — leftover stack bytes being
dereferenced as a pointer. `0xfffffc7fffdfa330` is in the main thread's
stack, which is where `arg` pointed.

Three things make this worse than an ordinary race:

1. **It is not the caller's mistake.** Nothing in the interface says
   the creation function must block until the new thread has consumed
   its argument, and the natural reading is that it must not — a
   creation function that blocks defeats the point of supplying one.
2. **It fails as a crash, not an error.** `door_xcreate` has an errno
   return and uses it elsewhere. Here the caller's process dies.
3. **There is no man page.** `door_xcreate(3C)` does not exist on this
   system, and `door_create.3c` never mentions the function. The only
   description of the contract is the source.

Synchronising so the new thread copies the argument before the creation
function returns does remove the crash — and then `door_xcreate` fails
with `EINVAL` instead, so there is no obvious way to satisfy it at all:

| creation function | result |
|---|---|
| start the thread, return immediately | **SIGSEGV inside libc** |
| start the thread, wait for it to copy the argument | `EINVAL` |
| refuse to create a thread (return -1) | `EPIPE` |

Every attribute combination behaves the same, with and without
`DOOR_PRIVATE` (`experiments/xcreate2.c`).

**Suggested fix:** either keep the thread argument somewhere that
outlives `door_xcreate` — the heap, freed by `door_xcreate_startf` — or
document that the creation function must not return until the new
thread has taken it, and return `EINVAL` rather than crashing when it
does.

This crate therefore does not use `door_xcreate`. It uses
`door_create(3C)` with `door_server_create(3C)`, which is older, is
documented, and delivers the requested thread stack size correctly
(`experiments/servercreate.c`). Appendix D and `experiments/README.md`
have the details.

### Man page defects

### M0. `door_xcreate(3C)` has no man page at all

The function is in `<door.h>` and exported from libc, but there is no
`door_xcreate.3c`, and `door_create.3c` does not mention it. Its thread
creation contract is subtle enough that getting it wrong crashes the
process (see B11), so the absence of a page is itself the defect.

### M1. `door_create(3C)` names a parameter that does not exist

`man/man3c/door_create.3c:45`, in the `DOOR_UNREF` description:

> In the case of an unreferenced invocation, the values for `arg_size`,
> `dp` and **`n_did`** are 0.

There is no `n_did`. The parameter is `n_desc`, which the same page
spells correctly at `:16`, `:29`, `:85` and `:161`.

### M2. `door_return(3C)` says "arguments" where it means "results"

`man/man3c/door_return.3c:43`:

> `E2BIG` — Arguments were too big for client.

`door_return` sends *results* to the client. The arguments came from the
client in the first place, and are checked by `door_call`, which has its
own separate `E2BIG` ("Arguments were too big for server thread stack").
Two different conditions, one wording.

### M3. `door_return(3C)` does not say what happens to descriptors on failure

The page documents `E2BIG`, `EFAULT`, `EINVAL` and `EMFILE`, and it
explains descriptor release for the "no client" case. It says nothing
about whether a `DOOR_RELEASE` descriptor has been consumed when
`door_return` *returns an error*.

That is not a nitpick. `door_call(3C)` is careful about exactly this on
its side — "the descriptor will be closed even if `door_call()` returns
an error, unless that error is `EFAULT` or `EBADF`" — so the omission on
the `door_return` side reads as an oversight rather than a deliberate
silence. Settling it required reading `door_results` in the kernel. See
F15 for the answer.

### M4. `door_return(3C)` does not say the procedure must not return

The page never states that a server procedure which returns instead of
calling `door_return` has undefined behaviour. Given that the kernel
enters it with `r_fp = 0` and plants no return address, and given that
`hotplugd` gets this wrong (B7), it is worth a sentence.

### M5. `door_server_create(3C)`'s threading requirement is obsolete

The page says:

> The specified server creation function should create user level
> threads using `thr_create()` with the `THR_BOUND` flag, or in the case
> of POSIX threads, `pthread_create()` with the `PTHREAD_SCOPE_SYSTEM`
> attribute.

That mattered under the old M:N thread model. illumos is strictly 1:1
now: `_thrp_create` records `THR_BOUND` in `ul_usropts`
(`lib/libc/port/threads/thr.c:635`) and never branches on it. The advice
is harmless but misleading — it implies a thread created with default
attributes is unsuitable, which is no longer true.

### M6. `door_bind(3C)` attributes forkall behaviour to `fork(2)`

`man/man3c/door_bind.3c:56`:

> If a process containing threads that have been bound to a door calls
> `fork(2)`, **the threads** in the child process will be bound to an
> invalid door.

Plural "threads" describes `forkall(2)`, and the kernel comment agrees —
`door_fork` at `door_sys.c:2120` opens with "The process is executing
`forkall()`". `fork(2)` on illumos clones only the calling thread, so at
most one thread can be affected, and only if the forking thread was
itself bound.

---

# Appendix C: Decisions made during implementation

`GOALS.md` §11 leaves three choices to whoever writes the code. Here
they are, with the reasoning.

## C.1 The `ReplyBuf` hard cap belongs to the server procedure

**Decision:** each generated trampoline carries its own limit, and
`ReplyBuf::DEFAULT_LIMIT` (64 KiB) applies unless the procedure says
otherwise. It is not a `Door` setting.

The alternative was to hang it off the `Door`. That reads better in a
builder chain, but it does not survive contact with the trampoline. The
trampoline is a `static extern "C"` function: the only things it
receives are the five C arguments and whatever the generated code
passes as constants. To read a per-`Door` limit it would have to
resolve the cookie first and look the number up — a table lookup on
every call, to enforce a value that never changes.

So the limit is a constant where the reply is actually built. That is
the procedure.

## C.2 `Door::revoke()` blocks, with no timeout

**Decision:** `revoke()` waits for the in-flight counter to reach zero
and never gives up.

A timeout sounds safer and is not. The counter exists so the state is
not freed while a call is still using it; if `revoke()` gave up and
returned after ten seconds, it would have to either free the state
anyway — a use-after-free — or hand the `Door` back, which makes the
signature `Result<S, RevokeError<Door<S>>>` and pushes the same
decision onto the caller with less information than we have.

A server procedure that never returns is a bug in that procedure.
Blocking makes it show up as a hang in a debugger, with the offending
thread on the stack. The alternatives turn it into memory corruption or
into an error nobody can act on.

`door_revoke(3C)` itself returns immediately, so a caller who wants the
door shut without waiting for the drain can drop the `Door` instead:
`Drop` revokes and deregisters without waiting, because it has no state
to hand back.

## C.3 `doors` does not re-export `doors-sys`

**Decision:** the raw layer is not public. `doors::Errno` is
re-exported, and nothing else. The old `doors::illumos` module is gone.

Keeping it public would freeze `doors-sys` as part of the `doors` API:
every change to a binding, every corrected signature, every added
constant becomes a semver event for a crate that has nothing to do with
it. `doors-sys` is exactly the layer we expect to keep correcting — the
`door_return` errno finding in Appendix D is the first of those, and it
arrived within a day of writing the crate.

Anyone who needs the raw calls can depend on `doors-sys` directly. It
is a published crate, it is documented, and depending on it is an
explicit choice to work below the safety layer rather than something
you fall into through a re-export.

---

# Appendix D: What the kernel actually does

`GOALS.md` §9.3 asked for one experiment. Running it changed three
things in the spec. The programs are in `experiments/`, the results in
`experiments/README.md`. Summarised:

- **A failed `door_return` does not consume descriptors.** Confirmed in
  every failure reachable from the safe API, with and without
  `DOOR_RELEASE`, checking file identity rather than just "the number
  is still open". Trampoline rule 4.2.4 stands.

- **`door_return` can fail without setting `errno`.** We set `errno` to
  zero immediately before the call and read zero back after it returned
  `-1`. The `doors-sys` wrapper reports `EINVAL` on that path, because
  its return type forbids zero and inventing a success would be worse.

- **`E2BIG` is not reachable from the reply side.**
  `DOOR_PARAM_DATA_MAX` caps *request* data. A 512 KiB reply under a
  1 KiB cap goes through, and so does a 256 MiB one. Reply overflow is
  reported to the client's `door_call`, never back to the server, which
  matches `door_results()` handing `EOVERFLOW` to `ct->d_error`.

- **The kernel silently drops descriptors it cannot deliver.** The
  client is told the call succeeded, with `desc_num=0`; the server is
  told `-1`. Neither side is told a descriptor went missing. A caller
  that needs one must check the count — success does not imply
  delivery.

---

# Appendix E: `door_revoke` closes the descriptor

**Status: fixed.** Root cause below; reproducer in
`doors/examples/fattach_race.rs`. Before the fix the reproducer failed
9 runs out of 10 at `THREADS=16 ROUNDS=400`; after it, 10 out of 10 are
clean and the suite passes 20 times in a row.

## The bug

`door_revoke(3C)` does not merely invalidate a door descriptor. **It
closes it.** `experiments/revoke_closes.c` shows this directly:

```
door_create returned fd 3
before revoke: fd 3 is open
door_revoke(3) = 0
after revoke:  fd 3 is CLOSED (errno 9, Bad file number)
close(3) = -1 errno=9 (Bad file number)
```

`Door::drop` and `Door::revoke` both called `door_revoke(fd)` and then
asked the registry to `close(fd)`. That second close is a double close.

In a single-threaded program a double close is nearly harmless: the
second one returns `EBADF` and nothing else happens. That is why the
suite always passed with `--test-threads=1`.

With threads it is not harmless at all. Between the two closes another
thread can be handed that descriptor number, and our second close then
shuts **its** door. `truss` catches it in the act:

```
1388/2:  door_revoke(3)   = 0           thread 2 revokes its door
1388/3:  door_create(...) = 4           thread 3 makes a new door
1388/2:  close(3)         Err#9 EBADF   thread 2 closes 3 a second time
```

Every symptom follows from this one fact:

- `fattach` fails with `EBADF`, because the door it was given has been
  closed out from under it.
- `door_call` fails with `EBADF`, for the same reason.
- Rust aborts with `IO Safety violation: owned file descriptor already
  closed` when the descriptor that got recycled belonged to a
  `Client`'s `OwnedFd`.

## Why it took so long to find

The two bugs fixed before this one were real — a registry slot-reuse
race, and `Door::revoke` returning through a `?` placed before
`mem::forget(self)` so `Drop` tore down twice. Both were genuine
descriptor-lifetime defects and both are still worth having fixed. But
neither was *this*, so the symptom survived them, and the failure rate
moved around enough (2 in 10 to 8 in 10) to make it look as though it
had.

The lesson is that the symptom was three moves away from the cause, and
the thing that finally settled it was not reading code. It was a
reproducer small enough to run under `truss`.

## The fix

The descriptor must be released exactly once, and `door_revoke` is that
release for a door this process owns. A door that was disowned by a
`fork` is the other case: the child must not revoke its parent's door,
so there it is a plain `close`.

Both have to happen inside the registry's critical section, because
§7.1's ordering rule is what stops a concurrent `fork` from closing a
descriptor number something else has already been given — and that is
the very hazard this bug demonstrates is real.

That is what `registry::deregister_and_release(inner, key, revoke)`
does. It is the only place a live door descriptor goes away.
`Door::drop` and `Door::revoke` no longer call `door_revoke`
themselves; they pass `revoke = true` when the door is ours and
`revoke = false` when a `fork` disowned it. `DoorBuilder::build`'s
`door_setparam` failure path had the same double close and now only
revokes.

When `door_revoke` fails, the descriptor is left alone rather than
closed. If it failed with `EBADF` the number is already free and may
belong to another thread by now, so a fallback close would be this very
bug again.

## A trap when checking a fix on the VM

`make sync` uses `rsync -a`, which preserves modification times. If a
file on the VM is edited in place and then synced back over, its mtime
can move *backwards* past the build artefacts, and cargo will decide
nothing needs rebuilding. A run then measures the old binary. Run
`find . -name '*.rs' -exec touch {} +` on the VM before building if you
have edited anything there by hand.

---

# Appendix E.0: original report, kept for the record

**Status: superseded by the diagnosis above.** Reproducible, not diagnosed. Now the only failing
test in the suite: everything else, including both interop directions,
is green.

## What happens

Running `cargo test --workspace` on illumos fails roughly 3 times in
10. The failure is always the same shape: `Door::attach` fails, with
either `EBADF` (9) or `EINVAL` (22) from `fattach`.

```
thread 'sending_to_a_refusing_door_reports_an_error' panicked at
  attach "/tmp/doors_fd_refused_send": fattach failed: errno 9
thread 'call_into_uses_the_callers_buffer_and_reports_overflow' panicked at
  attach: Sys { call: "fattach", errno: 22 }
```

## What is known

- It never happens with `--test-threads=1`. It needs several threads
  in one process building and dropping doors at once.
- It is **not** contention between test binaries. Running the
  `roundtrip` binary on its own still fails 2 times in 10, and separate
  binaries are separate processes with separate descriptor tables.
- `EBADF` means the door descriptor was closed while a live `Door`
  still held it. That is a lifetime bug, not a kernel quirk.
- The failure rate is unstable between sessions, ranging from about
  2 in 10 to 8 in 10 of full-suite runs. Do not use it to judge whether
  a change helped without running many more than ten iterations.
- Two real defects were found and fixed along the way. Neither
  removed the failure:
  1. A slot-reuse race in the registry, where a stale key could close
     the descriptor belonging to whichever door had since taken that
     slot.
  2. `Door::revoke` returned early through a `?` placed *before*
     `std::mem::forget(self)`, so `Drop` then ran the whole teardown a
     second time, deregistering and closing with a key that another
     door may already have been given. `Drop` now only tears down while
     `self.handle` is still `Some`.

  Both were genuine descriptor-lifetime bugs and both are worth
  keeping fixed. That the symptom survives them means at least one
  more path closes a descriptor too early, or closes one twice.

## What has been ruled out

- Cross-process interference (see above).
- Path collisions. Every test uses its own name under `/tmp`.
- The registry slot-reuse race, which is now covered by
  `registry::tests::a_stale_key_cannot_close_the_door_that_took_the_slot`
  and a concurrent stress test.

## Where to look next

The suspects are the things shared by every door in a process:

1. `door_server_create` installs **one** thread-creation callback for
   the whole process (see Appendix D and `experiments/README.md`). The
   `STACK_SIZES` table it consults is keyed by cookie. If `Pooled`
   hands out cookies that repeat after a door is dropped — a slot index
   would do exactly that — two live doors could collide in that table.
   That alone would only give a wrong stack size, but it suggests
   cookie values are being reused in a way other code may also assume
   they are not.
2. `Door::drop` and `Door::revoke` both tear down. `revoke` calls
   `mem::forget(self)` afterwards so `Drop` does not repeat the work.
   Worth re-checking every early-return path in `revoke` for one that
   leaves the door both deregistered and still owned.
3. The error paths in `DoorBuilder::build`, which close the descriptor
   by hand before the door is ever registered.

A good next step is to make every close in the crate go through one
function that records the descriptor number and asserts it was open,
then run the suite until it fails.

## E.1 Further defects found while chasing this

An audit of the cookie slab and trampoline while investigating the
above **ruled out** three suspects, with reasons worth keeping:

- The cookie is not a bare slot index. `pack()` puts a
  generation counter in the high half, and `slab_uninstall` bumps that
  generation *before* returning the index to the free list, under one
  write guard. Two live doors therefore cannot share a cookie.
- `STACK_SIZES` has no key collision, for the same reason.
- Descriptor ownership in `trampoline.rs` moves exactly once on every
  path, received and reply alike.

It also turned up three defects that are **not** the `fattach` failure
but are real, and are listed here in the order they should be fixed:

1. **`in_flight` is never incremented.** Only the initialiser and the
   load in `Door::revoke` exist. The drain loop therefore always reads
   zero and returns immediately, so `revoke()` does **not** wait for
   calls in flight, which `GOALS.md` §5.1 requires.

   This is **not** a soundness bug. It was one while the `Pinned`
   cookie strategy existed, because that strategy had nothing but the
   drain keeping the state alive. `Pinned` is gone. The slab is now the
   only design, and resolving a cookie clones an `Arc` under the slab
   lock, so an in-flight call already holds its own reference and the
   state cannot be freed underneath it. There is no use-after-free
   window left to close.

   What the missing counter still costs is the *shape* of the value
   `revoke()` hands back. Without a drain, a call may still be holding
   its clone when `Arc::try_unwrap` runs, so `revoke()` can return
   `Err(StateStillShared)` where it should have waited and returned an
   owned `S`. That is a quality-of-result defect, not a memory-safety
   one. The trampoline should still raise the counter before resolving
   the cookie and lower it after the user function returns.

2. **`STACK_SIZES` never shrinks.** Nothing removes an entry except the
   `door_create` failure path, so in a process that builds and drops
   many doors the table grows for the life of the process — and it is
   scanned under a global `Mutex` every time a server thread is
   created. Not a correctness bug, but the contention grows without
   bound. `Door::drop` should forget its cookie.

3. **`Door::revoke`'s `mem::forget(self)` leaks** the `Arc<DoorInner>`
   and the `attached` vector. Memory only, and small, but it is a leak
   on the ordinary shutdown path. Taking the fields out by hand, or
   using `ManuallyDrop`, would fix it.

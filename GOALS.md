# GOALS

Spec for rewriting this workspace to cover the whole illumos Doors API.

`docs/DESIGN.md` holds rationale. This file is the spec. Where they
disagree, this file wins.

MUST, MUST NOT, SHOULD, MAY are requirements. Code blocks are normative
unless the section says otherwise.

---

## 1. Crates

Three crates, one workspace.

- `doors-sys` — raw FFI. No safety, no ergonomics, no allocation.
  Depends only on `libc`.
- `door-macros` — `proc-macro = true`. All macro logic. Depends only on
  `syn`, `quote`, `proc-macro2`. MUST NOT depend on `doors`,
  `doors-sys`, `serde` or `postcard` outside `[dev-dependencies]`.
- `doors` — the safe API. Depends on `doors-sys` and `door-macros`, and
  re-exports the macros. MAY depend on `serde` and `postcard`, only
  behind the optional `rpc` feature (§3.8).

`doors` depends on both others; the other two are independent of each
other. Do NOT add a fourth crate for macro logic.

illumos only. Reference target `x86_64-unknown-illumos`.

---

## 2. `doors-sys`

Expose the complete C surface. Every item below MUST be present.

### 2.1 Functions

Bind exactly these:

```c
typedef void door_server_procedure_t(void *, char *, size_t, door_desc_t *, uint_t);
typedef void door_server_func_t(door_info_t *);
typedef int  door_xcreate_server_func_t(door_info_t *, void *(*)(void *), void *, void *);
typedef void door_xcreate_thrsetup_func_t(void *);

int door_create(door_server_procedure_t *, void *, uint_t);
int door_xcreate(door_server_procedure_t *, void *, uint_t,
                 door_xcreate_server_func_t *, door_xcreate_thrsetup_func_t *,
                 void *, int);
int door_revoke(int);
int door_info(int, door_info_t *);
int door_call(int, door_arg_t *);
int door_return(char *, size_t, door_desc_t *, uint_t);
int door_cred(door_cred_t *);
int door_ucred(ucred_t **);
int door_bind(int);
int door_unbind(void);
int door_getparam(int, int, size_t *);
int door_setparam(int, int, size_t);
door_server_func_t *door_server_create(door_server_func_t *);
```

All of them go in a private `extern "C"` block returning `c_int` exactly
as C declares. That block is not public API. Re-export every binding
directly except `door_return`; their public signatures are the C ones.

`door_return` is the one exception. Publish it as a thin `#[inline]`
wrapper:

```rust
/// Returns *only* on failure. On success control never comes back, so
/// there is no success value. The raw C function returns -1; this
/// wrapper reads errno and returns it, never zero.
///
/// Measured caveat (§9.3): door_return can fail while leaving errno
/// at zero. The wrapper reports EINVAL in that case rather than
/// fabricate a success the type system forbids.
pub unsafe fn door_return(
    data_ptr: *const c_char,
    data_size: size_t,
    desc_ptr: *const door_desc_t,
    num_desc: c_uint,
) -> Errno;
```

Not `!` — it does return. Not `c_int` — zero is impossible. This is the
only departure from the no-wrappers rule in §2.5.

### 2.2 Types

```rust
/// Non-zero errno. Zero is unrepresentable, which makes door_return's
/// signature honest.
pub type Errno = core::num::NonZeroI32;
```

A type alias, not a wrapper type. `doors` MUST re-export it as
`doors::Errno` however §11.3 is settled.

Also expose `door_desc_t`, `door_arg_t`, `door_info_t`, `door_cred_t`,
`door_return_desc_t`, `door_id_t`, `door_ptr_t`, `door_attr_t`.
`ucred_t` comes from `libc`; do not redefine it.

`door_desc_t` MUST reproduce the C layout exactly, union and
`d_resv[5]` arm included:

```c
typedef struct door_desc {
        door_attr_t     d_attributes;
        union {
                struct {
                        int             d_descriptor;
                        door_id_t       d_id;
                } d_desc;
                int     d_resv[5];
        } d_data;
} door_desc_t;
```

Honour the `#pragma pack(4)` guard `<sys/door.h>` applies to
`door_desc_t` and `door_info_t` on amd64. Assert `size_of` and
`align_of` for those two plus `door_arg_t` against values taken from a C
compile on illumos; commit the expected values as constants.

### 2.3 Constants

Create flags: `DOOR_UNREF`, `DOOR_PRIVATE`, `DOOR_UNREF_MULTI`,
`DOOR_REFUSE_DESC`, `DOOR_NO_CANCEL`, `DOOR_NO_DEPLETION_CB`,
`DOOR_PRIVCREATE`.

Info flags: `DOOR_LOCAL`, `DOOR_REVOKED`, `DOOR_IS_UNREF`,
`DOOR_DEPLETION_CB`.

Descriptor attributes: `DOOR_DESCRIPTOR`, `DOOR_RELEASE`.

Sentinels: `DOOR_INVAL`, `DOOR_UNREF_DATA`, `DOOR_QUERY`.

Parameters: `DOOR_PARAM_DESC_MAX`, `DOOR_PARAM_DATA_MAX`,
`DOOR_PARAM_DATA_MIN`.

Masks: `DOOR_CREATE_MASK`, `DOOR_ATTR_MASK`.

### 2.4 Other headers

`<stropts.h>`: `fattach`, `fdetach`.

`<thread.h>`: `thr_create`, `thr_stksegment`, `thr_min_stack`, and
`THR_BOUND`, `THR_DETACHED`, `THR_DAEMON`, `THR_SUSPENDED`,
`THR_NEW_LWP`.

`<pthread.h>`: `pthread_atfork`, `pthread_setcancelstate`,
`PTHREAD_CANCEL_DISABLE`.

Errno: `pub fn errno() -> c_int` reading `libc::___errno`. The
`door_return` wrapper uses it, and `doors` uses it to map every `-1`
onto an error variant.

### 2.5 Constraints

Every published function MUST be `unsafe`. No wrapper types, no
`Result`, no `Drop`, no allocation — except the `door_return` wrapper
and `errno()`.

**illumos only.** Do NOT gate anything behind `#[cfg(target_os = ...)]`
to make the crate build elsewhere. It builds on illumos or it does not
build. There is nothing to learn from compiling a doors crate on a
machine with no doors, and the stubs needed to fake it cost more than
they are worth.

---

## 3. `door-macros`

### 3.1 Structure

All logic against `proc_macro2::TokenStream`. The
`#[proc_macro_attribute]` function MUST be a thin shim:

```rust
fn expand(attr: proc_macro2::TokenStream, item: proc_macro2::TokenStream)
    -> proc_macro2::TokenStream { /* all the work */ }

#[proc_macro_attribute]
pub fn server(
    attr: proc_macro::TokenStream,
    item: proc_macro::TokenStream,
) -> proc_macro::TokenStream {
    expand(attr.into(), item.into()).into()
}
```

This makes the logic unit-testable in-crate with `#[cfg(test)]`. Write
those tests.

### 3.2 One entry point

Export exactly one `#[proc_macro_attribute]`, `server`, parsing a
`syn::ItemImpl`. `doors` re-exports it, so users write
`#[doors::server]`.

Server procedures live on an `impl` block so they can take `&self`,
supplied from the door cookie.

Methods are marked with an inert `#[door(...)]`. `#[door]` is NOT a
registered proc macro; the `#[doors::server]` expansion consumes and
strips it.

Do NOT export `server_procedure`, `server_rpc` or `server_raw`
separately. Do NOT create a macro per flag.

### 3.3 `#[door(...)]` options

All options go in the per-method `#[door(...)]`. The outer
`#[doors::server]` takes none.

Shape — at most one keyword; omitting all means `procedure`:

- `procedure` — `fn(&self, Request<'_, D>) -> Result<Vec<u8>, E>`
- `rpc` — `fn(&self, Req) -> Result<Resp, E>`, see §3.8
- `reply_buf` — `fn(&self, Request<'_, D>, &mut ReplyBuf) -> Result<(), E>`
- `raw` — the C server-procedure signature, validated and emitted
  unchanged

`D` is `NoDescriptors` when `refuse_desc` is set, `Descriptors`
otherwise (§5.4).

Flags — any combination:

- `refuse_desc` → `DOOR_REFUSE_DESC`
- `unref` → `DOOR_UNREF`
- `unref_multi` → `DOOR_UNREF_MULTI`
- `private` → `DOOR_PRIVATE`
- `untagged` → reply with no §3.9 status byte, for callers that do not
  use this crate (§6.5)
- `request_size = <range>` → `DOOR_PARAM_DATA_MIN` / `DATA_MAX`
- `max_descriptors = <n>` → `DOOR_PARAM_DESC_MAX`

`DOOR_NO_CANCEL` is always set and MUST NOT be an option.

Reject an unknown keyword, a repeated flag, or two shape keywords with
`syn::Error`.

### 3.4 Effects on generated code

With `refuse_desc` the method takes `Request<'_, NoDescriptors>`, which
cannot reach a descriptor; generate no runtime check.

With `unref` or `unref_multi`, generate a `DOOR_UNREF_DATA` check
dispatching to `on_unreferenced`, and `syn::Error` if the user did not
write one. Without those flags generate no check and require no such
method. Under `unref_multi`, `on_unreferenced` MUST take one argument
that has already re-checked `DOOR_IS_UNREF` via `door_info`.

`raw` suppresses trampoline generation. The method is emitted unchanged
and calls `doors_sys::door_return` itself. That does not violate §12.1,
which binds the safe API only.

### 3.5 Example

```rust
struct Greeter { greeting: String }

#[doors::server]
impl Greeter {
    #[door(rpc, refuse_desc, unref, request_size = ..=8192)]
    fn hello(&self, req: HelloReq) -> Result<HelloResp, MyError> { ... }

    #[door(refuse_desc)]                       // shape defaults to procedure
    fn ping(&self, req: Request<'_, NoDescriptors>) -> Result<Vec<u8>, MyError> { ... }

    fn on_unreferenced(&self) { ... }
}
```

### 3.6 Constructors

For each `#[door]`-annotated method `foo` on `impl T`, generate a local
extension trait in the user's crate:

```rust
trait TDoors { fn build_foo(self) -> Result<Door<T>, Error>; }
impl TDoors for doors::DoorBuilder<T> { fn build_foo(self) -> ... { ... } }
```

Do NOT emit an inherent `impl` on `doors::DoorBuilder` — Rust forbids
inherent impls on foreign types (E0116), and a foreign trait impl too
(E0117). Name the trait after the self type and match the `impl` block's
visibility, so call sites read:

```rust
let mut door = Door::builder(Greeter { .. })
    .thread_stack_size(256 * 1024)
    .build_hello()?;
```

`build_foo()` replaces `build()` as the terminal method. It MUST run
every check `build()` runs, MUST always set `DOOR_NO_CANCEL`, and MUST
apply the attribute's `request_size` and `max_descriptors`. If the user
also called `.request_size()` or `.max_descriptors()` on the builder,
emit a compile error rather than silently picking one.

### 3.7 Hygiene

Generated code MUST reference a hidden `doors::__private` module, never
public paths, and MUST NOT name `serde` or `postcard` paths. Reject bad
input with `syn::Error::to_compile_error`, never `panic!`. Cover every
rejection with a `trybuild` compile-fail test.

### 3.8 `rpc` and serialisation

`doors` MUST gain an optional, default-off `rpc` feature enabling
`serde` and `postcard`. Under it, `Req: serde::de::DeserializeOwned` and
`Resp: serde::Serialize`, with `postcard` as the wire format. Generated
code MUST call `doors::__private::rpc::decode_request` and
`doors::__private::rpc::encode_reply`. `#[door(rpc, ...)]` MUST emit a
clear `syn::Error` when the feature is off. `door-macros` MUST NOT
depend on `serde` or `postcard`.

### 3.9 Error replies

Every trampoline reply is a one-byte status tag then a payload:

- `0` — Ok; payload is the user function's reply bytes.
- `1` — the user function returned `Err(E)`; payload is `E`'s encoding.
- `2` — infrastructure failure; payload is a `ServerFault` discriminant.

Tag `2` covers a panic caught by `catch_unwind` (§4.2 rule 3) and a
failed cookie resolution (§5.3). It MUST NOT carry the panic message.

```rust
#[non_exhaustive]
pub enum ServerFault { Panicked, StateUnavailable, ReplyTooBig }

pub trait ErrorReply {
    fn write_error(self, out: &mut ReplyBuf) -> Result<(), ReplyTooBig>;
}
```

Blanket-implement `ErrorReply` for `E: core::fmt::Display`, writing the
`Display` output as UTF-8. Every `E` in an `rpc` or `reply_buf` shape
MUST satisfy it. Client side is §6.4.

---

## 4. Trampoline

The generated `extern "C"` function registered with `door_create`. The
only place in `doors` that calls `door_return`.

### 4.1 Shape (illustrative; §4.2 is normative)

```rust
extern "C" fn trampoline(cookie, argp, arg_size, dp, n_desc) {
    let mut out = ReplyBuf::new();

    {   // scope 1
        let state = resolve_cookie(cookie);
        let reply = catch_unwind(|| user_fn(state, request));
        out.fill_from(reply);          // applies the §3.9 status tag
    }   // scope 2 begins: no live destructors past this point

    let _ = door_return(out.as_ptr(), out.len(), out.fds(), out.nfds());

    // Only reachable on failure, and no descriptor was consumed.
    for fd in out.take_raw_fds() {
        drop(unsafe { OwnedFd::from_raw_fd(fd) });
    }

    let _ = door_return(null(), 0, null(), 0);
    std::process::abort();
}
```

### 4.2 Rules

1. MUST NOT be able to fall off its end. Terminate with `abort()` after
   the second `door_return`.
2. Every value with a destructor MUST be dropped before the first
   `door_return`. Nothing owning heap memory, a lock guard or an
   `OwnedFd` may be alive in scope 2.
3. The user function MUST be wrapped in `catch_unwind` inside scope 1,
   so a panic payload is dropped before `door_return`. A caught panic
   produces a §3.9 tag `2` reply.
4. Reply descriptors MUST cross into scope 2 as raw `int`s, and MUST be
   re-wrapped as `OwnedFd` and dropped if `door_return` returns. No
   `door_return` failure reachable from the safe API consumes a
   descriptor. §9.3 verifies this.
5. Users MUST NOT be able to call `door_return` through `doors`. The
   `raw` shape calls `doors-sys` directly and is outside this rule.

### 4.3 `ReplyBuf`

Somewhere to hold reply bytes that needs no freeing. Small replies go in
an inline array in the trampoline's own stack frame; larger ones spill
to a per-thread buffer reused across calls on that thread; above a
configurable hard cap, produce `Err(ReplyTooBig)` rather than growing.
MUST expose the writer interface `ErrorReply` needs and MUST implement
`core::fmt::Write`. Default the inline size to 2 KiB unless measurement
says otherwise.

---

## 5. Server types

### 5.1 `Door<S>`

```rust
pub struct Door<S> { /* see requirements */ }

impl<S> Door<S> {
    pub fn builder(state: S) -> DoorBuilder<S>;
    pub fn info(&self) -> Result<DoorInfo, Error>;
    pub fn revoke(self) -> Result<S, RevokeError>;
    pub fn attach<P: AsRef<Path>>(&mut self, path: P) -> Result<(), Error>;
    pub fn detach<P: AsRef<Path>>(&mut self, path: P) -> Result<(), Error>;
}
```

- No separate `Jamb` type. Attached paths live inside `Door`.
- The fd, the `disowned: AtomicBool` of §7, and the owning pid MUST live
  behind a shared type-erased inner allocation (`Arc<DoorInner>`) so the
  fork registry can reach them without knowing `S`. `Door<S>` moves;
  `DoorInner` MUST NOT.
- `Door::drop` MUST `door_revoke`, then `fdetach` and `unlink` every
  attached path — unless `disowned` is set or the current pid differs
  from the owning pid, in which case it does none of those three. It
  MUST still deregister and close per §7.1.
- `Door::revoke()` MUST revoke, wait for the in-flight counter to reach
  zero, drop the state, return it. `Err(Disowned)` when `disowned`.
- `Door::detach()` MUST return `Err(Disowned)` when `disowned`.
- `Door` MUST NOT implement `AsRawFd` or `IntoRawFd`.

### 5.2 `DoorBuilder<S>`

```rust
Door::builder(state)
    .request_size(0..=64 * 1024)     // DATA_MIN and DATA_MAX
    .max_descriptors(0)              // DESC_MAX
    .thread_stack_size(256 * 1024)   // via door_xcreate
    .build()?;                       // or .build_foo() from §3.6
```

`build()` and every generated `build_foo()` MUST reject a thread stack
too small for the declared request size — request data, descriptors,
`door_info_t` and `door_results` all land on the server thread stack, so
the check is real — and MUST always set `DOOR_NO_CANCEL`. `build()` is
for hand-written raw server procedures; macro-generated doors use
`build_foo()`.

### 5.3 Cookies

There is one cookie design, and it is not a choice.

The cookie is a slot index into a process-global slab, packed with a
generation counter. Resolving takes the slab's lock, checks the
generation, and clones an `Arc<T>` out — all under that one lock. So
state outlives a concurrent revoke: an in-flight call holds its own
reference. Resolution MUST be able to fail; a failure MUST produce a
§3.9 tag `2` reply and MUST NOT dereference anything.

Never dereference a cookie without going through the lookup.

There used to be a second design, `Pinned`, where the cookie was a
leaked `Arc<T>` pointer and resolving needed no lookup. It has been
removed, because it could not be written soundly. Resolving means
turning a raw pointer back into a reference count, and that is only
valid while the allocation is alive — which a raw pointer cannot tell
you. (`Arc` offers no such operation, for the same reason.) The slab
gets away with it only because its lock makes "check alive" and "take a
reference" one step. So the fast path was not implementable, and one
design is all there is.

### 5.4 `Request`

A library type in `doors`, parameterised by the same descriptor
typestate as `Client`:

```rust
pub struct Request<'a, D = NoDescriptors> { /* data + private desc store */ }

impl<D> Request<'_, D> {
    pub fn data(&self) -> &[u8];
    pub fn peer(&self) -> Result<UCred<'_>, Error>;   // door_ucred
}

impl Request<'_, Descriptors> {
    pub fn descriptors(&self) -> &[ReceivedFd];
}
```

`Request<'_, NoDescriptors>` has no method reaching a descriptor. The
macro picks the parameter from `refuse_desc`.

`peer()` MUST borrow from `Request`, so credentials cannot be requested
outside an invocation. `UCred` MUST reuse its allocation via
`&mut self`, matching `door_ucred(3C)`, and free it on `Drop`.

### 5.5 Server threads

Create them with `std::thread::Builder`, using `stack_size` from the
builder, and call
`pthread_setcancelstate(PTHREAD_CANCEL_DISABLE, NULL)` first thing on
each. Use `thr_create` with `THR_DAEMON` only for background threads the
crate owns, so they do not hold the process open at exit. Use
`thr_stksegment` to read the current stack when validating
`DOOR_PARAM_DATA_MAX`.

---

## 6. Client types

### 6.1 Descriptors are opt-in

```rust
pub struct Client<D = NoDescriptors> { fd: OwnedFd, _d: PhantomData<D> }

impl Client<NoDescriptors> {
    pub fn open<P: AsRef<Path>>(path: P) -> io::Result<Self>;
    pub fn with_descriptors(self) -> Result<Client<Descriptors>, Error>;
}

impl Client<Descriptors> {
    pub fn call_with_descriptors(&self, data: &[u8], fds: Vec<SentFd<'_>>)
        -> Result<Reply, CallError>;
}

impl<D> Client<D> {
    pub fn info(&self) -> Result<DoorInfo, Error>;
    pub fn limits(&self) -> Result<DoorParams, Error>;    // door_getparam
    pub fn call(&self, data: &[u8]) -> Result<Reply, CallError>;
    pub fn call_into<'b>(&self, data: &[u8], buf: &'b mut [u8])
        -> Result<&'b [u8], CallError>;
    pub fn call_idempotent(&self, data: &[u8]) -> Result<Reply, CallError>;
}
```

`call_with_descriptors` MUST take them by value. It cannot borrow: the
kernel closes a `DOOR_RELEASE` descriptor on most errors, so a caller
left holding an `OwnedFd` would double close.

- `Client` MUST NOT implement `AsRawFd` or `IntoRawFd`.
- `with_descriptors()` MUST read `door_info` and fail if the door has
  `DOOR_REFUSE_DESC`.
- `Client<NoDescriptors>` MUST close any descriptor the server returns,
  immediately, and report a protocol error. It MUST NOT hand the caller
  anything to clean up.
- Set `FD_CLOEXEC` on every client door fd by default, with an explicit
  opt-out.

### 6.2 `Reply`

Client-side only. A server procedure returns `Result<Vec<u8>, E>` or
writes into a `ReplyBuf`; it never constructs a `Reply`.

- MUST own any mapping the kernel created and `munmap` it on `Drop`,
  unconditionally.
- `data()` MUST return a slice borrowed from `Reply`, so it cannot
  outlive the mapping.
- Descriptors MUST be extracted from the mapped region **before** it is
  unmapped.
- `call_into` MUST unmap an overflow mapping immediately and return
  `Err(ReplyTooBig { needed })`. The caller MUST never observe a
  mapping on that path.

### 6.3 Descriptor types

```rust
pub enum SentFd<'a> {
    Shared(BorrowedFd<'a>),   // DOOR_DESCRIPTOR
    Released(OwnedFd),        // DOOR_DESCRIPTOR | DOOR_RELEASE
}

pub struct ReceivedFd {
    fd: OwnedFd,                 // from d_descriptor; closed on Drop
    door_id: Option<DoorId>,     // from d_id
    attributes: DescAttributes,
}
```

`SentFd` MUST always set `DOOR_DESCRIPTOR`. `DoorId` MUST be a distinct
type with no conversion to `RawFd`. No union may be exposed above
`doors-sys`.

### 6.4 Call errors

```rust
pub enum CallError {
    /// EFAULT and EBADF only. Not consumed; Released ones handed back.
    Rejected { returned: Vec<OwnedFd>, errno: Errno },
    /// Any other errno. The kernel consumed the descriptors.
    Consumed(Errno),
    /// Interrupted. The server may already have run. Descriptors consumed.
    Interrupted,
    /// The reply did not fit and the caller asked for no mapping.
    ReplyTooBig { needed: usize },
    /// §3.9 tag 1. The server procedure returned Err.
    Server { data: Vec<u8> },
    /// §3.9 tag 2. The server panicked or could not resolve its state.
    ServerFailed(ServerFault),
}
```

Descriptor handling, not optional:

1. Before `door_call`, convert every `Released` fd to a raw `int` with
   `into_raw_fd`, so the caller's `OwnedFd` no longer exists.
2. On success, `Consumed` and `Interrupted`, discard those raw fds
   without closing. The kernel closed them.
3. On `Rejected` only, re-wrap them as `OwnedFd` and return them in
   `returned`.
4. `Shared(BorrowedFd)` entries are never closed by the crate on any
   path.

Errno mapping: `EFAULT` and `EBADF` → `Rejected`; `EINTR` →
`Interrupted`; every other errno, documented or not → `Consumed`.

`call` and `call_into` MUST NOT retry `EINTR`. `call_idempotent` is the
only method that retries it, and its doc comment MUST say an interrupted
call may already have run.

### 6.5 Interop with peers that do not use this crate

**Required, both directions.** The crate MUST be able to call a door it
did not create, and MUST be able to serve a client that does not use
it. Doors are an operating system facility with existing users; a
binding that can only talk to itself is not a binding.

The §3.9 status tag is what gets in the way. It is a private convention
between two `doors` peers, and a C door server neither writes nor reads
it. So it becomes optional, and the choice is explicit on both sides.

Client:

```rust
impl<D> Client<D> {
    pub fn untagged(&self) -> Untagged<'_, D>;
}
```

`Untagged` MUST offer the same calls as `Client` — `call`,
`call_idempotent`, `call_into`, and `call_with_descriptors` on
`Untagged<'_, Descriptors>` — and MUST NOT parse a status byte.
`Reply::data()` is exactly what the server sent. Descriptor ownership,
mapping ownership and the `EINTR` rules are unchanged; only the reply
framing differs.

Server: `DoorBuilder::untagged()` and the `#[door(untagged)]` flag.

Untagged semantics, which MUST be documented on every method that has
them:

- `Ok(bytes)` → those bytes, exactly. No framing.
- `Err(e)` → the `ErrorReply` encoding, with **no marker**. The peer
  cannot tell it apart from success, because an untagged protocol has
  no error channel. A user who must signal failure to a foreign peer
  encodes it in their own reply format, exactly as a C door server
  does.
- A panic or an unresolvable cookie → a **zero-length** reply.
- `CallError::Server` and `CallError::ServerFailed` can never be
  produced on an untagged client call, because both come from tags.

Tagged remains the default, so two `doors` peers keep the richer
errors.

---

## 7. `fork` handling

Three layers, all required.

1. `FD_CLOEXEC` by default on every door fd the crate creates or opens,
   server and client, with an explicit per-door opt-out.
2. `doors::fork()` — a wrapper returning
   `ForkResult::{Parent { child }, Child}` that does the child-side
   cleanup.
3. A `pthread_atfork` backstop, registered once, lazily, on the first
   `Door` build.

### 7.1 The registry

A process-global `Mutex<Slab<Arc<DoorInner>>>` or equivalent.

- A `Door` joins when `build()` or `build_foo()` succeeds.
- A `Door` MUST leave the registry **before** its fd is closed.
  `Door::drop` and `Door::revoke()` MUST take the registry lock, remove
  the entry, close the fd, release the lock — in that order. Closing
  first would let a concurrent `fork` close a descriptor number
  something else has already reused.
- Deregistration happens even when `disowned` is set. §5.1's "none of
  those three" covers `door_revoke`, `fdetach` and `unlink` only.
- `Door::detach()` touches no fd and no registry entry.

### 7.2 The atfork handlers

`prepare` locks the registry, running in the still-multithreaded parent.
`parent` unlocks. `child` walks every entry doing
`disowned.store(true, Release)` then `close(fd)`, then unlocks.

The child handler MUST do only async-signal-safe work. Locking in
`prepare` is what makes it legal — the child inherits the registry
already locked and consistent, so it never acquires anything. An atomic
store and `close(2)` are permitted; allocation is not.

### 7.3 Also required

- Server doors MUST be disowned and closed in the child. Leaving the fd
  open changes when `DOOR_UNREF` fires in the parent.
- `Client` MUST be left alone on `fork`; a child may keep calling it.
- `DoorInner` MUST store the creating pid. `Door::drop` compares it with
  `getpid()` as a secondary check, because `vfork` and `forkall` do not
  run atfork handlers the same way.
- Do NOT add a cargo feature to disable the atfork handler. Document the
  `cdylib` / `dlclose` limitation in the crate docs instead.
- Document that a `Door` does not survive `fork`. A child that wants a
  door MUST create its own.

---

## 8. Test machines

Doors exist only on illumos. Development happens on macOS, so every
behavioural test runs on a disposable OmniOS VM.

### 8.1 `fart` — make the VMs

`fart` is the user's own tool at `/Users/davis/Projects/starcrash/fart`.
It spins disposable OmniOS bhyve guests as instant ZFS clones of a
golden template. Read `fart/vm.sh` before first use; its header block is
the reference.

Laptop-side driver, run from the `starcrash` repo root:

- `NIC=<uplink> sh fart/vm.sh bootstrap <HYP>` — install fart and the
  lab key on the hypervisor and build the boot image if missing.
  Idempotent. Run once.
- `NIC=<uplink> sh fart/vm.sh up <HYP> <name> [flavor]` — spin a fresh
  VM and echo its IP. `flavor` is `tools` or `zone`, default `tools`.
- `sh fart/vm.sh down <HYP> <name>` — destroy it.

`<HYP>` is the hypervisor host; `NIC` is its uplink, and there is no
safe default for either. On the hypervisor itself the engine is
`fart {up|down|ls|ip|status|nuke|build|build-status}`.

Rules:

- Every VM is disposable. Never fix a broken VM — `down` it and `up` a
  fresh one.
- `down` every VM you bring up, including on failure. `fart nuke` on the
  hypervisor destroys all fart VMs but never the permanent research
  guests.
- Check `fart status` for free RAM before spinning several. Each VM
  defaults to 2048 MB.
- A VM has an unprivileged `attacker` user with no sudo, plus key-based
  root ssh. Build and run tests as `attacker`; use root only where a
  test genuinely needs privilege.
- **Any ssh or scp to the hypervisor MUST be wrapped in a deadman
  timeout** (`timeout`, or `gtimeout` on macOS). An unwrapped remote
  call is blocked by a hook and leaves box-side processes hanging.

### 8.2 `rubber` — drive the VMs

`rubber` is the user's own MCP server at `/Users/davis/Projects/rubber`.
It operates on a remote host over SSH and exposes six tools: `exec`,
`read_file`, `write_file`, `edit_file`, `list_dir`, `search_files`. Use
it for the interactive edit-build-test loop on a VM instead of
hand-rolled ssh.

Register it once:

```sh
cd /Users/davis/Projects/rubber && cargo build --release
claude mcp add rubber --scope user -- \
    /Users/davis/Projects/rubber/target/release/rubber --config ./rubber.toml
```

Using it:

- Every call is stateless and carries its own `host` (IPv4), `user`,
  `cwd` and `timeout_secs`. There is no session and no server-held cwd,
  so pass `cwd` on every call.
- `host` is the IP that `vm.sh up` echoed. Confirm once, at bootstrap,
  that the VM is reachable from the laptop on port 22. If it is not,
  point `rubber` at the hypervisor and reach the VM from there with
  `exec`.
- Key-based auth only, port 22 only. First contact pins the host key; a
  changed key is refused. A fresh VM reusing an old IP will therefore be
  refused — clear that entry from `known_hosts` before retrying.
- A nonzero exit from `exec` is data, not an error. Read the outcome.
- `exec` output is capped per stream. Redirect long build logs to a file
  on the VM and `read_file` the interesting part.
- Every call is recorded in a local SQLite audit log, so a failing run
  can be reproduced exactly. Reference the audit row when reporting one.

### 8.3 Loop

1. Bring up one VM per parallel agent, named for the work.
2. `make vm-up` records the address in `.vm-ip`; every other `make`
   target reads it, rsyncs the worktree, and runs cargo **on the VM**.
   Nothing is built on the development machine.
3. Iterate with `rubber`, or with `make test` / `make test-loop`.
4. `down` the VM when the task ends, pass or fail.

---

## 9. Testing

### 9.1 Tests that need no live door

- Macro expansion unit tests inside `door-macros`, calling `expand`
  directly.
- `trybuild` compile-fail tests for every rejected input: unknown
  option, two shape keywords, `unref` without `on_unreferenced`,
  `#[door(rpc)]` without the `rpc` feature, and a builder call
  duplicating a `#[door]` option.
- Compile-fail tests proving the typestate: sending a descriptor through
  a `Client<NoDescriptors>` MUST NOT compile; calling `descriptors()` on
  a `Request<'_, NoDescriptors>` MUST NOT compile.
- Layout tests for `door_desc_t`, `door_arg_t`, `door_info_t`.

### 9.2 On a VM only

- Round-trip client and server tests for every shape in §3.3.
- `DOOR_UNREF` delivery, including that a forked child does not delay
  it.
- `Reply` unmaps an overflow mapping; `call_into` returns `ReplyTooBig`
  and leaves no mapping behind.
- `Door::drop` in a forked child does not remove the parent's door path.
- Descriptor round trip both directions, with and without
  `DOOR_RELEASE`.
- `Door::revoke()` waits for an in-flight call to finish.
- A panicking server procedure produces `CallError::ServerFailed` and
  the door still works for the next call.

### 9.3 Required experiment — DONE, see `experiments/`

Confirm a failed `door_return` does not consume descriptors, rather than
trusting the source reading:

1. Client sets `RLIMIT_NOFILE` low and fills its descriptor table.
2. Client calls a door whose server returns one `DOOR_RELEASE`
   descriptor.
3. Assert `door_return` fails.
4. Assert the server's descriptor is still open **and still refers to
   the same file** — compare `st_dev`/`st_ino`/`st_rdev`, not just
   `fcntl(fd, F_GETFD) != -1`, so a recycled descriptor number cannot
   pass for a survivor.

Repeat without `DOOR_RELEASE`.

**Result: rule 4.2.4 holds.** In every failure reachable from the safe
API the descriptor survived, same file. `experiments/README.md` has the
table and `experiments/matrix.c` reproduces it.

Three corrections the measurement forced, all confirmed on OmniOS
r151058:

- `door_return` returns `-1` but **leaves `errno` untouched**. It is
  not `EMFILE`. §2.1's "reads errno and returns it, never zero" is
  therefore unachievable; the wrapper falls back to `EINVAL` and
  documents that.
- The `E2BIG` case cannot be built as written.
  `DOOR_PARAM_DATA_MAX` caps **request** data, not replies, and `E2BIG`
  is not reachable by reply size either — replies up to 256 MiB
  succeed. Reply overflow is reported to the client's `door_call`,
  never back to the server.
- When the kernel cannot deliver a descriptor it reports **success with
  `desc_num=0` to the client** while returning `-1` to the server. A
  caller that needs a descriptor MUST check the count; success does not
  imply delivery.

---

## 10. Build order

Each step should compile and pass its own tests before the next begins.

1. `doors-sys` — complete FFI plus layout tests.
2. `Reply` and `Client` with guaranteed `munmap`.
3. The trampoline: `catch_unwind`, `ReplyBuf`, §3.9 tagging, the
   non-falling-through `door_return` sequence, and the §9.3 experiment.
   Drive it from a hand-written server procedure; the macro comes last.
4. `fork` handling, the registry, `FD_CLOEXEC` defaults, `EINTR` as its
   own variant. **MUST land before `Door` gains a teardown `Drop`.**
5. `DoorBuilder` with `door_setparam` and `door_xcreate`.
6. Cookie strategies and the `revoke()` drain.
7. Descriptor typestate, `ReceivedFd`, `SentFd`, `Request`'s typestate.
8. The unref hook.
9. `door-macros`, on top of all the above.

---

## 11. Decisions left to the implementer

Resolve during implementation and record the choice in `docs/DESIGN.md`.

1. Whether the `ReplyBuf` hard cap belongs on the `Door` or on each
   server procedure.
2. Whether `Door::revoke()` blocks forever on a hung server procedure or
   takes a timeout.
3. Whether `doors` re-exports `doors-sys` publicly. Today
   `doors::illumos` is public; keeping it freezes the raw layer as
   public API. `doors::Errno` is re-exported either way.

---

## 12. Invariants

Breaking one of these is wrong regardless of what else it achieves.

1. No safe API may call `door_return`.
2. No trampoline may fall off its end.
3. No value with a destructor may be alive when `door_return` is called.
4. No panic may unwind across an `extern "C"` boundary.
5. `Reply` always unmaps.
6. `Door` and `Client` never expose their raw file descriptor.
7. `DOOR_NO_CANCEL` is always set.
8. A forked child never tears down its parent's door or door path.
9. `EINTR` is never retried without the caller asking for it by name.
10. No `door_desc_t` union is visible above `doors-sys`.
11. A door leaves the fork registry before its descriptor is closed.
12. Every test VM brought up is brought back down.
13. The crate can call a foreign door, and serve a foreign client.

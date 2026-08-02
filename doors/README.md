# doors
![](https://github.com/robertdfrench/rusty-doors/raw/HEAD/etc/social_media_preview.jpg)

A Rust interface for the [illumos][1] [Doors API][2].

## What is a door?

A door is a file-like way for two processes on the same machine to
talk, a bit like a named pipe or a UNIX domain socket. A client calls a
door; the kernel runs a *server procedure* in the server process on the
calling thread's behalf and comes straight back, without giving up the
CPU. When latency matters they beat both.

A *server procedure* is the function a door invocation lands in. A
*door server* is a process that has made one of its server procedures
available as a door on the filesystem.

## Calling a door

```rust
use doors::Client;

let client = Client::open("/var/run/my_door")?;
let reply = client.call(b"ping")?;
println!("{}", String::from_utf8_lossy(reply.data()));
```

For a door this crate did not create — one written in C, say — use
`client.untagged().call(...)`, which passes the reply through exactly
as it arrived.

## Serving a door

```rust
use doors::{Door, NoDescriptors, Request};

struct Greeter { greeting: String }

#[doors::server]
impl Greeter {
    #[door(refuse_desc)]
    fn hello(&self, req: Request<'_, NoDescriptors>)
        -> Result<Vec<u8>, std::io::Error>
    {
        let who = String::from_utf8_lossy(req.data());
        Ok(format!("{}, {who}!", self.greeting).into_bytes())
    }
}

let mut door = Door::builder(Greeter { greeting: "hello".into() })
    .thread_stack_size(256 * 1024)
    .build_hello()?;
door.attach("/var/run/my_door")?;
```

Add `#[door(untagged)]` when the clients are not using this crate.

## Platform

illumos only. This crate does not build anywhere else and does not try
to. See the [repository README][4] for how the tests are run.

## Crates

- [`doors-sys`][5] — the raw C surface, if you want to work below this
  layer.
- [`door-macros`][6] — the macro implementation. Not useful on its own;
  this crate re-exports it.

<!-- REFERENCES -->
[1]: https://illumos.org/
[2]: https://illumos.org/man/3C/door_create
[4]: https://github.com/robertdfrench/rusty-doors
[5]: https://crates.io/crates/doors-sys
[6]: https://crates.io/crates/door-macros

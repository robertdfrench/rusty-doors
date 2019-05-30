# Rusty Doors
*Rust is great. Doors are great. Putting them together is not yet great.*

The goal of this crate is to expose the [illumos][1] [Doors API][2] in Rust. Eventually, the unsafe parts of this api should be merged into the "[solarish][3]" section of the libc crate. However, I think Rust will allow for some very expressive, safe ways to work with doors, and that is what I would like this crate to focus on in the long term.

### What we have currently
```rust
/* Server process */

doorfn!(Server() {
	/* Do some great server stuff */
});

fn main() {
	let path = "server.door";
	match Server::attach_to("server.door") {
		None => panic!("Could not prepare a door on the filesystem"),
		Some(_d) => {
			let x = time::Duration::from_millis(1000 * 360);
			thread::sleep(x);
		}
	}
}
```

### What we want
```rust
#[doorfn]
fn Server(data: vec<MyType>, descriptors: vec<Descriptor>, cookie: i32) {
	/* Do some great server stuf */
}

fn main() {
	Server::attach_to("server.door")?;
	door::await_clients(); 
}
```

and in the client:
```rust
fn main() {
	let server = File::open("server.door")?.into_door();
	server.call()?;
}
```

[1]: https://illumos.org/
[2]: https://github.com/robertdfrench/revolving-door
[3]: https://github.com/rust-lang/libc/tree/master/src/unix/solarish

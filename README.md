# Rusty Doors
*Rust is great. Doors are great. Putting them together is not yet great.*

What would be great:
```rust
/* Server process */
fn server(data: &mut Vec<T>, descriptors: &mut Vec<Descriptors>) {
	/* Do some great stuff here */
}

fn main() {
	door::create(server, "/tmp/server.door");
	sleep();
}

/* Client process */
fn main() {
	let door = door::open("/tmp/server.door");
	/* fill data and descriptors with some great stuff */
	door.call(data, descriptors)
```

What we have currently (not yet great):
```rust
/* Server process */

fn server() {
	println!("I was invoked");
}

doorfn!(server);

fn main() {
	let path = "server.door";
	match door::server_safe_open(path) {
		None => panic!("Could not prepare a door on the filesystem"),
		Some(_file) => {
			match door::create_at(doors::server, path) {
				None => panic!("Could not create a door"),
				Some(_d) => {
					let x = time::Duration::from_millis(1000 * 360);
					thread::sleep(x);
				}
			}
		}
	}
}
```

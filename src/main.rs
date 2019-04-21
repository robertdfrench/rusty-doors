use std::env;
use std::{thread, time};
use std::fs::File;

mod door;

fn client() {
	match File::open("server.door") {
		Ok(file) => {
			let door = door::from(file);
			if !door.call() {
				panic!("Could not call door bud");
			}
		}
		Err(_e) => panic!("No such door bud")
	}
}

fn server() {
	let path = "server.door";
	match door::server_safe_open(path) {
		None => panic!("Could not prepare a door on the filesystem"),
		Some(_file) => {
			match door::create_at(path) {
				None => panic!("Could not create a door"),
				Some(_d) => {
					let x = time::Duration::from_millis(1000 * 360);
					thread::sleep(x);
				}
			}
		}
	}
}

fn main() {
	match env::var("SERVER") {
		Ok(_val) => server(),
		Err(_e) => client(),
	}
}


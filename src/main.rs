use std::env;
use std::{thread, time};
use std::fs::File;

#[macro_use]
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

doorfn!(Answer() {
	println!("I am a normal ass Rust function");
});

fn server() {
	let path = "server.door";
	match Answer::attach_to(path) {
		None => panic!("Could not create a door"),
		Some(_d) => {
			println!("Door has been attached!");
			let x = time::Duration::from_millis(1000 * 360);
			thread::sleep(x);
		}
	}
}

fn main() {
	match env::var("SERVER") {
		Ok(_val) => server(),
		Err(_e) => client(),
	}
}


all: about build format test

about:
	@banner about
	@cargo --version
	@rustc --version
	@uname -a

build:
	@banner build
	ptime -m cargo build
	ptime -m cargo build --release

buildomat: all

format:
	@banner format
	cargo fmt -- --check
	cargo clippy

test:
	@banner test
	true \
		&& (cargo run --example barebones_server &) \
		&& sleep 1 \
		&& cargo test \
		&& wait

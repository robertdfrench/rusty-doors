help: ##: Print this help menu
	@echo "USAGE"
	@awk -F':' '/##:/ && !/awk/ { OFS="\t"; print "make "$$1,$$3 }' Makefile \
		| sort

about: ##: Print version information
	@banner about
	@cargo --version
	@rustc --version
	@uname -a

all: about build format test docs ##: Run the full build pipeline

build: ##: Build debug and release binaries
	@banner build
	ptime -m cargo build
	ptime -m cargo build --release

docs: ##: Build documentation
	@banner docs
	cargo doc

format: ##: Check for code formatting issues
	@banner format
	cargo fmt -- --check
	cargo clippy

.git/hooks/pre-commit:
	echo "#!/bin/bash\nmake all" > .git/hooks/pre-commit
	chmod +x .git/hooks/pre-commit

hook: .git/hooks/pre-commit ##: Run 'make all' as a pre-commit hook

launch: ##: Generate launch script for the example servers
	@find examples -type f -name '*_server.rs' \
		| xargs -n1 basename \
		| cut -f1 -d'.' \
		| xargs -n1 -Iy echo 'cargo run --example y &'

test: ##: Run tests against the example servers
	@banner test
	true \
		&& eval `make launch` \
		&& sleep 1 \
		&& cargo test \
		&& wait

banner=printf "\033[35m"; banner $@; print "\033[0m";

help: ##: Print this help menu
	@echo "USAGE"
	@awk -F':' '/##:/ && !/awk/ { OFS="\t"; print "make "$$1,$$3 }' Makefile \
		| sort

about: ##: Print version information
	@$(banner)
	@cargo --version
	@rustc --version
	@uname -a

all: about build format test docs ##: Run the full build pipeline

build: ##: Build debug and release binaries
	@$(banner)
	cargo build
	cargo build --tests
	cargo build --examples

docs: ##: Build documentation
	@$(banner)
	cargo doc

format: ##: Check for code formatting issues
	@$(banner)
	cargo fmt -- --check
	cargo clippy

.git/hooks/pre-commit:
	echo "#!/bin/bash\nmake all" > .git/hooks/pre-commit
	chmod +x .git/hooks/pre-commit

hook: .git/hooks/pre-commit ##: Run 'make all' as a pre-commit hook

launch: ##: Generate launch script for the example servers
	@find doors/examples -type f -name '*_server.rs' \
		| sed 's,doors,target/debug,; s/\.rs//' \
		| xargs -n1 -Ix echo './x &'

publish: ##: Publish all crates in this workspace to crates.io
	@$(banner)
	cargo publish --package door-macros
	cargo publish --package doors

test: build ##: Run tests against the example servers
	@$(banner)
	true \
		&& eval `make launch` \
		&& sleep 1 \
		&& cargo test \
		&& wait

help: ##: Print this help menu
	@echo "USAGE"
	@awk -F':' '/##:/ && !/awk/ { OFS="\t"; print "make "$$1,$$3 }' Makefile

about: ##: Print version information
	@banner about
	@cargo --version
	@rustc --version
	@uname -a

build: ##: Build debug and release binaries
	@banner build
	ptime -m cargo build
	ptime -m cargo build --release

cicd: about build format test ##: Run the full build pipeline

format: ##: Check for code formatting issues
	@banner format
	cargo fmt -- --check
	cargo clippy

.git/hooks/pre-commit:
	echo "#!/bin/bash\nmake cicd" > .git/hooks/pre-commit
	chmod +x .git/hooks/pre-commit

hook: .git/hooks/pre-commit ##: Run 'make cicd' as a pre-commit hook


test: ##: Run tests against the example servers
	@banner test
	true \
		&& (cargo run --example barebones_server &) \
		&& sleep 1 \
		&& cargo test \
		&& wait

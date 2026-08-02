# `banner` is not on every system, and `print` is a ksh builtin that
# /bin/sh on a Mac does not have. Keep this portable.
banner=printf "\033[35m== %s ==\033[0m\n" $@;

# Doors are an illumos facility. This crate is not portable and does not
# try to be: it does not compile anywhere else, and there is nothing
# useful to learn from building it on a laptop. Every target here that
# touches cargo runs it on an illumos machine.
#
# This Makefile does NOT create that machine. Bring one up yourself and
# pass its address:
#
#     ssh root@omnios-big beekeeper up doors --ready usable
#     make test TARGET=<the address it printed>
#     ssh root@omnios-big beekeeper down doors
#
# Any illumos host with a rust toolchain will do; beekeeper is just a
# convenient way to get a disposable one. See `ssh root@<hyp> beekeeper
# help`.

TARGET ?=
USER_ON_TARGET ?= attacker
REMOTE ?= /home/$(USER_ON_TARGET)/rusty-doors

# The private key for the lab guests. beekeeper serves it:
#     ssh root@omnios-big beekeeper labkey > ~/.ssh/beekeeper-labkey
#     chmod 600 ~/.ssh/beekeeper-labkey
# `make labkey HYP=<host>` does that for you.
KEY ?= $(HOME)/.ssh/beekeeper-labkey
HYP ?= omnios-big

# Guests are reached THROUGH the hypervisor by default. The direct
# route to the lab network is not dependable from a development
# machine -- it has disappeared mid-session -- and the hypervisor
# always is, since it is where beekeeper runs. Set JUMP= (empty) to
# connect straight to the guest instead.
JUMP ?= root@$(HYP)
PROXY = $(if $(JUMP),-o ProxyJump=$(JUMP),)

# Lab guests are disposable and get a new address each time, so pinning
# host keys would only ever produce false alarms.
SSH = ssh -o BatchMode=yes -i $(KEY) -o IdentitiesOnly=yes \
	-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
	-o ConnectTimeout=12 $(PROXY)

# Every remote call is wrapped in a deadman timeout.
TIMEOUT = $(shell command -v timeout 2>/dev/null || command -v gtimeout)

# cargo lands in /opt/ooce/bin, which a non-login shell does not pick up.
# `command -v cargo` first: without it, a missing toolchain makes every
# grep-based check match nothing, and a broken run looks perfectly clean.
CARGO_ENV = export PATH=/opt/ooce/bin:$$PATH; \
	command -v cargo >/dev/null || { echo "no cargo on $(TARGET)" >&2; exit 1; };

help: ##: Print this help menu
	@echo "USAGE   (most targets need TARGET=<address>)"
	@awk -F':' '/##:/ && !/awk/ { OFS="\t"; print "make "$$1,$$3 }' Makefile \
		| sort

require-target:
	@test -n "$(TARGET)" || { \
		echo "Set TARGET=<address> of an illumos host." >&2; \
		echo "  ssh root@$(HYP) beekeeper up doors --ready usable" >&2; \
		exit 1; }

labkey: ##: Fetch the lab private key from the hypervisor into $(KEY)
	@$(banner)
	@$(TIMEOUT) 60 ssh -o BatchMode=yes -o ConnectTimeout=8 root@$(HYP) \
		beekeeper labkey > $(KEY)
	@chmod 600 $(KEY)
	@echo "wrote $(KEY)"

about: require-target ##: Print version information from the target
	@$(banner)
	@$(TIMEOUT) 60 $(SSH) $(USER_ON_TARGET)@$(TARGET) \
		'$(CARGO_ENV) cargo --version; rustc --version; uname -a'

sync: require-target ##: Copy the worktree to the target
	@$(TIMEOUT) 300 rsync -az --delete \
		--exclude target/ --exclude .git/ \
		-e "$(SSH)" ./ $(USER_ON_TARGET)@$(TARGET):$(REMOTE)/

build: sync ##: Build the workspace on the target
	@$(banner)
	@$(TIMEOUT) 900 $(SSH) $(USER_ON_TARGET)@$(TARGET) \
		'$(CARGO_ENV) cd $(REMOTE) && cargo build --workspace --all-targets'

test: sync ##: Run the whole test suite on the target
	@$(banner)
	@$(TIMEOUT) 900 $(SSH) $(USER_ON_TARGET)@$(TARGET) \
		'$(CARGO_ENV) cd $(REMOTE) && cargo test --workspace'

test-loop: sync ##: Run the suite N times to catch flaky failures (N=20)
	@$(banner)
	@$(TIMEOUT) 3000 $(SSH) $(USER_ON_TARGET)@$(TARGET) \
		'$(CARGO_ENV) cd $(REMOTE); p=0; f=0; \
		 for i in $$(seq $(or $(N),20)); do \
		   if cargo test --workspace >/tmp/run.log 2>&1; then p=$$((p+1)); \
		   else f=$$((f+1)); grep -E "panicked at|^error" /tmp/run.log | head -3; \
		   fi; \
		 done; echo "pass=$$p fail=$$f"'

format: sync ##: Check formatting and lints on the target
	@$(banner)
	@$(TIMEOUT) 600 $(SSH) $(USER_ON_TARGET)@$(TARGET) \
		'$(CARGO_ENV) cd $(REMOTE) && cargo fmt --all -- --check \
		 && cargo clippy --workspace --all-targets --features rpc'

docs: sync ##: Build documentation on the target
	@$(banner)
	@$(TIMEOUT) 600 $(SSH) $(USER_ON_TARGET)@$(TARGET) \
		'$(CARGO_ENV) cd $(REMOTE) && cargo doc --workspace --no-deps'

examples: sync ##: Build the example servers on the target
	@$(banner)
	@$(TIMEOUT) 900 $(SSH) $(USER_ON_TARGET)@$(TARGET) \
		'$(CARGO_ENV) cd $(REMOTE) && cargo build --examples --features rpc'

shell: require-target ##: Open a shell on the target in the synced worktree
	@$(SSH) -t $(USER_ON_TARGET)@$(TARGET) \
		'export PATH=/opt/ooce/bin:$$PATH; cd $(REMOTE); exec bash -l'

all: about build format test docs ##: Run the full pipeline on the target

publish: ##: Publish all crates in this workspace to crates.io
	@$(banner)
	cargo publish --package doors-sys
	cargo publish --package door-macros
	cargo publish --package doors

.git/hooks/pre-commit:
	echo "#!/bin/bash\nmake all" > .git/hooks/pre-commit
	chmod +x .git/hooks/pre-commit

hook: .git/hooks/pre-commit ##: Run 'make all' as a pre-commit hook

.PHONY: help require-target labkey about sync build test test-loop format \
	docs examples shell all publish hook

# --- formal models --------------------------------------------------
#
# TLA+ specs for the parts where interleaving is the danger and testing
# cannot reach. See specs/README.md.

specs/tla2tools.jar:
	curl -fSL -o $@ https://github.com/tlaplus/tlaplus/releases/download/v1.8.0/tla2tools.jar

%.check: specs/tla2tools.jar ##: Model-check a TLA+ spec, e.g. make specs/ForkRegistry.check
	@$(banner)
	@cd specs && java -XX:+UseParallelGC -cp tla2tools.jar tlc2.TLC $(notdir $*).tla

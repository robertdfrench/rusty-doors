# `banner` is not on every system, and `print` is a ksh builtin that
# /bin/sh on a Mac does not have. Keep this portable.
banner=printf "\033[35m== %s ==\033[0m\n" $@;

# Doors are an illumos facility. This crate is not portable and does not
# try to be: it does not compile anywhere else, and there is nothing
# useful to learn from building it on a laptop. Every target below that
# touches cargo runs it on the VM.
#
# See GOALS.md section 8. `make vm-up` records the VM's address in
# .vm-ip, and everything after that reads it, so you rarely pass VM_IP
# by hand.

HYP ?= omnios-big
NIC ?= e1000g1
VM_NAME ?= doors
VM_USER ?= attacker
KEY ?= ../starcrash/keys/labkey
REMOTE ?= /home/$(VM_USER)/rusty-doors
VM_IP ?= $(shell cat .vm-ip 2>/dev/null)

# Fresh VMs are disposable and get a new address each time, so pinning
# host keys would only ever produce false alarms.
SSH = ssh -o BatchMode=yes -i $(KEY) -o IdentitiesOnly=yes \
	-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
	-o ConnectTimeout=12

# Every remote call is wrapped in a deadman timeout (GOALS.md 8.1).
TIMEOUT = $(shell command -v timeout 2>/dev/null || command -v gtimeout)

# cargo lands in /opt/ooce/bin, which a non-login shell does not pick up.
CARGO_ENV = export PATH=/opt/ooce/bin:$$PATH;

help: ##: Print this help menu
	@echo "USAGE"
	@awk -F':' '/##:/ && !/awk/ { OFS="\t"; print "make "$$1,$$3 }' Makefile \
		| sort

require-vm:
	@test -n "$(VM_IP)" || { \
		echo "No VM. Run 'make vm-up' first, or pass VM_IP=<addr>." >&2; \
		exit 1; }

about: require-vm ##: Print version information from the VM
	@$(banner)
	@$(TIMEOUT) 60 $(SSH) $(VM_USER)@$(VM_IP) \
		'$(CARGO_ENV) cargo --version; rustc --version; uname -a'

sync: require-vm ##: Copy the worktree to the VM
	@$(TIMEOUT) 300 rsync -az --delete \
		--exclude target/ --exclude .git/ --exclude .vm-ip \
		-e "$(SSH)" ./ $(VM_USER)@$(VM_IP):$(REMOTE)/

build: sync ##: Build the workspace on the VM
	@$(banner)
	@$(TIMEOUT) 900 $(SSH) $(VM_USER)@$(VM_IP) \
		'$(CARGO_ENV) cd $(REMOTE) && cargo build --workspace --all-targets'

test: sync ##: Run the whole test suite on the VM
	@$(banner)
	@$(TIMEOUT) 900 $(SSH) $(VM_USER)@$(VM_IP) \
		'$(CARGO_ENV) cd $(REMOTE) && cargo test --workspace'

test-loop: sync ##: Run the suite N times on the VM to catch flaky failures (N=20)
	@$(banner)
	@$(TIMEOUT) 3000 $(SSH) $(VM_USER)@$(VM_IP) \
		'$(CARGO_ENV) cd $(REMOTE); p=0; f=0; \
		 for i in $$(seq $(or $(N),20)); do \
		   if cargo test --workspace >/tmp/run.log 2>&1; then p=$$((p+1)); \
		   else f=$$((f+1)); grep -E "panicked at|^error" /tmp/run.log | head -3; \
		   fi; \
		 done; echo "pass=$$p fail=$$f"'

format: sync ##: Check formatting and lints on the VM
	@$(banner)
	@$(TIMEOUT) 600 $(SSH) $(VM_USER)@$(VM_IP) \
		'$(CARGO_ENV) cd $(REMOTE) && cargo fmt --all -- --check \
		 && cargo clippy --workspace --all-targets'

docs: sync ##: Build documentation on the VM
	@$(banner)
	@$(TIMEOUT) 600 $(SSH) $(VM_USER)@$(VM_IP) \
		'$(CARGO_ENV) cd $(REMOTE) && cargo doc --workspace --no-deps'

shell: require-vm ##: Open a shell on the VM in the synced worktree
	@$(SSH) -t $(VM_USER)@$(VM_IP) '$(CARGO_ENV) cd $(REMOTE); exec bash -l'

all: about build format test docs ##: Run the full pipeline on the VM

# --- VM lifecycle ----------------------------------------------------

vm-up: ##: Spin a fresh illumos VM, install rust, record its IP
	@$(banner)
	@cd ../starcrash && NIC='$(NIC)' sh fart/vm.sh up '$(HYP)' '$(VM_NAME)' \
		| tail -1 > $(CURDIR)/.vm-ip
	@echo "VM at $$(cat .vm-ip)"
	@$(TIMEOUT) 900 $(SSH) root@$$(cat .vm-ip) \
		'pkg install -q ooce/developer/rust || true'
	@$(MAKE) --no-print-directory about

vm-down: ##: Destroy the VM. Always do this when you are finished.
	@$(banner)
	@cd ../starcrash && NIC='$(NIC)' sh fart/vm.sh down '$(HYP)' '$(VM_NAME)'
	@rm -f .vm-ip

vm-status: ##: List the fart VMs on the hypervisor
	@$(TIMEOUT) 60 ssh -o BatchMode=yes -o ConnectTimeout=8 \
		root@$(HYP) 'fart status; fart ls'

# --- release ---------------------------------------------------------

publish: ##: Publish all crates in this workspace to crates.io
	@$(banner)
	cargo publish --package doors-sys
	cargo publish --package door-macros
	cargo publish --package doors

.git/hooks/pre-commit:
	echo "#!/bin/bash\nmake all" > .git/hooks/pre-commit
	chmod +x .git/hooks/pre-commit

hook: .git/hooks/pre-commit ##: Run 'make all' as a pre-commit hook

.PHONY: help require-vm about sync build test test-loop format docs shell \
	all vm-up vm-down vm-status publish hook

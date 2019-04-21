SHELL=bash

test: target/debug/webhinge clean server.door
	./$<

server.door: target/debug/webhinge
	SERVER=1 ./$< &
	sleep 1
	[ -e $@ ]

target/debug/webhinge: $(wildcard src/*.rs)
	cargo build

clean:
	pkill target/debug/webhinge || true
	rm -rf server.pid server.door

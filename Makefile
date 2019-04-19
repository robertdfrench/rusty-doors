test: target/debug/webhinge
	./$<

target/debug/webhinge: $(wildcard src/*.rs)
	cargo build

clean:
	rm -rf target

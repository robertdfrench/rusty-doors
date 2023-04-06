## Example Servers (for Unit Tests)

Unit tests are written from the perspective of a door client, and each
one expects a running door server. Before the tests are run, each of
these example programs is spawned, and each starts its own door server,
answering client calls as they come in.


### Running the Examples

Generally, you don't need to. Running `make test` will automatically
spawn all of these example door servers with a short timeout -- just
long enough to run the test suite.

When debugging a specific server/client interaction, an individual
example server can of course be launched by calling `cargo run --example
NAME`.  More interestingly, the examples can be launched en masse by
running `eval $(make launch)` in your shell.

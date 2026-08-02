/*
 * A door server that has never heard of this crate.
 *
 * Plain C, plain door_create/door_return, no status byte and no
 * framing of any kind -- exactly what an existing illumos door server
 * looks like. doors/tests/interop.rs builds this, runs it, and calls
 * it from Rust, which is the only honest way to show that the crate
 * can talk to doors it did not create.
 *
 * The reply is the request reversed, because that is easy to assert on
 * and impossible to produce by accident.
 *
 *   gcc -m64 -Wall -o c_server c_server.c
 *   ./c_server /tmp/some_door
 */
#include <door.h>
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#define	MAXREPLY	4096

/* ARGSUSED */
static void
server_proc(void *cookie, char *argp, size_t arg_size, door_desc_t *dp,
    uint_t n_desc)
{
	char reply[MAXREPLY];
	size_t i, n;

	n = arg_size;
	if (n > sizeof (reply))
		n = sizeof (reply);

	for (i = 0; i < n; i++)
		reply[i] = argp[n - 1 - i];

	/*
	 * The bytes go out on their own. A C door server has no status
	 * byte to write, which is the whole reason the Rust side needs
	 * an untagged mode to read this.
	 */
	(void) door_return(reply, n, NULL, 0);

	/* Only reachable if door_return failed. Do not fall off the end. */
	(void) door_return(NULL, 0, NULL, 0);
	_exit(1);
}

int
main(int argc, char **argv)
{
	const char *path;
	int did, fd;

	if (argc < 2) {
		(void) fprintf(stderr, "usage: %s <door-path>\n", argv[0]);
		return (2);
	}
	path = argv[1];

	(void) fdetach(path);
	(void) unlink(path);

	/* fattach needs the path to exist first. */
	fd = creat(path, 0600);
	if (fd < 0) {
		(void) fprintf(stderr, "creat %s: %s\n", path,
		    strerror(errno));
		return (1);
	}
	(void) close(fd);

	did = door_create(server_proc, NULL, DOOR_REFUSE_DESC);
	if (did < 0) {
		(void) fprintf(stderr, "door_create: %s\n", strerror(errno));
		return (1);
	}
	if (fattach(did, path) < 0) {
		(void) fprintf(stderr, "fattach %s: %s\n", path,
		    strerror(errno));
		return (1);
	}

	/*
	 * Tell the test we are ready. It waits for this line rather than
	 * sleeping, so the test is not racing the door coming up.
	 */
	(void) printf("ready\n");
	(void) fflush(stdout);

	/* Serve until killed. */
	for (;;)
		(void) pause();
}

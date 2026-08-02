/*
 * GOALS.md 9.3 case 1, take 2.
 *
 * Take 1 showed the descriptor surviving, but reported errno==0 after
 * door_return and EINTR at the client, neither of which the man page
 * leads you to expect. This version:
 *
 *   - records door_return's actual return value, not just errno;
 *   - runs the client as a separate exec'd process, so nothing is
 *     inherited across fork;
 *   - repeats the call, to show the door still works afterwards;
 *   - reports whether the returned fd is still open AND still refers
 *     to the same file, so a recycled descriptor number cannot pass
 *     for a surviving one.
 *
 * Build: gcc -m64 -Wall -o emfile2 emfile2.c
 * Run:   ./emfile2 server   (forks and execs ./emfile2 client)
 */
#include <door.h>
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/resource.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

static const char *DOOR_PATH = "/tmp/door_ret_emfile2";
static const char *SELF = "./emfile2";

static int calls;

/* ARGSUSED */
static void
server_proc(void *cookie, char *argp, size_t asz, door_desc_t *dp,
    uint_t ndesc)
{
	door_desc_t d;
	struct stat before, after;
	int fd, rc, saved;

	calls++;

	fd = open("/dev/null", O_RDONLY);
	if (fd < 0) {
		fprintf(stderr, "RESULT setup_failed open: %s\n",
		    strerror(errno));
		fflush(stderr);
		_exit(2);
	}
	if (fstat(fd, &before) != 0) {
		fprintf(stderr, "RESULT setup_failed fstat\n");
		fflush(stderr);
		_exit(2);
	}

	(void) memset(&d, 0, sizeof (d));
	d.d_attributes = DOOR_DESCRIPTOR | DOOR_RELEASE;
	d.d_data.d_desc.d_descriptor = fd;

	fprintf(stderr, "server[call %d]: returning fd %d DOOR_RELEASE\n",
	    calls, fd);
	fflush(stderr);

	errno = 0;
	rc = door_return(NULL, 0, &d, 1);
	saved = errno;

	/* Only reachable because door_return failed. */
	fprintf(stderr, "RESULT call=%d door_return_rc=%d errno=%d (%s)\n",
	    calls, rc, saved, saved == 0 ? "errno not set" : strerror(saved));

	errno = 0;
	if (fcntl(fd, F_GETFD) == -1) {
		fprintf(stderr, "RESULT call=%d fd_still_open=0 errno=%d\n",
		    calls, errno);
	} else if (fstat(fd, &after) != 0) {
		fprintf(stderr, "RESULT call=%d fd_still_open=1 fstat_failed\n",
		    calls);
	} else if (before.st_dev == after.st_dev &&
	    before.st_ino == after.st_ino && before.st_rdev == after.st_rdev) {
		fprintf(stderr, "RESULT call=%d fd_still_open=1 same_file=1\n",
		    calls);
	} else {
		fprintf(stderr, "RESULT call=%d fd_still_open=1 same_file=0 "
		    "(descriptor number was recycled)\n", calls);
	}
	fflush(stderr);

	(void) close(fd);

	/* Do not fall off the end: return empty, then give up. */
	(void) door_return(NULL, 0, NULL, 0);
	fprintf(stderr, "RESULT call=%d second_door_return_also_failed "
	    "errno=%d\n", calls, errno);
	fflush(stderr);
	_exit(0);
}

static int
run_client(void)
{
	struct rlimit rl;
	door_arg_t arg;
	int dfd, n;

	dfd = open(DOOR_PATH, O_RDONLY);
	if (dfd < 0) {
		fprintf(stderr, "client: open door: %s\n", strerror(errno));
		return (3);
	}

	rl.rlim_cur = 32;
	rl.rlim_max = 32;
	if (setrlimit(RLIMIT_NOFILE, &rl) != 0) {
		fprintf(stderr, "client: setrlimit: %s\n", strerror(errno));
		return (3);
	}

	n = 0;
	while (open("/dev/null", O_RDONLY) >= 0)
		n++;
	fprintf(stderr, "client: table full after %d extra fds (errno %d)\n",
	    n, errno);

	(void) memset(&arg, 0, sizeof (arg));
	errno = 0;
	if (door_call(dfd, &arg) < 0) {
		fprintf(stderr, "RESULT client_door_call_errno=%d (%s)\n",
		    errno, strerror(errno));
	} else {
		fprintf(stderr, "RESULT client_door_call=ok desc_num=%u\n",
		    arg.desc_num);
	}
	fflush(stderr);
	return (0);
}

int
main(int argc, char **argv)
{
	int did, status;
	pid_t pid;

	if (argc > 1 && strcmp(argv[1], "client") == 0)
		return (run_client());

	(void) fdetach(DOOR_PATH);
	(void) unlink(DOOR_PATH);
	(void) close(creat(DOOR_PATH, 0600));

	did = door_create(server_proc, NULL, DOOR_NO_CANCEL);
	if (did < 0) {
		fprintf(stderr, "door_create: %s\n", strerror(errno));
		return (1);
	}
	if (fattach(did, DOOR_PATH) < 0) {
		fprintf(stderr, "fattach: %s\n", strerror(errno));
		return (1);
	}

	pid = fork();
	if (pid == 0) {
		(void) execl(SELF, SELF, "client", (char *)NULL);
		fprintf(stderr, "execl: %s\n", strerror(errno));
		_exit(4);
	}
	(void) waitpid(pid, &status, 0);

	(void) sleep(1);
	fprintf(stderr, "server: %d call(s) handled\n", calls);
	(void) fdetach(DOOR_PATH);
	(void) unlink(DOOR_PATH);
	return (0);
}

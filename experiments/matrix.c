/*
 * GOALS.md 9.3, full matrix.
 *
 * For each failure mode we can provoke from the safe API, record:
 *   - what door_return returned and what errno it left behind;
 *   - whether the server's descriptor survived, and still refers to
 *     the same file (st_dev/st_ino/st_rdev), so a recycled descriptor
 *     number cannot masquerade as a survivor;
 *   - what the client saw.
 *
 * Cases:
 *   release_emfile  DOOR_DESCRIPTOR|DOOR_RELEASE, client table full
 *   shared_emfile   DOOR_DESCRIPTOR only,         client table full
 *   bigdata         reply larger than the client's rbuf and than
 *                   DOOR_PARAM_DATA_MAX, descriptor attached
 *   ok             control: nothing wrong, door_return should not
 *                   return at all
 *
 * Build: gcc -m64 -Wall -o matrix matrix.c
 * Run:   ./matrix <case>
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

static const char *DOOR_PATH = "/tmp/door_ret_matrix";
static const char *SELF = "./matrix";
static const char *mode;

#define	BIGSZ	(512 * 1024)

/* ARGSUSED */
static void
server_proc(void *cookie, char *argp, size_t asz, door_desc_t *dp,
    uint_t ndesc)
{
	door_desc_t d;
	struct stat before, after;
	static char big[BIGSZ];
	char *data = NULL;
	size_t dsz = 0;
	int fd, rc, saved;

	fd = open("/dev/null", O_RDONLY);
	if (fd < 0 || fstat(fd, &before) != 0) {
		fprintf(stderr, "RESULT setup_failed\n");
		fflush(stderr);
		_exit(2);
	}

	(void) memset(&d, 0, sizeof (d));
	d.d_attributes = DOOR_DESCRIPTOR;
	if (strcmp(mode, "shared_emfile") != 0)
		d.d_attributes |= DOOR_RELEASE;
	d.d_data.d_desc.d_descriptor = fd;

	if (strcmp(mode, "bigdata") == 0) {
		(void) memset(big, 'x', sizeof (big));
		data = big;
		dsz = sizeof (big);
	}

	fprintf(stderr, "server: fd=%d attrs=0x%x data=%zu\n",
	    fd, d.d_attributes, dsz);
	fflush(stderr);

	errno = 0;
	rc = door_return(data, dsz, &d, 1);
	saved = errno;

	fprintf(stderr, "RESULT door_return_rc=%d errno=%d (%s)\n",
	    rc, saved, saved == 0 ? "ERRNO NOT SET" : strerror(saved));

	errno = 0;
	if (fcntl(fd, F_GETFD) == -1) {
		fprintf(stderr, "RESULT fd_survived=0 fcntl_errno=%d\n", errno);
	} else if (fstat(fd, &after) == 0 && before.st_dev == after.st_dev &&
	    before.st_ino == after.st_ino && before.st_rdev == after.st_rdev) {
		fprintf(stderr, "RESULT fd_survived=1 same_file=1\n");
	} else {
		fprintf(stderr, "RESULT fd_survived=1 same_file=0\n");
	}
	fflush(stderr);

	(void) close(fd);
	(void) door_return(NULL, 0, NULL, 0);
	fprintf(stderr, "RESULT recovery_door_return_failed errno=%d\n",
	    errno);
	fflush(stderr);
	_exit(0);
}

static int
run_client(void)
{
	struct rlimit rl;
	door_arg_t arg;
	char rbuf[64];
	int dfd, n;

	dfd = open(DOOR_PATH, O_RDONLY);
	if (dfd < 0) {
		fprintf(stderr, "client: open door: %s\n", strerror(errno));
		return (3);
	}

	if (strcmp(mode, "release_emfile") == 0 ||
	    strcmp(mode, "shared_emfile") == 0) {
		rl.rlim_cur = 32;
		rl.rlim_max = 32;
		if (setrlimit(RLIMIT_NOFILE, &rl) != 0) {
			fprintf(stderr, "client: setrlimit: %s\n",
			    strerror(errno));
			return (3);
		}
		n = 0;
		while (open("/dev/null", O_RDONLY) >= 0)
			n++;
		fprintf(stderr, "client: table full (+%d, errno %d)\n",
		    n, errno);
	}

	(void) memset(&arg, 0, sizeof (arg));
	arg.rbuf = rbuf;
	arg.rsize = sizeof (rbuf);

	errno = 0;
	if (door_call(dfd, &arg) < 0) {
		fprintf(stderr, "RESULT client_errno=%d (%s)\n",
		    errno, strerror(errno));
	} else {
		fprintf(stderr, "RESULT client=ok data_size=%zu desc_num=%u "
		    "remapped=%d\n", arg.data_size, arg.desc_num,
		    arg.rbuf != rbuf);
	}
	fflush(stderr);
	return (0);
}

int
main(int argc, char **argv)
{
	int did, status;
	pid_t pid;

	mode = (argc > 1) ? argv[1] : "release_emfile";

	if (argc > 2 && strcmp(argv[2], "client") == 0)
		return (run_client());

	fprintf(stderr, "########## case: %s ##########\n", mode);

	(void) fdetach(DOOR_PATH);
	(void) unlink(DOOR_PATH);
	(void) close(creat(DOOR_PATH, 0600));

	did = door_create(server_proc, NULL, DOOR_NO_CANCEL);
	if (did < 0) {
		fprintf(stderr, "door_create: %s\n", strerror(errno));
		return (1);
	}
	if (strcmp(mode, "bigdata") == 0) {
		/* Cap request data well below the reply we will send. */
		if (door_setparam(did, DOOR_PARAM_DATA_MAX, 1024) != 0)
			fprintf(stderr, "door_setparam: %s\n", strerror(errno));
	}
	if (fattach(did, DOOR_PATH) < 0) {
		fprintf(stderr, "fattach: %s\n", strerror(errno));
		return (1);
	}

	pid = fork();
	if (pid == 0) {
		(void) execl(SELF, SELF, mode, "client", (char *)NULL);
		_exit(4);
	}
	(void) waitpid(pid, &status, 0);
	(void) sleep(1);

	(void) fdetach(DOOR_PATH);
	(void) unlink(DOOR_PATH);
	return (0);
}

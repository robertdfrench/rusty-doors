/*
 * GOALS.md 9.3 case 2: can door_return be made to fail with E2BIG?
 *
 * The spec suggests setting DOOR_PARAM_DATA_MAX below the reply size,
 * but that parameter caps REQUEST data, not replies -- a 512 KiB reply
 * under a 1 KiB DATA_MAX sailed through. So try size directly: walk
 * the reply size up past the kernel's door_max_arg and see whether
 * door_return ever comes back, and with what.
 *
 * Also checks the descriptor-survival question again on whatever
 * failure we do manage to provoke.
 */
#include <door.h>
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

static const char *DOOR_PATH = "/tmp/door_ret_big";
static const char *SELF = "./big";
static size_t reply_size;

/* ARGSUSED */
static void
server_proc(void *cookie, char *argp, size_t asz, door_desc_t *dp,
    uint_t ndesc)
{
	door_desc_t d;
	struct stat before, after;
	char *data;
	int fd, rc, saved;

	fd = open("/dev/null", O_RDONLY);
	if (fd < 0 || fstat(fd, &before) != 0) {
		fprintf(stderr, "RESULT setup_failed\n");
		fflush(stderr);
		_exit(2);
	}

	data = mmap(NULL, reply_size, PROT_READ | PROT_WRITE,
	    MAP_PRIVATE | MAP_ANON, -1, 0);
	if (data == MAP_FAILED) {
		fprintf(stderr, "RESULT mmap_failed size=%zu: %s\n",
		    reply_size, strerror(errno));
		fflush(stderr);
		_exit(2);
	}

	(void) memset(&d, 0, sizeof (d));
	d.d_attributes = DOOR_DESCRIPTOR | DOOR_RELEASE;
	d.d_data.d_desc.d_descriptor = fd;

	errno = 0;
	rc = door_return(data, reply_size, &d, 1);
	saved = errno;

	fprintf(stderr, "RESULT size=%zu door_return_rc=%d errno=%d (%s)\n",
	    reply_size, rc, saved,
	    saved == 0 ? "ERRNO NOT SET" : strerror(saved));

	errno = 0;
	if (fcntl(fd, F_GETFD) == -1) {
		fprintf(stderr, "RESULT size=%zu fd_survived=0\n", reply_size);
	} else if (fstat(fd, &after) == 0 && before.st_ino == after.st_ino &&
	    before.st_rdev == after.st_rdev) {
		fprintf(stderr, "RESULT size=%zu fd_survived=1 same_file=1\n",
		    reply_size);
	} else {
		fprintf(stderr, "RESULT size=%zu fd_survived=1 same_file=0\n",
		    reply_size);
	}
	fflush(stderr);

	(void) close(fd);
	(void) door_return(NULL, 0, NULL, 0);
	_exit(0);
}

static int
run_client(void)
{
	door_arg_t arg;
	char rbuf[64];
	int dfd;

	dfd = open(DOOR_PATH, O_RDONLY);
	if (dfd < 0)
		return (3);

	(void) memset(&arg, 0, sizeof (arg));
	arg.rbuf = rbuf;
	arg.rsize = sizeof (rbuf);

	errno = 0;
	if (door_call(dfd, &arg) < 0) {
		fprintf(stderr, "RESULT client_errno=%d (%s)\n",
		    errno, strerror(errno));
	} else {
		fprintf(stderr, "RESULT client=ok data_size=%zu desc_num=%u\n",
		    arg.data_size, arg.desc_num);
		if (arg.rbuf != rbuf)
			(void) munmap(arg.rbuf, arg.rsize);
	}
	fflush(stderr);
	return (0);
}

int
main(int argc, char **argv)
{
	int did, status;
	pid_t pid;

	reply_size = (argc > 1) ? strtoull(argv[1], NULL, 0) : (1 << 20);

	if (argc > 2 && strcmp(argv[2], "client") == 0)
		return (run_client());

	fprintf(stderr, "########## reply size %zu ##########\n", reply_size);

	(void) fdetach(DOOR_PATH);
	(void) unlink(DOOR_PATH);
	(void) close(creat(DOOR_PATH, 0600));

	did = door_create(server_proc, NULL, DOOR_NO_CANCEL);
	if (did < 0 || fattach(did, DOOR_PATH) < 0) {
		fprintf(stderr, "setup: %s\n", strerror(errno));
		return (1);
	}

	pid = fork();
	if (pid == 0) {
		(void) execl(SELF, SELF, argv[1] ? argv[1] : "1048576",
		    "client", (char *)NULL);
		_exit(4);
	}
	(void) waitpid(pid, &status, 0);
	(void) sleep(1);
	(void) fdetach(DOOR_PATH);
	(void) unlink(DOOR_PATH);
	return (0);
}

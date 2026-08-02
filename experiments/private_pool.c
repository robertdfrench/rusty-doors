/*
 * Does a DOOR_PRIVATE door need door_bind(3C)?
 *
 * DOORS-CRATE-FINDINGS.md finding 1 says a thread joins a door's
 * PRIVATE pool by calling door_bind with that door's descriptor and
 * then parking in door_return(NULL,0,NULL,0).  A thread that parks
 * without binding joins the process-wide pool instead, so a
 * DOOR_PRIVATE door gets none of the threads made for it and stops
 * answering under concurrency.
 *
 * The crate parks without binding.  This decides whether that is the
 * cause before anything is changed.
 *
 *   ./private_pool nobind   park without door_bind  (what the crate does)
 *   ./private_pool bind     door_bind, then park    (the proposed fix)
 *
 * Each run makes a DOOR_PRIVATE door, starts several threads through
 * door_server_create, and then makes CALLS concurrent calls.  It prints
 * how many returned.  A door that cannot be served answers a couple and
 * then blocks for ever, so the calls carry an alarm.
 *
 *   gcc -m64 -Wall -o private_pool private_pool.c -lpthread
 */
#include <door.h>
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#define	CALLS	20
#define	STACK	(256 * 1024)

static int	do_bind;
static int	door_fd = -1;
static int	answered;
static int	returned_ok;
static pthread_mutex_t mtx = PTHREAD_MUTEX_INITIALIZER;

/* ARGSUSED */
static void
proc(void *cookie, char *argp, size_t sz, door_desc_t *dp, uint_t n)
{
	(void) pthread_mutex_lock(&mtx);
	answered++;
	(void) pthread_mutex_unlock(&mtx);
	(void) door_return("ok", 2, NULL, 0);
}

/* ARGSUSED */
static void *
server_thread(void *arg)
{
	(void) pthread_setcancelstate(PTHREAD_CANCEL_DISABLE, NULL);

	/*
	 * The whole question.  Binding puts this thread in THIS door's
	 * private pool; parking without binding puts it in the global
	 * one, where a DOOR_PRIVATE door will never see it.
	 */
	if (do_bind) {
		if (door_bind(door_fd) < 0)
			(void) fprintf(stderr, "door_bind: %s\n",
			    strerror(errno));
	}

	(void) door_return(NULL, 0, NULL, 0);
	return (NULL);
}

/* ARGSUSED */
static void
create_proc(door_info_t *info)
{
	pthread_attr_t attr;
	pthread_t t;

	(void) pthread_attr_init(&attr);
	(void) pthread_attr_setdetachstate(&attr, PTHREAD_CREATE_DETACHED);
	(void) pthread_attr_setstacksize(&attr, STACK);
	(void) pthread_create(&t, &attr, server_thread, NULL);
	(void) pthread_attr_destroy(&attr);
}

static void *
caller(void *arg)
{
	door_arg_t a;
	char rbuf[64];
	int fd = *(int *)arg;

	(void) memset(&a, 0, sizeof (a));
	a.rbuf = rbuf;
	a.rsize = sizeof (rbuf);

	if (door_call(fd, &a) == 0) {
		(void) pthread_mutex_lock(&mtx);
		returned_ok++;
		(void) pthread_mutex_unlock(&mtx);
	}
	return (NULL);
}

int
main(int argc, char **argv)
{
	pthread_t th[CALLS];
	int i;

	setvbuf(stdout, NULL, _IONBF, 0);
	do_bind = (argc > 1 && strcmp(argv[1], "bind") == 0);
	(void) printf("mode=%s  ", do_bind ? "bind" : "nobind");

	(void) door_server_create(create_proc);

	door_fd = door_create(proc, NULL, DOOR_PRIVATE | DOOR_NO_CANCEL);
	if (door_fd < 0) {
		(void) printf("door_create: %s\n", strerror(errno));
		return (1);
	}

	/*
	 * A door that cannot be served blocks its callers for ever, so
	 * cap the whole run rather than waiting.
	 */
	(void) alarm(10);

	for (i = 0; i < CALLS; i++)
		(void) pthread_create(&th[i], NULL, caller, &door_fd);
	for (i = 0; i < CALLS; i++)
		(void) pthread_join(th[i], NULL);

	(void) alarm(0);
	(void) printf("calls=%d answered=%d returned=%d\n",
	    CALLS, answered, returned_ok);
	(void) door_revoke(door_fd);
	return (returned_ok == CALLS ? 0 : 1);
}

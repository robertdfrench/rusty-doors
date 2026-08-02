/* Which attribute combinations does door_xcreate accept? */
#include <door.h>
#include <errno.h>
#include <stdio.h>
#include <string.h>
#include <pthread.h>

/* ARGSUSED */
static void
proc(void *c, char *a, size_t s, door_desc_t *d, uint_t n)
{
	(void) door_return(NULL, 0, NULL, 0);
}

/* ARGSUSED */
static int
create_thread(door_info_t *info, void *(*f)(void *), void *arg, void *ck)
{
	pthread_attr_t attr;
	pthread_t t;

	(void) pthread_attr_init(&attr);
	(void) pthread_attr_setdetachstate(&attr, PTHREAD_CREATE_DETACHED);
	(void) pthread_attr_setstacksize(&attr, 256 * 1024);
	if (pthread_create(&t, &attr, f, arg) != 0)
		return (-1);
	return (0);
}

static void
try(const char *label, uint_t attrs, int nthread, int with_create)
{
	int d;

	errno = 0;
	d = door_xcreate(proc, NULL, attrs,
	    with_create ? create_thread : NULL, NULL, NULL, nthread);
	printf("%-46s attrs=0x%-5x nthread=%d create=%d -> %s\n",
	    label, attrs, nthread, with_create,
	    d < 0 ? strerror(errno) : "OK");
	if (d >= 0)
		(void) door_revoke(d);
}

int
main(void)
{
	setvbuf(stdout, NULL, _IONBF, 0);
	try("NO_CANCEL only",            DOOR_NO_CANCEL, 1, 1);
	try("NO_CANCEL|PRIVATE",         DOOR_NO_CANCEL | DOOR_PRIVATE, 1, 1);
	try("PRIVATE only",              DOOR_PRIVATE, 1, 1);
	try("PRIVATE, no create func",   DOOR_PRIVATE, 1, 0);
	try("PRIVATE|REFUSE_DESC",
	    DOOR_PRIVATE | DOOR_REFUSE_DESC | DOOR_NO_CANCEL, 1, 1);
	try("PRIVATE nthread=0",         DOOR_PRIVATE, 0, 1);
	try("PRIVATE nthread=4",         DOOR_PRIVATE, 4, 1);
	try("PRIVATE|UNREF",
	    DOOR_PRIVATE | DOOR_UNREF | DOOR_NO_CANCEL, 1, 1);
	try("PRIVATE|NO_DEPLETION_CB",
	    DOOR_PRIVATE | DOOR_NO_DEPLETION_CB | DOOR_NO_CANCEL, 1, 1);
	return (0);
}

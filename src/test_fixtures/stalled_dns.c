#define _GNU_SOURCE
#include <dlfcn.h>
#include <netdb.h>
#include <string.h>
#include <time.h>
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>
int getaddrinfo(const char *node, const char *service, const struct addrinfo *hints, struct addrinfo **result) {
    if (node && strcmp(node, "mywhisper-review.invalid") == 0) {
        const char *marker = getenv("MYWHISPER_RESOLVER_MARKER");
        if (marker) {
            /* Existence is the barrier. Do not expose an empty/truncated PID
             * between fopen and fprintf to the process supervising this stall. */
            char *temporary = NULL;
            if (asprintf(&temporary, "%s.XXXXXX", marker) >= 0) {
                int fd = mkstemp(temporary);
                if (fd >= 0) {
                    FILE *file = fdopen(fd, "w");
                    if (file) {
                        int written = fprintf(file, "%ld\n", (long)getpid());
                        int closed = fclose(file);
                        if (written > 0 && closed == 0) {
                            if (rename(temporary, marker) != 0) unlink(temporary);
                        } else {
                            unlink(temporary);
                        }
                    } else {
                        close(fd);
                        unlink(temporary);
                    }
                }
                free(temporary);
            }
        }
        struct timespec delay = {11, 0};
        nanosleep(&delay, 0);
        return EAI_NONAME;
    }
    int (*real)(const char *, const char *, const struct addrinfo *, struct addrinfo **) = dlsym(RTLD_NEXT, "getaddrinfo");
    return real(node,service,hints,result);
}

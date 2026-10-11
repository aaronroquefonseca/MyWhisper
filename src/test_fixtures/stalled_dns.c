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
            FILE *file = fopen(marker, "w");
            if (file) {
                fprintf(file, "%ld\n", (long)getpid());
                fclose(file);
            }
        }
        struct timespec delay = {11, 0};
        nanosleep(&delay, 0);
        return EAI_NONAME;
    }
    int (*real)(const char *, const char *, const struct addrinfo *, struct addrinfo **) = dlsym(RTLD_NEXT, "getaddrinfo");
    return real(node,service,hints,result);
}

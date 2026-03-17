// FIXTURE: C file with mixed priority items
// TESTS: Truncation priority testing

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

typedef int ErrorCode;

struct Config {
    char host[256];
    int port;
    int timeout;
};

enum LogLevel {
    LOG_DEBUG,
    LOG_INFO,
    LOG_WARN,
    LOG_ERROR
};

void init_config(struct Config* config) {
    memset(config, 0, sizeof(struct Config));
    config->port = 8080;
    config->timeout = 30;
}

ErrorCode process_request(const char* url, struct Config* config) {
    if (url == NULL || config == NULL) {
        return -1;
    }
    printf("Processing: %s on port %d\n", url, config->port);
    return 0;
}

void cleanup(struct Config* config) {
    memset(config, 0, sizeof(struct Config));
}

#ifndef OBSERVATORY_LAUNCHER_ARGUMENTS_H
#define OBSERVATORY_LAUNCHER_ARGUMENTS_H

#include <stdbool.h>
#include <stddef.h>
#include <string.h>

/* Finder has no arguments. The two existing startup options allow an explicit
 * disposable-home GUI qualification without opening a general command route. */
static bool observatory_arguments(int argc, char *const argv[],
                                  const char **home, bool *no_browser) {
    *home = NULL;
    *no_browser = false;
    for (int i = 1; i < argc; ++i) {
        if (strcmp(argv[i], "--home") == 0) {
            if (*home != NULL || i + 1 >= argc || argv[i + 1][0] != '/') {
                return false;
            }
            *home = argv[++i];
        } else if (strcmp(argv[i], "--no-browser") == 0) {
            if (*no_browser) {
                return false;
            }
            *no_browser = true;
        } else {
            return false;
        }
    }
    return true;
}

#endif

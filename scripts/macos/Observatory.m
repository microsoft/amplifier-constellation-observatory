// The signed GUI entry point is separate from the checksum-bound service.
// This program never installs, supervises, retries or chooses another program.
#import <AppKit/AppKit.h>
#include <mach-o/dyld.h>
#include <stdlib.h>
#include <sys/stat.h>
#include <unistd.h>

#include "launcher_arguments.h"

static int launch_error(NSString *detail) {
    [NSApplication sharedApplication];
    [NSApp setActivationPolicy:NSApplicationActivationPolicyAccessory];
    // This fixed error path supports macOS 11, including SDKs where the
    // replacement activation method is unavailable.
#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Wdeprecated-declarations"
    [NSApp activateIgnoringOtherApps:YES];
#pragma clang diagnostic pop
    NSAlert *alert = [[NSAlert alloc] init];
    alert.messageText = @"Observatory could not open";
    alert.informativeText = detail;
    [alert addButtonWithTitle:@"OK"];
    [alert runModal];
    return 1;
}

int main(int argc, char *argv[]) {
    @autoreleasepool {
        const char *home = NULL;
        bool no_browser = false;
        if (!observatory_arguments(argc, argv, &home, &no_browser)) {
            return launch_error(@"Open Observatory.app normally. This launcher only accepts a complete application and its supported startup options. Nothing was changed.");
        }

        uint32_t size = 0;
        (void)_NSGetExecutablePath(NULL, &size);
        if (size == 0 || size > 65536) {
            return launch_error(@"The application location could not be verified. Open the complete Observatory.app download again.");
        }
        char *raw = malloc(size);
        if (raw == NULL || _NSGetExecutablePath(raw, &size) != 0) {
            free(raw);
            return launch_error(@"The application location could not be read. Nothing was changed.");
        }
        char *resolved = realpath(raw, NULL);
        free(raw);
        if (resolved == NULL) {
            return launch_error(@"The application location is no longer available. Open the complete Observatory.app download again.");
        }
        NSString *launcher = [[NSFileManager defaultManager]
            stringWithFileSystemRepresentation:resolved length:strlen(resolved)];
        free(resolved);
        NSString *folder = launcher.stringByDeletingLastPathComponent;
        NSString *contents = folder.stringByDeletingLastPathComponent;
        NSString *app = contents.stringByDeletingLastPathComponent;
        if (![launcher.lastPathComponent isEqualToString:@"Observatory"] ||
            ![folder.lastPathComponent isEqualToString:@"MacOS"] ||
            ![contents.lastPathComponent isEqualToString:@"Contents"] ||
            ![app.pathExtension isEqualToString:@"app"]) {
            return launch_error(@"Keep Observatory inside its complete application bundle. No installation was started.");
        }
        NSString *payload = [folder stringByAppendingPathComponent:@"agent-observatory"];
        const char *program = payload.fileSystemRepresentation;
        struct stat info;
        if (program == NULL || lstat(program, &info) != 0 ||
            !S_ISREG(info.st_mode) || (info.st_mode & 0111) == 0) {
            return launch_error(@"The Observatory service is missing or unavailable in this app. Download the complete app again. Your existing installations were not changed.");
        }

        char *arguments[5];
        size_t count = 0;
        arguments[count++] = (char *)program;
        if (home != NULL) {
            arguments[count++] = "--home";
            arguments[count++] = (char *)home;
        }
        if (no_browser) {
            arguments[count++] = "--no-browser";
        }
        arguments[count] = NULL;
        execv(program, arguments);
        return launch_error(@"The Observatory service could not start. Your existing installations were preserved. Open the complete app again or use its support information.");
    }
}

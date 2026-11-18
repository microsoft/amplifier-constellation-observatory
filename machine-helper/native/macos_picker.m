#import <AppKit/AppKit.h>
#import <UniformTypeIdentifiers/UniformTypeIdentifiers.h>

// Fixed native UI only. This bridge never reads the selected file, launches a
// command, creates an installation or receives an Observatory credential.
int constellation_choose_pairing_file(char *buffer, size_t capacity) {
    @autoreleasepool {
        if (buffer == NULL || capacity == 0 || ![NSThread isMainThread]) return -1;
        buffer[0] = '\0';
        [NSApplication sharedApplication];
        [NSApp setActivationPolicy:NSApplicationActivationPolicyAccessory];
        NSOpenPanel *panel = [NSOpenPanel openPanel];
        panel.title = @"Choose your Observatory setup file";
        panel.message = @"Choose observatory-pairing.json from the Observatory download you extracted. You will enter your pairing code next.";
        panel.prompt = @"Use setup file";
        panel.canChooseFiles = YES;
        panel.canChooseDirectories = NO;
        panel.allowsMultipleSelection = NO;
        panel.canCreateDirectories = NO;
        panel.resolvesAliases = NO;
        panel.canDownloadUbiquitousContents = NO;
        panel.canResolveUbiquitousConflicts = NO;
        panel.allowedContentTypes = @[UTTypeJSON];
        [NSApp activateIgnoringOtherApps:YES];
        if ([panel runModal] != NSModalResponseOK) return 0;
        if (panel.URLs.count != 1 || !panel.URL.isFileURL) return -1;
        return [panel.URL getFileSystemRepresentation:buffer maxLength:capacity] ? 1 : -1;
    }
}

#import <AppKit/AppKit.h>
#include <stdlib.h>
#include <string.h>

char *session_take_save_path(const char *filename, const char *label) {
    @autoreleasepool {
        NSSavePanel *panel = [NSSavePanel savePanel];
        panel.nameFieldStringValue = [NSString stringWithUTF8String:filename];
        panel.prompt = [NSString stringWithUTF8String:label];
        panel.allowedFileTypes = @[panel.nameFieldStringValue.pathExtension];
        panel.allowsOtherFileTypes = NO;
        panel.extensionHidden = NO;
        panel.canCreateDirectories = YES;
        if ([panel runModal] != NSModalResponseOK) return NULL;
        const char *path = panel.URL.fileSystemRepresentation;
        return path ? strdup(path) : NULL;
    }
}

void session_take_free_path(char *path) {
    free(path);
}

bool session_take_copy(const unsigned char *bytes, size_t length) {
    @autoreleasepool {
        NSString *text = [[NSString alloc] initWithBytes:bytes
                                                 length:length
                                               encoding:NSUTF8StringEncoding];
        if (text == nil) return false;
        NSPasteboard *board = [NSPasteboard generalPasteboard];
        [board clearContents];
        return [board setString:text forType:NSPasteboardTypeString];
    }
}

#import <AppKit/AppKit.h>
#import <ApplicationServices/ApplicationServices.h>
#include <fcntl.h>
#include <limits.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/time.h>
#include <unistd.h>

// Inspect only the Harness window. AXPress or a click on the row's centre is
// limited to a node whose accessible text identifies the exact target session.
//
// This file deliberately stays in CoreFoundation: every value is copied with
// AXUIElementCopyAttributeValue and released with CFRelease, never bridged
// through Objective-C. WebKit replaces its accessibility tree while a page
// navigates, so an element carried across an autorelease boundary can be freed
// underneath us; the crash report shows CoreFoundation trapping in
// _AXUIElementValidate exactly that way.

static void writeDebugC(const char *message) {
    if (!getenv("MEMTRANS_LOCATE_DEBUG")) return;
    const char *path = getenv("MEMTRANS_LOCATE_LOG");
    if (!path) path = "/tmp/memtrans-locate.log";
    int fd = open(path, O_WRONLY | O_APPEND | O_CREAT, 0600);
    if (fd < 0) return;
    char line[320];
    struct timeval now;
    gettimeofday(&now, NULL);
    int length = snprintf(line, sizeof(line), "%ld.%06d %s\n", (long)now.tv_sec, (int)now.tv_usec, message);
    if (length > 0) write(fd, line, (size_t)length);
    close(fd);
}

typedef struct {
    const char *title;
    const char *marker;
    const char *suffix;
    BOOL pressed;
    NSUInteger budget;
} Search;

static AXUIElementRef currentItem = NULL;
static CFStringRef currentKey = NULL;

static void recordCrash(int signalNumber) {
    char line[200];
    snprintf(line, sizeof(line), "crash signal=%d item=%p", signalNumber, (void *)currentItem);
    writeDebugC(line);
    signal(signalNumber, SIG_DFL);
    raise(signalNumber);
}

static CFTypeRef copyAttribute(AXUIElementRef item, CFStringRef key) {
    currentItem = item;
    currentKey = key;
    CFTypeRef value = NULL;
    if (AXUIElementCopyAttributeValue(item, key, &value) != kAXErrorSuccess) return NULL;
    return value;
}

static BOOL isElement(CFTypeRef value) {
    return value && CFGetTypeID(value) == AXUIElementGetTypeID();
}

// Checks AXTitle, AXDescription, AXValue and AXLabel, in that order. Values are
// copied into a small stack buffer and compared as UTF-8.
static BOOL matchesText(AXUIElementRef item, const char *wanted, BOOL exact, BOOL *actionable,
                        CFStringRef *roleOut) {
    CFStringRef role = (CFStringRef)copyAttribute(item, kAXRoleAttribute);
    if (roleOut) *roleOut = role;
    else if (role) CFRelease(role);
    if (actionable) {
        *actionable = NO;
        CFArrayRef actions = NULL;
        if (AXUIElementCopyActionNames(item, &actions) == kAXErrorSuccess) {
            for (CFIndex i = 0; i < CFArrayGetCount(actions); i++) {
                CFStringRef action = (CFStringRef)CFArrayGetValueAtIndex(actions, i);
                if (action && CFGetTypeID(action) == CFStringGetTypeID() &&
                    CFStringCompare(action, kAXPressAction, 0) == kCFCompareEqualTo)
                    *actionable = YES;
            }
            CFRelease(actions);
        }
    }
    CFStringRef keys[] = {kAXTitleAttribute, kAXDescriptionAttribute, kAXValueAttribute};
    for (int k = 0; k < 3; k++) {
        CFTypeRef value = copyAttribute(item, keys[k]);
        if (!value) continue;
        BOOL hit = NO;
        if (CFGetTypeID(value) == CFStringGetTypeID()) {
            CFStringRef text = (CFStringRef)value;
            NSString *needle = [NSString stringWithUTF8String:wanted];
            if (needle) hit = exact ? [(__bridge NSString *)text isEqualToString:needle]
                                    : [(__bridge NSString *)text containsString:needle];
        }
        CFRelease(value);
        if (hit) return YES;
    }
    return NO;
}

static BOOL isPressable(CFStringRef role) {
    return role && (CFStringCompare(role, CFSTR("AXRow"), 0) == kCFCompareEqualTo ||
                    CFStringCompare(role, CFSTR("AXButton"), 0) == kCFCompareEqualTo ||
                    CFStringCompare(role, CFSTR("AXLink"), 0) == kCFCompareEqualTo);
}

static BOOL clickCentre(AXUIElementRef item) {
    CFTypeRef position = copyAttribute(item, kAXPositionAttribute);
    CFTypeRef size = copyAttribute(item, kAXSizeAttribute);
    BOOL clicked = NO;
    if (position && size && CFGetTypeID(position) == AXValueGetTypeID() &&
        CFGetTypeID(size) == AXValueGetTypeID()) {
        CGPoint point = CGPointZero;
        CGSize area = CGSizeZero;
        if (AXValueGetValue((AXValueRef)position, kAXValueCGPointType, &point) &&
            AXValueGetValue((AXValueRef)size, kAXValueCGSizeType, &area) &&
            area.width >= 4 && area.height >= 4) {
            CGPoint target = CGPointMake(point.x + area.width / 2, point.y + area.height / 2);
            CGEventRef down = CGEventCreateMouseEvent(NULL, kCGEventLeftMouseDown, target, kCGMouseButtonLeft);
            CGEventRef up = CGEventCreateMouseEvent(NULL, kCGEventLeftMouseUp, target, kCGMouseButtonLeft);
            if (down) {
                CGEventPost(kCGHIDEventTap, down);
                CFRelease(down);
            }
            if (up) {
                CGEventPost(kCGHIDEventTap, up);
                CFRelease(up);
            }
            clicked = down && up;
        }
    }
    if (position) CFRelease(position);
    if (size) CFRelease(size);
    return clicked;
}

static BOOL press(AXUIElementRef item) {
    if (AXUIElementPerformAction(item, kAXPressAction) == kAXErrorSuccess) return YES;
    return clickCentre(item);
}

// A row whose accessible text names the target session. The marker text is the
// body of the delivered message and is never a row, so it is skipped.
static BOOL isTarget(AXUIElementRef item, Search *search, BOOL *actionable) {
    if (matchesText(item, search->marker, NO, NULL, NULL)) return NO;
    if (matchesText(item, search->title, YES, NULL, NULL)) return YES;
    if (matchesText(item, search->title, NO, actionable, NULL)) return YES;
    return matchesText(item, search->suffix, NO, NULL, NULL);
}

static void searchTree(AXUIElementRef item, Search *search, NSUInteger depth) {
    if (search->pressed || search->budget == 0 || depth > 48 || !isElement(item)) return;
    --search->budget;
    BOOL actionable = NO;
    if (getenv("MEMTRANS_LOCATE_TRACE"))
    {
        char line[120];
        snprintf(line, sizeof(line), "walk depth=%lu budget=%lu", (unsigned long)depth,
                 (unsigned long)search->budget);
        writeDebugC(line);
    }
    BOOL textMatch = isTarget(item, search, &actionable);
    CFStringRef role = (CFStringRef)copyAttribute(item, kAXRoleAttribute);
    BOOL row = isPressable(role);
    if (role) CFRelease(role);
    if (textMatch && (actionable || row)) {
        if (press(item)) {
            search->pressed = YES;
            return;
        }
    }
    CFTypeRef children = copyAttribute(item, kAXChildrenAttribute);
    if (children) {
        if (CFGetTypeID(children) == CFArrayGetTypeID()) {
            CFArrayRef array = (CFArrayRef)children;
            for (CFIndex i = 0; i < CFArrayGetCount(array) && !search->pressed; i++) {
                CFTypeRef child = CFArrayGetValueAtIndex(array, i);
                if (isElement(child)) searchTree((AXUIElementRef)child, search, depth + 1);
            }
        }
        CFRelease(children);
    }
}

static BOOL bodyAndEditor(AXUIElementRef window, const char *marker, NSDate *deadline) {
    if (!isElement(window)) return NO;
    NSMutableArray *queue = [NSMutableArray arrayWithObject:(__bridge id)window];
    BOOL body = NO, editor = NO;
    NSUInteger count = 0;
    while (queue.count > 0 && deadline.timeIntervalSinceNow > 0 && count < 3000) {
        id __attribute__((objc_precise_lifetime)) retainedItem = queue.firstObject;
        AXUIElementRef item = (__bridge AXUIElementRef)retainedItem;
        [queue removeObjectAtIndex:0];
        ++count;
        CFStringRef role = (CFStringRef)copyAttribute(item, kAXRoleAttribute);
        if (getenv("MEMTRANS_LOCATE_TRACE")) {
            char line[160];
            snprintf(line, sizeof(line), "scan node=%lu role=%s", (unsigned long)count,
                     role ? "set" : "nil");
            writeDebugC(line);
        }
        if (role) {
            if ((CFStringCompare(role, CFSTR("AXTextArea"), 0) == kCFCompareEqualTo ||
                 CFStringCompare(role, CFSTR("AXTextField"), 0) == kCFCompareEqualTo)) {
                CFTypeRef enabled = copyAttribute(item, kAXEnabledAttribute);
                if (enabled) {
                    if (CFGetTypeID(enabled) == CFBooleanGetTypeID() && !CFBooleanGetValue((CFBooleanRef)enabled))
                        editor = NO;
                    else editor = YES;
                    CFRelease(enabled);
                } else {
                    editor = YES;
                }
            }
            if (CFStringCompare(role, CFSTR("AXStaticText"), 0) == kCFCompareEqualTo &&
                matchesText(item, marker, NO, NULL, NULL))
                body = YES;
            CFRelease(role);
        }
        if (body && editor) return YES;
        CFTypeRef children = copyAttribute(item, kAXChildrenAttribute);
        if (children) {
            if (CFGetTypeID(children) == CFArrayGetTypeID()) {
                CFArrayRef array = (CFArrayRef)children;
                for (CFIndex i = 0; i < CFArrayGetCount(array); i++) {
                    CFTypeRef child = CFArrayGetValueAtIndex(array, i);
                    if (isElement(child)) [queue addObject:(__bridge id)child];
                }
            }
            CFRelease(children);
        }
    }
    return NO;
}

static void unminimize(AXUIElementRef window) {
    CFTypeRef minimized = copyAttribute(window, kAXMinimizedAttribute);
    if (minimized) {
        if (CFGetTypeID(minimized) == CFBooleanGetTypeID() && CFBooleanGetValue((CFBooleanRef)minimized))
            AXUIElementSetAttributeValue(window, kAXMinimizedAttribute, kCFBooleanFalse);
        CFRelease(minimized);
    }
}

// Whether the Harness desktop app was already running before Launch Services
// opened it. A running app keeps the Workspace registry it loaded at startup,
// so a Session attached by the handoff process appears in the sidebar only
// after the next launch.
int memtrans_harness_running(void) {
    @autoreleasepool {
        return [NSRunningApplication runningApplicationsWithBundleIdentifier:@"com.deepseek.dsh"].count > 0;
    }
}

// Called on a blocking worker after Launch Services has opened the desktop app.
int memtrans_locate(const char *targetTitle, const char *targetId) {
    @autoreleasepool {
        if (!AXIsProcessTrusted()) {
            writeDebugC("not trusted");
            if ([NSBundle.mainBundle.bundleIdentifier isEqualToString:@"app.memtrans.desktop"]) {
                AXIsProcessTrustedWithOptions((__bridge CFDictionaryRef)@{(__bridge NSString *)kAXTrustedCheckOptionPrompt: @YES});
            }
            return 1;
        }
        char marker[128];
        // The ID suffix also matches markers in sessions created before the rename.
        snprintf(marker, sizeof(marker), " %s]", targetId);
        // The visible title ends with a short id suffix; match that suffix when
        // the row exposes a truncated title, and the full id otherwise.
        const char *suffix = targetId;
        const char *separator = strstr(targetTitle, " · ");
        if (separator) suffix = separator + strlen(" · ");
        NSDate *deadline = [NSDate dateWithTimeIntervalSinceNow:20];
        signal(SIGTRAP, recordCrash);
        signal(SIGSEGV, recordCrash);
        signal(SIGBUS, recordCrash);
        writeDebugC("trusted");
        BOOL pressed = NO;
        do {
            @autoreleasepool {
                NSRunningApplication *app = [NSRunningApplication runningApplicationsWithBundleIdentifier:@"com.deepseek.dsh"].firstObject;
                if (app && app.active) {
                    AXUIElementRef root = AXUIElementCreateApplication(app.processIdentifier);
                    AXUIElementSetMessagingTimeout(root, 1.0);
                    CFTypeRef windowElement = copyAttribute(root, kAXFocusedWindowAttribute);
                    writeDebugC("app active");
                    if (isElement(windowElement)) {
                        writeDebugC("window element");
                        AXUIElementRef front = (AXUIElementRef)windowElement;
                        unminimize(front);
                        if (bodyAndEditor(front, marker, deadline)) {
                            CFRelease(windowElement);
                            CFRelease(root);
                            return 0;
                        }
                        if (!pressed) {
                            Search search = {targetTitle, marker, suffix, NO, 2400};
                            searchTree(front, &search, 0);
                            pressed = search.pressed;
                            if (pressed) writeDebugC("pressed row");
                        }
                        CFRelease(windowElement);
                    }
                    CFRelease(root);
                }
            }
            if (deadline.timeIntervalSinceNow <= 0) break;
            [NSThread sleepForTimeInterval:0.4];
        } while (deadline.timeIntervalSinceNow > 0);
        {
            char line[80];
            snprintf(line, sizeof(line), "not found pressed=%d", pressed);
            writeDebugC(line);
        }
        return 2;
    }
}

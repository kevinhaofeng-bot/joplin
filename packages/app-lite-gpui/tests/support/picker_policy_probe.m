// Diagnostic only: no panel, repository, profile, or application event loop.
#import <AppKit/AppKit.h>
#include <stdio.h>
int main(void) {
    @autoreleasepool {
        NSApplication *app = NSApplication.sharedApplication;
        NSInteger before = app.activationPolicy;
        BOOL accepted = [app setActivationPolicy:NSApplicationActivationPolicyAccessory];
        printf("before=%ld accepted=%d after=%ld\n", (long)before, accepted, (long)app.activationPolicy);
    }
    return 0;
}

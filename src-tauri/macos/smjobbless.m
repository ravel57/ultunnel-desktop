#import <Foundation/Foundation.h>
#import <ServiceManagement/ServiceManagement.h>
#import <Security/Security.h>

#include <stdbool.h>
#include <stdio.h>
#include <string.h>

static char g_last_error[4096];

static void set_last_error_cstr(const char *msg) {
  if (!msg) msg = "";
  snprintf(g_last_error, sizeof(g_last_error), "%s", msg);
  g_last_error[sizeof(g_last_error) - 1] = '\0';
}

static void set_last_error_osstatus(OSStatus st, const char *prefix) {
  CFStringRef msgRef = SecCopyErrorMessageString(st, NULL);
  char msgBuf[2048] = {0};
  if (msgRef) {
    CFStringGetCString(msgRef, msgBuf, sizeof(msgBuf), kCFStringEncodingUTF8);
    CFRelease(msgRef);
  }
  if (prefix && prefix[0]) {
    snprintf(g_last_error, sizeof(g_last_error), "%s: %s (OSStatus=%ld)",
             prefix, msgBuf[0] ? msgBuf : "(no description)", (long)st);
  } else {
    snprintf(g_last_error, sizeof(g_last_error), "%s (OSStatus=%ld)",
             msgBuf[0] ? msgBuf : "(no description)", (long)st);
  }
  g_last_error[sizeof(g_last_error) - 1] = '\0';
}


static void set_last_error_cf(CFErrorRef error, const char *prefix) {
  if (!error) {
    set_last_error_cstr(prefix ? prefix : "");
    return;
  }

  CFStringRef desc = CFErrorCopyDescription(error);
  CFStringRef reason = CFErrorCopyFailureReason(error);

  char descBuf[2048] = {0};
  char reasonBuf[2048] = {0};

  if (desc) CFStringGetCString(desc, descBuf, sizeof(descBuf), kCFStringEncodingUTF8);
  if (reason) CFStringGetCString(reason, reasonBuf, sizeof(reasonBuf), kCFStringEncodingUTF8);

  long code = (long)CFErrorGetCode(error);
  const char *domainC = "";
  char domainBuf[256] = {0};
  CFStringRef domain = CFErrorGetDomain(error);
  if (domain && CFStringGetCString(domain, domainBuf, sizeof(domainBuf), kCFStringEncodingUTF8)) {
    domainC = domainBuf;
  }

  if (prefix && prefix[0]) {
    if (reasonBuf[0]) {
      snprintf(g_last_error, sizeof(g_last_error),
               "%s: %s (reason: %s) [%s:%ld]",
               prefix, descBuf[0] ? descBuf : "(no description)",
               reasonBuf, domainC, code);
    } else {
      snprintf(g_last_error, sizeof(g_last_error),
               "%s: %s [%s:%ld]",
               prefix, descBuf[0] ? descBuf : "(no description)",
               domainC, code);
    }
  } else {
    if (reasonBuf[0]) {
      snprintf(g_last_error, sizeof(g_last_error),
               "%s (reason: %s) [%s:%ld]",
               descBuf[0] ? descBuf : "(no description)",
               reasonBuf, domainC, code);
    } else {
      snprintf(g_last_error, sizeof(g_last_error),
               "%s [%s:%ld]",
               descBuf[0] ? descBuf : "(no description)",
               domainC, code);
    }
  }

  g_last_error[sizeof(g_last_error) - 1] = '\0';

  if (desc) CFRelease(desc);
  if (reason) CFRelease(reason);
}

__attribute__((visibility("default")))
const char *ultunnel_smjobbless_last_error(void) {
  return g_last_error;
}

__attribute__((visibility("default")))
bool ultunnel_smjobbless_install(const char *label_cstr) {
  if (!label_cstr || !label_cstr[0]) {
    set_last_error_cstr("empty label");
    return false;
  }

  @autoreleasepool {
    CFStringRef label = CFStringCreateWithCString(kCFAllocatorDefault, label_cstr, kCFStringEncodingUTF8);
    if (!label) {
      set_last_error_cstr("cannot create CFString for label");
      return false;
    }

    AuthorizationRef authRef = NULL;
    AuthorizationFlags flags =
      kAuthorizationFlagInteractionAllowed |
      kAuthorizationFlagPreAuthorize |
      kAuthorizationFlagExtendRights;

    OSStatus st = AuthorizationCreate(NULL, kAuthorizationEmptyEnvironment, flags, &authRef);
    if (st != errAuthorizationSuccess || authRef == NULL) {
      CFRelease(label);
      set_last_error_osstatus(st, "AuthorizationCreate failed");
      return false;
    }

    AuthorizationItem right = { kSMRightBlessPrivilegedHelper, 0, NULL, 0 };
    AuthorizationRights rights = { 1, &right };

    st = AuthorizationCopyRights(authRef, &rights, kAuthorizationEmptyEnvironment, flags, NULL);
    if (st != errAuthorizationSuccess) {
      AuthorizationFree(authRef, kAuthorizationFlagDefaults);
      CFRelease(label);
      set_last_error_osstatus(st, "AuthorizationCopyRights failed");
      return false;
    }

    CFErrorRef error = NULL;
    Boolean ok = SMJobBless(kSMDomainSystemLaunchd, label, authRef, &error);

    // Если launchd:4 — часто мешает старый job/helper. Удаляем и пробуем 1 раз снова.
    if (!ok && error) {
      CFStringRef domain = CFErrorGetDomain(error);
      CFIndex code = CFErrorGetCode(error);
      if (domain && CFStringCompare(domain, CFSTR("CFErrorDomainLaunchd"), 0) == kCFCompareEqualTo && code == 4) {
        CFRelease(error); error = NULL;
        CFErrorRef rmErr = NULL;
        SMJobRemove(kSMDomainSystemLaunchd, label, authRef, true, &rmErr);
        if (rmErr) CFRelease(rmErr);
        ok = SMJobBless(kSMDomainSystemLaunchd, label, authRef, &error);
      }
    }

    AuthorizationFree(authRef, kAuthorizationFlagDefaults);
    CFRelease(label);

    if (!ok) {
      set_last_error_cf(error, "SMJobBless failed");
      if (error) CFRelease(error);
      return false;
    }

    set_last_error_cstr("");
    if (error) CFRelease(error);
    return true;
  }
}

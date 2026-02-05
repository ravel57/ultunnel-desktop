#import <Foundation/Foundation.h>
#import <ServiceManagement/ServiceManagement.h>
#import <Security/Security.h>

static NSString *toNSString(const char *c) {
  if (!c) return nil;
  return [NSString stringWithUTF8String:c];
}

static char *dupCString(NSString *s) {
  if (!s) return NULL;
  const char *utf8 = [s UTF8String];
  if (!utf8) return NULL;
  size_t n = strlen(utf8);
  char *out = (char *)malloc(n + 1);
  memcpy(out, utf8, n);
  out[n] = 0;
  return out;
}

void smhelper_free(void *p) {
  if (p) free(p);
}

// ====== INSTALL (SMJobBless) ======

int smjobbless_install(const char *label_c, char **error_out) {
  @autoreleasepool {
    NSString *label = toNSString(label_c);
    if (!label.length) {
      if (error_out) *error_out = dupCString(@"Empty helper label");
      return 0;
    }

    AuthorizationRef authRef = NULL;
    OSStatus status = AuthorizationCreate(NULL, kAuthorizationEmptyEnvironment, kAuthorizationFlagDefaults, &authRef);
    if (status != errAuthorizationSuccess || !authRef) {
      if (error_out) *error_out = dupCString([NSString stringWithFormat:@"AuthorizationCreate failed: %d", (int)status]);
      if (authRef) AuthorizationFree(authRef, kAuthorizationFlagDefaults);
      return 0;
    }

    AuthorizationItem item = {kSMRightBlessPrivilegedHelper, 0, NULL, 0};
    AuthorizationRights rights = {1, &item};

    AuthorizationFlags flags = (AuthorizationFlags)(
      kAuthorizationFlagInteractionAllowed |
      kAuthorizationFlagPreAuthorize |
      kAuthorizationFlagExtendRights
    );

    status = AuthorizationCopyRights(authRef, &rights, kAuthorizationEmptyEnvironment, flags, NULL);
    if (status != errAuthorizationSuccess) {
      if (error_out) *error_out = dupCString([NSString stringWithFormat:@"AuthorizationCopyRights failed: %d", (int)status]);
      AuthorizationFree(authRef, kAuthorizationFlagDefaults);
      return 0;
    }

    CFErrorRef error = NULL;
    Boolean ok = SMJobBless(kSMDomainSystemLaunchd, (__bridge CFStringRef)label, authRef, &error);
    AuthorizationFree(authRef, kAuthorizationFlagDefaults);

    if (!ok) {
      if (error_out) {
        NSString *desc = error ? CFBridgingRelease(CFErrorCopyDescription(error))
                               : @"SMJobBless failed (no CFError)";
        *error_out = dupCString(desc);
      }
      if (error) CFRelease(error);
      return 0;
    }

    return 1;
  }
}

// ===== XPC protocol (MUST match Swift helper selectors) =====
//
// Swift protocol (ObjC selectors):
//   ping(_ reply: @escaping () -> Void)                         => ping:
//   startSingBox(_:configPath:argsJson:reply:)                  => startSingBox:configPath:argsJson:reply:
//   stopSingBox(_ reply: @escaping (Int32, String) -> Void)     => stopSingBox:
//   status(_ reply: @escaping (Bool, Int32) -> Void)            => status:
//   tailLogs(_:reply:)                                          => tailLogs:reply:

@protocol UltunnelPrivilegedHelperProtocol
- (void)pingWithReply:(void (^)(NSString *msg))reply;

- (void)startSingBox:(NSString *)singBoxPath
          configPath:(NSString *)configPath
            argsJson:(NSString *)argsJson
               reply:(void (^)(int32_t code, NSString *msg))reply;

- (void)stopSingBoxWithReply:(void (^)(int32_t code, NSString *msg))reply;

- (void)statusWithReply:(void (^)(BOOL running, int32_t pid))reply;
@end


static NSArray<NSString *> *parse_args_json(NSString *argsJson, NSString **errOut) {
  if (!argsJson.length) return @[];

  NSData *data = [argsJson dataUsingEncoding:NSUTF8StringEncoding];
  if (!data) {
    if (errOut) *errOut = @"args_json: invalid UTF-8";
    return nil;
  }

  NSError *e = nil;
  id obj = [NSJSONSerialization JSONObjectWithData:data options:0 error:&e];
  if (e) {
    if (errOut) *errOut = [NSString stringWithFormat:@"args_json: JSON parse error: %@", e.localizedDescription ?: @"unknown"];
    return nil;
  }

  if ([obj isKindOfClass:[NSArray class]]) {
    NSMutableArray<NSString *> *out = [NSMutableArray array];
    for (id it in (NSArray *)obj) {
      if ([it isKindOfClass:[NSString class]]) {
        [out addObject:(NSString *)it];
      } else if ([it isKindOfClass:[NSNumber class]]) {
        [out addObject:[(NSNumber *)it stringValue]];
      } else {
        // игнорируем неподдерживаемые типы
      }
    }
    return out;
  }

  // допускаем args_json как строку (один аргумент)
  if ([obj isKindOfClass:[NSString class]]) {
    return @[(NSString *)obj];
  }

  if (obj == (id)kCFNull || obj == nil) return @[];

  if (errOut) *errOut = @"args_json: expected JSON array (or string)";
  return nil;
}

static int call_helper(NSString *label,
                       void (^invoke)(id<UltunnelPrivilegedHelperProtocol> remote,
                                      void (^done)(BOOL ok, NSString *err)),
                       char **error_out) {
  __block BOOL finished = NO;
  __block BOOL ok = NO;
  __block NSString *errStr = nil;

  dispatch_semaphore_t sem = dispatch_semaphore_create(0);

  NSXPCConnection *conn = [[NSXPCConnection alloc] initWithMachServiceName:label
                                                                  options:NSXPCConnectionPrivileged];
  conn.remoteObjectInterface = [NSXPCInterface interfaceWithProtocol:@protocol(UltunnelPrivilegedHelperProtocol)];

  conn.invalidationHandler = ^{
    if (!finished) {
      finished = YES;
      ok = NO;
      errStr = errStr ?: @"XPC invalidated";
      dispatch_semaphore_signal(sem);
    }
  };

  conn.interruptionHandler = ^{
    if (!finished) {
      finished = YES;
      ok = NO;
      errStr = errStr ?: @"XPC interrupted";
      dispatch_semaphore_signal(sem);
    }
  };

  [conn resume];

  id<UltunnelPrivilegedHelperProtocol> remote =
    [conn remoteObjectProxyWithErrorHandler:^(NSError * _Nonnull error) {
      if (!finished) {
        finished = YES;
        ok = NO;
        errStr = error.localizedDescription ?: @"XPC proxy error";
        dispatch_semaphore_signal(sem);
      }
    }];

  invoke(remote, ^(BOOL ok2, NSString *err2) {
    if (!finished) {
      finished = YES;
      ok = ok2;
      errStr = err2;
      dispatch_semaphore_signal(sem);
    }
  });

  dispatch_time_t t = dispatch_time(DISPATCH_TIME_NOW, (int64_t)(10 * NSEC_PER_SEC));
  if (dispatch_semaphore_wait(sem, t) != 0) {
    ok = NO;
    errStr = @"XPC timeout waiting for helper reply";
  }

  [conn invalidate];

  if (!ok && error_out) *error_out = dupCString(errStr ?: @"Helper call failed");
  return ok ? 1 : 0;
}

int smhelper_start_singbox(const char *label_c,
                           const char *singbox_path_c,
                           const char *config_path_c,
                           const char *args_json_c,
                           char **error_out) {
  @autoreleasepool {
    NSString *label = toNSString(label_c);
    NSString *sing  = toNSString(singbox_path_c);
    NSString *cfg   = toNSString(config_path_c);
    NSString *argsJ = toNSString(args_json_c) ?: @"";

    if (!label.length || !sing.length || !cfg.length) {
      if (error_out) *error_out = dupCString(@"label/singbox_path/config_path is empty");
      return 0;
    }

//    NSString *parseErr = nil;
//    NSArray<NSString *> *args = parse_args_json(argsJ, &parseErr);
//    if (!args) {
//      if (error_out) *error_out = dupCString(parseErr ?: @"args_json parse failed");
//      return 0;
//    }

    return call_helper(label, ^(id<UltunnelPrivilegedHelperProtocol> remote, void (^done)(BOOL ok, NSString *err)) {
      [remote startSingBox:sing configPath:cfg argsJson:argsJ reply:^(int32_t code, NSString *msg) {
        BOOL success = (code == 0);
        done(success, msg ?: @"");
      }];
    }, error_out);
  }
}

int smhelper_stop_singbox(const char *label_c, char **error_out) {
  @autoreleasepool {
    NSString *label = toNSString(label_c);
    if (!label.length) {
      if (error_out) *error_out = dupCString(@"Empty helper label");
      return 0;
    }

    return call_helper(label, ^(id<UltunnelPrivilegedHelperProtocol> remote, void (^done)(BOOL ok, NSString *err)) {
      [remote stopSingBoxWithReply:^(int32_t code, NSString *msg) {
        BOOL success = (code == 0);
        done(success, msg ?: @"");
      }];
    }, error_out);
  }
}

int smhelper_status(const char *label_c, int *running_out, int *code_out, char **error_out) {
  @autoreleasepool {
    NSString *label = toNSString(label_c);
    if (!label.length) {
      if (error_out) *error_out = dupCString(@"Empty helper label");
      return 0;
    }

    __block int runningVal = 0;
    __block int pidVal = 0;

    int ok = call_helper(label, ^(id<UltunnelPrivilegedHelperProtocol> remote, void (^done)(BOOL ok, NSString *err)) {
      [remote statusWithReply:^(BOOL running, int32_t pid) {
        runningVal = running ? 1 : 0;
        pidVal = (int)pid;
        done(YES, nil);
      }];
    }, error_out);

    if (ok) {
      if (running_out) *running_out = runningVal;
      if (code_out) *code_out = pidVal; // сюда кладём pid
      return 1;
    }
    return 0;
  }
}

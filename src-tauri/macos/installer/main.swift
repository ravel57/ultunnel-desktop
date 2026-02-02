import Foundation
import ServiceManagement
import Security

let helperLabel = "ru.ravel.ultunnel-macos.helper"

func blessHelper() throws {
    var authRef: AuthorizationRef?
    var status = AuthorizationCreate(nil, nil, [], &authRef)
    guard status == errAuthorizationSuccess, let authRef else {
        throw NSError(domain: "AuthorizationCreate", code: Int(status))
    }

    // Запрашиваем право на установку privileged helper (это и вызывает системный парольный диалог)
    var authItem = AuthorizationItem(name: kSMRightBlessPrivilegedHelper,
                                     valueLength: 0,
                                     value: nil,
                                     flags: 0)
    var authRights = AuthorizationRights(count: 1, items: &authItem)

    let flags: AuthorizationFlags = [.interactionAllowed, .extendRights, .preAuthorize]
    status = AuthorizationCopyRights(authRef, &authRights, nil, flags, nil)
    guard status == errAuthorizationSuccess else {
        throw NSError(domain: "AuthorizationCopyRights", code: Int(status))
    }

    var error: Unmanaged<CFError>?
    let ok = SMJobBless(kSMDomainSystemLaunchd, helperLabel as CFString, authRef, &error)
    if !ok {
        if let e = error?.takeRetainedValue() {
            let desc = CFErrorCopyDescription(e) as String
            let info = CFErrorCopyUserInfo(e) as NSDictionary
            fputs("SMJobBless failed: \(desc)\nuserInfo: \(info)\n", stderr)
            throw e as Error
        } else {
            fputs("SMJobBless failed: unknown error\n", stderr)
            throw NSError(domain: "SMJobBless", code: 1)
        }
    }
}


func connect() -> NSXPCConnection {
    let c = NSXPCConnection(machServiceName: helperLabel, options: .privileged)
    c.remoteObjectInterface = NSXPCInterface(with: HelperProtocol.self)
    c.resume()
    return c
}

func usage() {
    print("""
          usage:
            ultunnel-helper-installer install
            ultunnel-helper-installer start <configPath>
            ultunnel-helper-installer stop
            ultunnel-helper-installer status
          """)
}

let args = CommandLine.arguments
guard args.count >= 2 else { usage(); exit(2) }

let cmd = args[1]

do {
    switch cmd {
    case "install":
        try blessHelper()
        print("OK: installed")

    case "start":
        guard args.count >= 3 else { usage(); exit(2) }
        let configPath = args[2]

        let c = connect()
        let proxy = c.remoteObjectProxyWithErrorHandler { err in
            fputs("xpc error: \(err)\n", stderr)
            exit(1)
        } as! HelperProtocol

        let sem = DispatchSemaphore(value: 0)
        proxy.startSingBox(configPath: configPath) { code, msg in
            print("code=\(code) \(msg)")
            sem.signal()
        }
        _ = sem.wait(timeout: .now() + 10)

    case "stop":
        let c = connect()
        let proxy = c.remoteObjectProxyWithErrorHandler { err in
            fputs("xpc error: \(err)\n", stderr)
            exit(1)
        } as! HelperProtocol

        let sem = DispatchSemaphore(value: 0)
        proxy.stopSingBox { code, msg in
            print("code=\(code) \(msg)")
            sem.signal()
        }
        _ = sem.wait(timeout: .now() + 10)

    case "status":
        let c = connect()
        let proxy = c.remoteObjectProxyWithErrorHandler { err in
            fputs("xpc error: \(err)\n", stderr)
            exit(1)
        } as! HelperProtocol

        let sem = DispatchSemaphore(value: 0)
        proxy.status { running, pid in
            print("running=\(running) pid=\(pid)")
            sem.signal()
        }
        _ = sem.wait(timeout: .now() + 10)

    default:
        usage()
        exit(2)
    }
} catch {
    fputs("ERROR: \(error)\n", stderr)
    exit(1)
}

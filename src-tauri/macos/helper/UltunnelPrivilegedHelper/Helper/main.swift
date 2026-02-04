import Foundation

final class SingBoxRunner {
    private var process: Process?

    // Поменяйте, если sing-box лежит в другом месте.
    // Важно: у root-launchd может не быть PATH, поэтому только абсолютный путь.
    private let singBoxPath = "/Applications/ultunnel-desktop.app/Contents/MacOS/sing-box"

    func start(configPath: String) throws -> String {
        if let p = process, p.isRunning {
            return "already running (pid=\(p.processIdentifier))"
        }

        let p = Process()
        p.executableURL = URL(fileURLWithPath: singBoxPath)
        p.arguments = ["run", "-c", configPath]

        // Желательно задать минимальный env
        p.environment = [
            "PATH": "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
        ]

        // (опционально) вывод в лог-файлы:
        // let out = FileHandle(forWritingAtPath: "/var/log/ultunnel-singbox.log")
        // p.standardOutput = out
        // p.standardError = out

        try p.run()
        process = p
        return "started pid=\(p.processIdentifier)"
    }

    func stop() -> String {
        guard let p = process, p.isRunning else {
            process = nil
            return "not running"
        }
        p.terminate()
        process = nil
        return "stopped"
    }

    func isRunning() -> (Bool, Int32) {
        if let p = process, p.isRunning {
            return (true, p.processIdentifier)
        }
        return (false, 0)
    }
}

final class Helper: NSObject, HelperProtocol {
    private let runner = SingBoxRunner()

    func startSingBox(configPath: String, reply: @escaping (Int32, String) -> Void) {
        do {
            let res = try runner.start(configPath: configPath)
            reply(0, res)
        } catch {
            reply(1, "failed: \(error)")
        }
    }

    func stopSingBox(reply: @escaping (Int32, String) -> Void) {
        reply(0, runner.stop())
    }

    func status(reply: @escaping (Bool, Int32) -> Void) {
        let (r, pid) = runner.isRunning()
        reply(r, pid)
    }
}

final class ListenerDelegate: NSObject, NSXPCListenerDelegate {
    func listener(_ listener: NSXPCListener, shouldAcceptNewConnection c: NSXPCConnection) -> Bool {
        c.exportedInterface = NSXPCInterface(with: HelperProtocol.self)
        c.exportedObject = Helper()
        c.resume()
        return true
    }
}

let machServiceName = "ru.ravel.ultunnel-macos.helper"
let listener = NSXPCListener(machServiceName: machServiceName)
let delegate = ListenerDelegate()
listener.delegate = delegate
listener.resume()

RunLoop.current.run()

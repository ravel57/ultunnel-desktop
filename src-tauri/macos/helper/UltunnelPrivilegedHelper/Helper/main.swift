import Foundation
import OSLog
import Darwin

private let machServiceName = "ru.ravel.ultunnel-macos.helper"
private let log = Logger(subsystem: machServiceName, category: "main")

log.info("helper started pid=\(getpid())")

// MARK: - SingBoxRunner

final class SingBoxRunner {
    private let q = DispatchQueue(label: "ru.ravel.ultunnel.helper.singbox")
    private var process: Process?
    private var stdoutPipe: Pipe?
    private var stderrPipe: Pipe?

    private var ring: [String] = []
    private let ringMax = 800

    func tailLogs(maxLines: Int) -> String {
        q.sync {
            let n = max(0, min(maxLines, ring.count))
            return ring.suffix(n).joined(separator: "\n")
        }
    }

    func isRunning() -> (Bool, Int32) {
        q.sync {
            if let p = process, p.isRunning { return (true, p.processIdentifier) }
            return (false, 0)
        }
    }

    func stop() -> String {
        q.sync {
            if let p = process {
                let pid = p.processIdentifier
                if p.isRunning { p.terminate() }

                var waitedMs = 0
                while p.isRunning && waitedMs < 2000 {
                    usleep(50_000); waitedMs += 50
                }
                if p.isRunning { _ = Darwin.kill(pid_t(pid), SIGKILL) }
                p.waitUntilExit()
                process = nil
            }
            if let s = try? String(contentsOfFile: "/var/run/ultunnel-singbox.pid", encoding: .utf8),
            let pid = Int32(s.trimmingCharacters(in: .whitespacesAndNewlines)),
            pid > 1 {
                _ = Darwin.kill(pid, SIGTERM)
                var waitedMs = 0
                while waitedMs < 2000 {
                    if Darwin.kill(pid, 0) != 0 { break } // процесса нет
                    usleep(50_000); waitedMs += 50
                }
                if Darwin.kill(pid, 0) == 0 {
                    _ = Darwin.kill(pid, SIGKILL)
                }
                try? FileManager.default.removeItem(atPath: "/var/run/ultunnel-singbox.pid")
                return "stopped pid=\(pid)"
            }
            runCmd("/usr/bin/pkill", ["-TERM", "-x", "sing-box"])
            usleep(100_000)
            runCmd("/usr/bin/pkill", ["-KILL", "-x", "sing-box"])
            return "stopped (fallback)"
        }
    }

    func start(singBoxPath: String, configPath: String, extraArgs: [String]) throws -> String {
        try q.sync {
            if let p = process, p.isRunning {
                return "already running (pid=\(p.processIdentifier))"
            }

            guard FileManager.default.isExecutableFile(atPath: singBoxPath) else {
                throw NSError(
                    domain: "SingBoxRunner",
                    code: 2,
                    userInfo: [NSLocalizedDescriptionKey: "sing-box not executable or not found: \(singBoxPath)"]
                )
            }
            guard FileManager.default.fileExists(atPath: configPath) else {
                throw NSError(
                    domain: "SingBoxRunner",
                    code: 3,
                    userInfo: [NSLocalizedDescriptionKey: "config not found: \(configPath)"]
                )
            }

            let p = Process()
            p.executableURL = URL(fileURLWithPath: singBoxPath)

            var args: [String] = ["run", "-c", configPath]
            args.append(contentsOf: extraArgs)
            p.arguments = args

            // launchd часто стартует с пустым окружением
            p.environment = [
                "PATH": "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
                "HOME": "/var/root"
            ]

            let out = Pipe()
            let err = Pipe()
            stdoutPipe = out
            stderrPipe = err
            p.standardOutput = out
            p.standardError = err

            func attachReader(_ pipe: Pipe, prefix: String) {
                pipe.fileHandleForReading.readabilityHandler = { [weak self] fh in
                    let data = fh.availableData
                    if data.isEmpty { return }
                    if let s = String(data: data, encoding: .utf8) {
                        self?.appendLog(prefix: prefix, text: s)
                    } else {
                        self?.appendLog(prefix: prefix, text: "<non-utf8 \(data.count) bytes>")
                    }
                }
            }
            attachReader(out, prefix: "OUT")
            attachReader(err, prefix: "ERR")

            p.terminationHandler = { [weak self] proc in
                self?.appendLog(prefix: "PROC", text: "terminated status=\(proc.terminationStatus) reason=\(proc.terminationReason.rawValue)")
                self?.q.async {
                    self?.process = nil
                    self?.stdoutPipe?.fileHandleForReading.readabilityHandler = nil
                    self?.stderrPipe?.fileHandleForReading.readabilityHandler = nil
                    self?.stdoutPipe = nil
                    self?.stderrPipe = nil
                }
            }

            try p.run()
            process = p
            try? "\(p.processIdentifier)".write(
                toFile: "/var/run/ultunnel-singbox.pid",
                atomically: true,
                encoding: .utf8
            )
            appendLog(prefix: "PROC", text: "started pid=\(p.processIdentifier) exe=\(singBoxPath) args=\(args)")
            return "started pid=\(p.processIdentifier)"
        }
    }

    private func appendLog(prefix: String, text: String) {
        q.async {
            let normalized = text.replacingOccurrences(of: "\r", with: "")
            let lines = normalized
            .split(separator: "\n", omittingEmptySubsequences: false)
            .map { "[\(prefix)] \($0)" }

            for l in lines { self.ring.append(l) }

            if self.ring.count > self.ringMax {
                self.ring.removeFirst(self.ring.count - self.ringMax)
            }
        }
    }

    private func runCmd(_ path: String, _ args: [String]) {
        let p = Process()
        p.executableURL = URL(fileURLWithPath: path)
        p.arguments = args
        do {
            try p.run()
            p.waitUntilExit()
        } catch {
            // игнор — это fallback
        }
    }
}

// MARK: - JSON utils

private func decodeArgsJson(_ s: String) -> [String] {
    let trimmed = s.trimmingCharacters(in: .whitespacesAndNewlines)
    if trimmed.isEmpty { return [] }

    guard let data = trimmed.data(using: .utf8) else { return [] }

    do {
        let obj = try JSONSerialization.jsonObject(with: data, options: [])
        if let arr = obj as? [String] { return arr }
        return []
    } catch {
        return []
    }
}

// MARK: - XPC Helper

final class Helper: NSObject, UltunnelPrivilegedHelperProtocol {
    private let runner = SingBoxRunner()

    @objc(pingWithReply:)
    func pingWithReply(_ reply: @escaping (String) -> Void) {
        reply("pong (pid=\(getpid()))")
    }

    @objc(startSingBox:configPath:argsJson:reply:)
    func startSingBox(
    _ singBoxPath: String,
    configPath: String,
    argsJson: String,
    reply: @escaping (Int32, String) -> Void
    ) {
        let extraArgs = decodeArgsJson(argsJson)
        do {
            let msg = try runner.start(singBoxPath: singBoxPath, configPath: configPath, extraArgs: extraArgs)
            reply(0, msg)
        } catch {
            let logs = runner.tailLogs(maxLines: 120)
            reply(1, "failed: \(error)\nLast logs:\n\(logs)")
        }
    }

    @objc(stopSingBoxWithReply:)
    func stopSingBoxWithReply(_ reply: @escaping (Int32, String) -> Void) {
        reply(0, runner.stop())
    }

    @objc(statusWithReply:)
    func statusWithReply(_ reply: @escaping (Bool, Int32) -> Void) {
        let (running, pid) = runner.isRunning()
        reply(running, pid)
    }
}

// MARK: - NSXPCListener

final class ListenerDelegate: NSObject, NSXPCListenerDelegate {
    private let helper = Helper()

    func listener(_ listener: NSXPCListener, shouldAcceptNewConnection c: NSXPCConnection) -> Bool {
        c.exportedInterface = NSXPCInterface(with: UltunnelPrivilegedHelperProtocol.self)
        c.exportedObject = helper
        c.resume()
        return true
    }
}

// MARK: - main (ВАЖНО: удерживаем delegate)

let listener = NSXPCListener(machServiceName: machServiceName)
let delegate = ListenerDelegate()          // strong ref
listener.delegate = delegate
listener.resume()

RunLoop.current.run()

import Foundation

@objc(UltunnelPrivilegedHelperProtocol)
protocol UltunnelPrivilegedHelperProtocol {
    func ping(_ reply: @escaping () -> Void)

    // selector: startSingBox:configPath:argsJson:reply:
    @objc(startSingBox:configPath:argsJson:reply:)
    func startSingBox(
    _ singBoxPath: String,
    configPath: String,
    argsJson: String,
    reply: @escaping (Int32, String) -> Void
    )

    @objc(stopSingBoxWithReply:)
    func stopSingBox(withReply reply: @escaping (Int32, String) -> Void)

    @objc(statusWithReply:)
    optional func status(withReply reply: @escaping (Bool, Int32) -> Void)

    // selector: tailLogs:reply:
    @objc(tailLogs:reply:)
    func tailLogs(_ maxLines: Int32, reply: @escaping (String) -> Void)
}

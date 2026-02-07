import Foundation

@objc(UltunnelPrivilegedHelperProtocol)
protocol UltunnelPrivilegedHelperProtocol: NSObjectProtocol {

    // selector: pingWithReply:
    @objc(pingWithReply:)
    func pingWithReply(_ reply: @escaping (String) -> Void)

    // selector: startSingBox:configPath:argsJson:reply:
    @objc(startSingBox:configPath:argsJson:reply:)
    func startSingBox(
        _ singBoxPath: String,
        configPath: String,
        argsJson: String,
        reply: @escaping (Int32, String) -> Void
    )

    // selector: stopSingBoxWithReply:
    @objc(stopSingBoxWithReply:)
    func stopSingBoxWithReply(_ reply: @escaping (Int32, String) -> Void)

    // selector: statusWithReply:
    @objc(statusWithReply:)
    func statusWithReply(_ reply: @escaping (Bool, Int32) -> Void)
}

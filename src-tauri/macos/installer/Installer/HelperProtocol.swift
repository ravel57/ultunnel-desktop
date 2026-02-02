import Foundation

@objc public protocol HelperProtocol {
    func startSingBox(configPath: String, reply: @escaping (Int32, String) -> Void)
    func stopSingBox(reply: @escaping (Int32, String) -> Void)
    func status(reply: @escaping (Bool, Int32) -> Void)
}

// FIXTURE: Swift file with mixed priority items
// TESTS: Truncation priority testing

import Foundation

typealias UserId = UUID

enum LogLevel {
    case debug, info, warn, error
}

protocol ConfigService {
    func getHost() -> String
    func getPort() -> Int
}

struct Config {
    let host: String
    let port: Int
    let timeout: Int
}

class Server {
    private let config: Config

    init(config: Config) {
        self.config = config
    }

    func start() {
        print("Starting on \(config.host):\(config.port)")
    }

    func stop() {
        print("Stopping server")
    }
}

func processRequest(_ url: String, config: Config) -> Int {
    guard !url.isEmpty else { return -1 }
    print("Processing: \(url) on port \(config.port)")
    return 0
}

func validateUrl(_ url: String) -> Bool {
    return url.hasPrefix("http://") || url.hasPrefix("https://")
}

let MAX_RETRIES = 3
let DEFAULT_PORT = 8080

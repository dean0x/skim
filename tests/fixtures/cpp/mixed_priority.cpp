// FIXTURE: C++ file with mixed priority items
// TESTS: Truncation priority testing

#include <iostream>
#include <string>
#include <vector>

using StringVec = std::vector<std::string>;

struct Config {
    std::string host;
    int port;
    int timeout;
};

enum class LogLevel {
    Debug,
    Info,
    Warn,
    Error
};

class Server {
public:
    Server(const Config& config) : config_(config) {}

    void start() {
        std::cout << "Starting server on " << config_.host
                  << ":" << config_.port << std::endl;
    }

    void stop() {
        std::cout << "Stopping server" << std::endl;
    }

private:
    Config config_;
};

void process_request(const std::string& url) {
    std::cout << "Processing: " << url << std::endl;
}

int main() {
    Config cfg{"localhost", 8080, 30};
    Server server(cfg);
    server.start();
    process_request("/api/health");
    server.stop();
    return 0;
}

// FIXTURE: Kotlin file with mixed priority items
// TESTS: Truncation priority testing

package com.example

import java.util.UUID

typealias UserId = UUID

enum class LogLevel {
    DEBUG, INFO, WARN, ERROR
}

interface ConfigService {
    fun getHost(): String
    fun getPort(): Int
}

data class Config(val host: String, val port: Int, val timeout: Int)

class Server(private val config: Config) {
    fun start() {
        println("Starting on ${config.host}:${config.port}")
    }

    fun stop() {
        println("Stopping server")
    }
}

fun processRequest(url: String, config: Config): Int {
    if (url.isBlank()) return -1
    println("Processing: $url on port ${config.port}")
    return 0
}

fun validateUrl(url: String): Boolean {
    return url.startsWith("http://") || url.startsWith("https://")
}

const val MAX_RETRIES = 3
const val DEFAULT_PORT = 8080

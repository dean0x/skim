/**
 * FIXTURE: Kotlin data classes, companion objects, and object declarations
 * TESTS: Data class extraction, companion object handling
 */

package com.example.models

data class User(
    val id: Long,
    val name: String,
    val email: String,
    val active: Boolean = true
) {
    companion object {
        fun fromMap(map: Map<String, Any>): User {
            return User(
                id = map["id"] as Long,
                name = map["name"] as String,
                email = map["email"] as String
            )
        }

        const val MAX_NAME_LENGTH = 100
    }

    fun toDisplayString(): String {
        return "$name <$email>"
    }
}

data class Address(val street: String, val city: String, val zip: String)

object AppConfig {
    val version = "1.0.0"
    val debug = false

    fun getConnectionString(): String {
        return "jdbc:postgresql://localhost/app"
    }
}

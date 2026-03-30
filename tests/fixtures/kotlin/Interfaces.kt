/**
 * FIXTURE: Kotlin interfaces and sealed classes
 * TESTS: Interface extraction, sealed class hierarchy
 */

package com.example.types

sealed class Result<out T> {
    data class Success<T>(val value: T) : Result<T>()
    data class Error(val message: String, val cause: Throwable? = null) : Result<Nothing>()
    object Loading : Result<Nothing>()
}

interface Validator<T> {
    fun validate(input: T): Result<T>
    fun isValid(input: T): Boolean = validate(input) is Result.Success
}

interface Serializable {
    fun toJson(): String
    fun toBytes(): ByteArray
}

class EmailValidator : Validator<String> {
    override fun validate(input: String): Result<String> {
        return if (input.contains("@")) {
            Result.Success(input)
        } else {
            Result.Error("Invalid email format")
        }
    }
}

enum class Priority {
    LOW, MEDIUM, HIGH, CRITICAL
}

typealias UserMap = Map<Long, User>
typealias Handler<T> = (T) -> Unit

/**
 * FIXTURE: Simple Kotlin class
 * TESTS: Basic function/class extraction
 */

package com.example

import java.util.UUID

data class User(val id: UUID, val name: String, val email: String)

interface UserRepository {
    fun findById(id: UUID): User?
    fun save(user: User): User
    fun delete(id: UUID)
}

class UserService(private val repository: UserRepository) {
    fun getUser(id: UUID): User {
        return repository.findById(id) ?: throw NoSuchElementException("User not found")
    }

    suspend fun createUser(name: String, email: String): User {
        val user = User(UUID.randomUUID(), name, email)
        return repository.save(user)
    }

    fun deleteUser(id: UUID) {
        repository.delete(id)
    }
}

fun add(a: Int, b: Int): Int = a + b

/**
 * FIXTURE: Kotlin coroutines and suspend functions
 * TESTS: Suspend function handling, coroutine builder patterns
 */

package com.example.async

import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.flow

interface AsyncRepository<T> {
    suspend fun findById(id: Long): T?
    suspend fun save(entity: T): T
    suspend fun findAll(): List<T>
}

class UserRepository : AsyncRepository<User> {
    override suspend fun findById(id: Long): User? {
        delay(100)
        return User(id, "test", "test@example.com")
    }

    override suspend fun save(entity: User): User {
        delay(50)
        return entity
    }

    override suspend fun findAll(): List<User> {
        delay(200)
        return listOf(User(1, "alice", "alice@example.com"))
    }

    fun observeUsers(): Flow<List<User>> = flow {
        while (true) {
            emit(findAll())
            delay(5000)
        }
    }
}

suspend fun fetchAndProcess(repository: AsyncRepository<User>): List<String> {
    val users = repository.findAll()
    return users.map { it.name }
}

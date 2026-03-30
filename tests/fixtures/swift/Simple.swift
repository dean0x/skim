/**
 * FIXTURE: Simple Swift struct and protocol
 * TESTS: Basic function/struct extraction
 */

import Foundation

struct User {
    let id: UUID
    let name: String
    let email: String
}

protocol UserRepository {
    func findById(_ id: UUID) -> User?
    func save(_ user: User) -> User
    func delete(_ id: UUID)
}

class UserService {
    private let repository: UserRepository

    init(repository: UserRepository) {
        self.repository = repository
    }

    func getUser(id: UUID) -> User {
        guard let user = repository.findById(id) else {
            fatalError("User not found")
        }
        return user
    }

    func deleteUser(id: UUID) {
        repository.delete(id)
    }
}

func add(_ a: Int, _ b: Int) -> Int {
    return a + b
}

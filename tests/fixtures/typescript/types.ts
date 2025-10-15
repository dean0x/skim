/**
 * FIXTURE: TypeScript type definitions
 * TESTS: Type extraction mode
 */

// Type alias
type UserId = string;

// Interface
interface User {
    id: UserId;
    name: string;
    email: string;
}

// Enum
enum Status {
    Active = "active",
    Inactive = "inactive",
    Pending = "pending"
}

// Class
class UserService {
    private users: User[] = [];

    findUser(id: UserId): User | null {
        return this.users.find(u => u.id === id) || null;
    }
}

// Function (should be excluded in types mode)
function processUser(user: User): void {
    console.log(`Processing ${user.name}`);
}

function hello(): string {
    return "world";
}

interface User {
    id: number;
    name: string;
    email: string;
}

class UserService {
    private users: User[] = [];

    getById(id: number): User | undefined {
        return this.users.find(u => u.id === id);
    }

    add(user: User): void {
        this.users.push(user);
    }
}

export { hello, UserService };
export type { User };

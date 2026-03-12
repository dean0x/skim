import { Request, Response } from 'express';
import { Logger } from './logger';

type UserId = string;
type ApiResponse<T> = { data: T; error?: string };

interface UserService {
    getUser(id: UserId): Promise<User>;
    createUser(data: CreateUserInput): Promise<User>;
}

interface User {
    id: UserId;
    name: string;
    email: string;
}

export function handleRequest(req: Request, res: Response): void {
    const user = req.body;
    if (!user.name) {
        res.status(400).json({ error: 'Name required' });
        return;
    }
    res.json({ data: user });
}

export function validateEmail(email: string): boolean {
    const regex = /^[^\s@]+@[^\s@]+\.[^\s@]+$/;
    return regex.test(email);
}

class UserController {
    constructor(private service: UserService, private logger: Logger) {}

    async getUser(id: string): Promise<ApiResponse<User>> {
        this.logger.info('Getting user', { id });
        const user = await this.service.getUser(id);
        return { data: user };
    }
}

const MAX_RETRIES = 3;
let requestCount = 0;

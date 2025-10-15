/**
 * Complex TypeScript example with various features
 */

import { User } from './types';

// Type definitions
type UserId = string;
type UserRole = 'admin' | 'user' | 'guest';

interface ApiResponse<T> {
    data: T;
    status: number;
    message?: string;
}

// Enum
enum ErrorCode {
    NotFound = 404,
    Unauthorized = 401,
    ServerError = 500
}

// Class with generics
class ApiClient<T> {
    private baseUrl: string;

    constructor(baseUrl: string) {
        this.baseUrl = baseUrl;
    }

    async get<R>(endpoint: string): Promise<ApiResponse<R>> {
        const response = await fetch(`${this.baseUrl}${endpoint}`);
        const data = await response.json();
        return { data, status: response.status };
    }

    async post<R>(endpoint: string, body: T): Promise<ApiResponse<R>> {
        const response = await fetch(`${this.baseUrl}${endpoint}`, {
            method: 'POST',
            body: JSON.stringify(body),
            headers: { 'Content-Type': 'application/json' }
        });
        return await response.json();
    }
}

// Arrow functions
const processUser = async (userId: UserId): Promise<User | null> => {
    try {
        const client = new ApiClient<User>('/api');
        const response = await client.get<User>(`/users/${userId}`);
        return response.data;
    } catch (error) {
        console.error('Failed to fetch user', error);
        return null;
    }
};

// Decorator (TypeScript experimental)
function log(target: any, key: string, descriptor: PropertyDescriptor) {
    const original = descriptor.value;
    descriptor.value = function(...args: any[]) {
        console.log(`Calling ${key} with`, args);
        return original.apply(this, args);
    };
    return descriptor;
}

export { ApiClient, processUser, ErrorCode };

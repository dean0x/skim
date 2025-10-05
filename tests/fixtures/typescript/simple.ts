/**
 * FIXTURE: Simple TypeScript functions
 * TESTS: Basic function signature extraction
 */

export function add(a: number, b: number): number {
    return a + b;
}

export function greet(name: string): string {
    const message = `Hello, ${name}!`;
    return message;
}

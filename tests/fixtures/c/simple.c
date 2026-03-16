// FIXTURE: Simple C functions and types
// TESTS: Basic function signature extraction

#include <stdio.h>
#include <stdlib.h>

typedef int Status;

enum Color {
    RED,
    GREEN,
    BLUE
};

struct Point {
    int x;
    int y;
};

int add(int a, int b) {
    return a + b;
}

void greet(const char* name) {
    printf("Hello, %s!\n", name);
}

struct Point make_point(int x, int y) {
    struct Point p;
    p.x = x;
    p.y = y;
    return p;
}

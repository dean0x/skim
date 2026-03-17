// FIXTURE: Simple C++ classes and functions
// TESTS: Basic function and class extraction

#include <iostream>
#include <string>

class Calculator {
public:
    Calculator(int initial) : value_(initial) {}

    int add(int x) {
        value_ += x;
        return value_;
    }

    int getValue() const {
        return value_;
    }

private:
    int value_;
};

int add(int a, int b) {
    return a + b;
}

std::string greet(const std::string& name) {
    return "Hello, " + name + "!";
}

enum class Color {
    Red,
    Green,
    Blue
};

class Shape {
public:
    virtual double area() const = 0;
    virtual ~Shape() = default;
};

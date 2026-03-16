// FIXTURE: C++ type definitions
// TESTS: Types mode extraction

#include <string>
#include <vector>
#include <memory>

using StringVec = std::vector<std::string>;
using IntPair = std::pair<int, int>;

struct Point {
    double x;
    double y;
    double z;
};

enum class Status {
    Active,
    Inactive,
    Pending
};

class Animal {
public:
    virtual std::string speak() const = 0;
    virtual ~Animal() = default;
protected:
    std::string name_;
};

template<typename T>
class Container {
public:
    void push(T value) {
        data_.push_back(value);
    }

    T pop() {
        T val = data_.back();
        data_.pop_back();
        return val;
    }

private:
    std::vector<T> data_;
};

namespace shapes {

struct Circle {
    double radius;
};

struct Rectangle {
    double width;
    double height;
};

} // namespace shapes

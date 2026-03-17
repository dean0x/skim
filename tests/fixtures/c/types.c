// FIXTURE: C type definitions
// TESTS: Types mode extraction

typedef unsigned int uint;
typedef char* string;

struct Person {
    char name[100];
    int age;
    float height;
};

enum Status {
    STATUS_ACTIVE = 0,
    STATUS_INACTIVE = 1,
    STATUS_PENDING = 2
};

union Value {
    int i;
    float f;
    char c;
};

typedef struct {
    int x;
    int y;
    int z;
} Vector3;

typedef enum {
    LOG_DEBUG,
    LOG_INFO,
    LOG_WARN,
    LOG_ERROR
} LogLevel;

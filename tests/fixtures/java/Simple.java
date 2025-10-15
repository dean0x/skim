/**
 * FIXTURE: Simple Java class
 * TESTS: Basic method signature extraction
 */

public class Simple {
    private int value;

    public Simple(int value) {
        this.value = value;
    }

    public int add(int a, int b) {
        return a + b;
    }

    public String greet(String name) {
        return "Hello, " + name + "!";
    }

    public static void main(String[] args) {
        Simple calc = new Simple(10);
        System.out.println(calc.add(5, 3));
    }
}

interface Computer {
    int compute(int x);
}

enum Status {
    ACTIVE,
    INACTIVE,
    PENDING
}

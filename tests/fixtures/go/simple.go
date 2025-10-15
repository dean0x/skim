// FIXTURE: Simple Go functions
// TESTS: Basic function signature extraction

package main

import "fmt"

func Add(a int, b int) int {
    return a + b
}

func Greet(name string) string {
    return fmt.Sprintf("Hello, %s!", name)
}

type Calculator struct {
    value int
}

func (c *Calculator) Add(x int) int {
    return c.value + x
}

type Computer interface {
    Compute(x int) int
}

type Status int

const (
    Active Status = iota
    Inactive
    Pending
)

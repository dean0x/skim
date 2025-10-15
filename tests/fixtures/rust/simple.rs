//! FIXTURE: Simple Rust functions
//! TESTS: Basic function signature extraction

pub fn add(a: i32, b: i32) -> i32 {
    a + b
}

pub fn greet(name: &str) -> String {
    format!("Hello, {}!", name)
}

pub struct Calculator {
    value: i32,
}

impl Calculator {
    pub fn new(value: i32) -> Self {
        Self { value }
    }

    pub fn add(&self, x: i32) -> i32 {
        self.value + x
    }
}

pub trait Compute {
    fn compute(&self, x: i32) -> i32;
}

pub enum Status {
    Active,
    Inactive,
    Pending,
}

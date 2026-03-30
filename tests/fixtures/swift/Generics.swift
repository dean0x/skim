/**
 * FIXTURE: Swift generics and type constraints
 * TESTS: Generic function extraction, type constraint handling
 */

import Foundation

func swap<T>(_ a: inout T, _ b: inout T) {
    let temp = a
    a = b
    b = temp
}

func findFirst<T: Equatable>(in array: [T], matching value: T) -> Int? {
    for (index, element) in array.enumerated() {
        if element == value {
            return index
        }
    }
    return nil
}

protocol Container {
    associatedtype Item
    var count: Int { get }
    func item(at index: Int) -> Item
    mutating func append(_ item: Item)
}

struct Stack<Element>: Container {
    typealias Item = Element
    private var items: [Element] = []

    var count: Int {
        return items.count
    }

    func item(at index: Int) -> Element {
        return items[index]
    }

    mutating func append(_ item: Element) {
        items.append(item)
    }

    mutating func pop() -> Element? {
        return items.popLast()
    }
}

class Repository<T: Equatable> {
    private var storage: [T] = []

    func add(_ item: T) {
        storage.append(item)
    }

    func find(_ item: T) -> Bool {
        return storage.contains(item)
    }

    func all() -> [T] {
        return storage
    }
}

typealias StringStack = Stack<String>
typealias Predicate<T> = (T) -> Bool

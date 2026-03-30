/**
 * FIXTURE: Swift protocols and extensions
 * TESTS: Protocol extraction, extension handling
 */

import Foundation

protocol Drawable {
    func draw()
    var color: String { get }
}

protocol Resizable {
    func resize(width: Double, height: Double)
}

extension Drawable {
    func draw() {
        print("Drawing with color: \(color)")
    }
}

struct Circle: Drawable, Resizable {
    var color: String
    var radius: Double

    func draw() {
        print("Drawing circle with radius \(radius)")
    }

    func resize(width: Double, height: Double) {
        // Use the minimum dimension
    }

    var area: Double {
        return Double.pi * radius * radius
    }
}

class Shape {
    var name: String

    init(name: String) {
        self.name = name
    }

    func describe() -> String {
        return "Shape: \(name)"
    }
}

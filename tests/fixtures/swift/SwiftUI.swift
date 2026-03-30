/**
 * FIXTURE: SwiftUI-style view structs
 * TESTS: Struct body stripping, computed properties
 */

import Foundation

struct ContentView {
    var title: String
    var subtitle: String
    var isLoading: Bool = false

    var displayTitle: String {
        if isLoading {
            return "Loading..."
        }
        return title
    }

    func makeBody() -> String {
        return "\(displayTitle)\n\(subtitle)"
    }
}

struct UserListView {
    var users: [String]
    var selectedIndex: Int?

    var selectedUser: String? {
        guard let index = selectedIndex else {
            return nil
        }
        return users[index]
    }

    func renderList() -> [String] {
        return users.map { user in
            return "- \(user)"
        }
    }

    mutating func selectUser(at index: Int) {
        selectedIndex = index
    }
}

enum ViewState {
    case idle
    case loading
    case loaded(data: [String])
    case error(message: String)
}

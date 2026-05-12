def hello():
    return "world"


def add(a: int, b: int) -> int:
    return a + b


class UserService:
    def __init__(self):
        self._users = []

    def get_by_id(self, user_id: int):
        return next((u for u in self._users if u["id"] == user_id), None)

    def add_user(self, user: dict) -> None:
        self._users.append(user)

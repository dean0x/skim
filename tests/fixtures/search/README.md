# MyApp Authentication Service

A microservice handling user authentication and authorization.

## Getting Started

```bash
cargo build --release
cargo run -- --config config.json
```

## API Endpoints

- `POST /auth/login` — Authenticate with email and password
- `GET /auth/verify` — Verify a bearer token
- `PUT /users/:id/role` — Update user role (admin only)

## Architecture

The service uses a [layered architecture](https://en.wikipedia.org/wiki/Multitier_architecture)
with separate handler, service, and repository layers.

See [CONTRIBUTING.md](./CONTRIBUTING.md) for development guidelines.

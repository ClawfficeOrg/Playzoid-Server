# Playzoid Server — Rust Drop-in Replacement Plan

## Overview
This document outlines a plan to create a Rust-based server that is a drop-in replacement for the TaloDev Org services (godot, backend, frontend, hosting repositories). The goal is to match existing functionality (endpoints, data models, real-time behavior) while providing a foundation for future extensions such as subaccount chat under a main user account (appearing as distinct users).

## Assumptions about TaloDev (based on public inspection)
- The system provides REST/JSON APIs for CRUD operations on entities like users, servers, scenes, assets, etc.
- Real-time updates may be delivered via WebSocket or polling (to be confirmed).
- Authentication likely uses API keys or JWT tokens.
- Data is stored in a relational database (PostgreSQL/MySQL) or a document store (MongoDB).
- Frontend and Godot clients consume the same API endpoints.

## Proposed Rust Architecture
- **Framework**: `actix-web` (mature, high-performance) or `warp`/`axum` if preferred.
- **Async runtime**: `tokio` with `#[tokio::main]` or `async-std`.
- **Database**: `sqlx` (compile-time checked SQL) for PostgreSQL, or `mongodb` driver if MongoDB is used.
- **Authentication**: `jsonwebtoken` (JWT) + `bcrypt` for password hashing if applicable; API key middleware for service-to-service calls.
- **Real-time**: `actix-web-actix-proto` or manual WebSocket via `actix-web` + `tokio-tungstenite` for bidirectional updates.
- **Serialization**: `serde` + `serde_json`.
- **Config**: `config` crate or `dotenvy` + custom structs.
- **Logging**: `tracing` + `tracing-subscriber`.
- **Testing**: `actix-web-http-test` + `sqlx::test` or `mockall`.

## Suggested Folder Layout
```
Playzoid-Server/
├── Cargo.toml
├── README.md
├── .gitignore
├── src/
│   ├── main.rs
│   ├── lib.rs        # if we want a library
│   ├── api/
│   │   ├── mod.rs
│   │   ├── handlers/
│   │   │   ├── users.rs
│   │   │   ├── servers.rs
│   │   │   ├── scenes.rs
│   │   │   ├── assets.rs
│   │   │   └── ...
│   │   ├── middleware/
│   │   │   ├── auth.rs
│   │   │   ├── logging.rs
│   │   │   └── rate_limit.rs
│   │   ├── models/
│   │   │   ├── user.rs
│   │   │   ├── server.rs
│   │   │   ├── scene.rs
│   │   │   └── ...
│   │   ├── routes.rs
│   │   └── error.rs
│   ├── db/
│   │   ├── mod.rs
│   │   ├── connection.rs
│   │   └── migrations/
│   ├── realtime/
│   │   ├── mod.rs
│   │   ├── websocket.rs
│   │   └── publisher.rs
│   ├── utils/
│   │   └── helpers.rs
│   └── config.rs
├── migrations/
│   └─ V1__init.sql
├── tests/
│   ├── api_test.rs
│   └── db_test.rs
├── benches/
│   └── ...
└── docs/
    ├─ PLAN.md        # this file
    └─ ARCHITECTURE.md
```

## API Contract Summary (to be filled from inspection)
| Method | Endpoint | Description | Auth | Real-time counterpart |
|--------|----------|-------------|------|-----------------------|
| GET    | /users   | List users  | API key | WS: user list updates |
| POST   | /users   | Create user | API key | WS: new user event    |
| GET    | /users/:id | Get user   | API key | WS: user update       |
| PATCH  | /users/:id | Update user| API key | WS: user update       |
| DELETE | /users/:id | Delete user| API key | WS: user deleted      |
| ...    | ...      | ...         | ...    | ...                   |

## Subaccount Chat Extension (future)
- Main account owns subagents (like Clawffice-Space agents).
- Each subagent gets a distinct user ID under the main account for display and messaging.
- Chat persistence: store messages in a `subagent_messages` table linked to main account.
- WebSocket topic: `ws://server/subchat/{main_account_id}` broadcasts to all subagent connections under that main account.
- Access control: middleware ensures a subagent can only speak as its own assigned user ID.

## Build & Run
```bash
# Build
cargo build --release

# Run (dev)
cargo run

# Run (prod)
./target/release/playzoid-server
```

## Next Steps
1. Confirm actual TaloDev API endpoints by inspecting the public repos or running a local instance.
2. Implement the CRUD endpoints and verify against existing clients.
3. Add WebSocket support for real-time updates.
4. Add authentication and rate limiting.
5. Implement database migrations and connection pooling.
6. Write integration tests.
7. Once parity is reached, begin extending with subaccount chat under the main account.

---
*Generated as initial plan for Playzoid-Server Rust drop-in replacement.*
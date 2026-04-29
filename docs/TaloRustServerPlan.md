# Talo Replacement Server in Rust

## Overview
This document outlines a drop-in replacement server for the Talo game backend written in Rust. It aims to replicate the functionalities provided by Talo's TypeScript-based backend while making use of Rust's ecosystem for enhanced performance and reliability. The plan will also specify extensions for subaccount chat functionality under the main user account, appearing as distinct users.

## Current Functionality

Talo provides features like:
- Player management (persistent data, groups, authentication)
- Leaderboards
- Game saves
- Real-time game channels (WebSocket-based communication)
- Game analytics
- Player presence with custom statuses
- Cloud game configuration
- Feedback collection

## Target Architecture

### Frameworks/Tools
- **actix-web** (HTTP server framework)
- **tokio** (async runtime)
- **sqlx** (for database interaction with MySQL/PostgreSQL support)
- **serde** (serialization/deserialization)
- **jsonwebtoken** (JWT authentication middleware)
- **tracing** (structured logging and tracing)
- **redis-rs** (Redis client library for caching and session management)
- **warp or Axum for APIs** (with OpenAPI/Swagger integration using `utoipa`)

### Folder Layout
```plaintext
src
├── api                 # HTTP API handlers and routes
│   ├── players.rs      # Player management APIs
│   ├── auth.rs         # Authentication/Authorization logic
│   ├── leaderboard.rs  # Leaderboard APIs
├── entities            # Database models
│   ├── player.rs       # Player-related entities
│   ├── stats.rs        # Game stats
│   ├── savegame.rs     # Game save structure
├── middleware          # Middlewares (JWT, rate limiting, etc.)
├── services            # Business logic for specific actions (e.g., saving a leaderboard)
├── sockets             # WebSocket communication for real-time channels
└── main.rs             # Application entry point

migrations/             # SQL migration files
config/                 # Configuration files (e.g., Docker, .env templates)
```

### Proposed Cargo Dependencies
```toml
[dependencies]
actix-web = "4.0"
tokio = { version = "1", features = ["full"] }
sqlx = { version = "0.5", features = ["mysql", "runtime-tokio-native-tls"] }
serde = { version = "1.0", features = ["derive"] }
jsonwebtoken = "8.3"
tracing = "0.1"
redis = "0.23"
utopia = "1.0" # (or customized OpenAPI crates for Swagger integration)
```

## API Contract Summary

### Authentication
#### POST `/auth/login`
- Authenticate user and return JWT.
- Headers: `Content-Type: application/json`
- Body:
  ```json
  {
    "username": "sample_user",
    "password": "secure_pass"
  }
  ```
- Response:
  ```json
  {
    "token": "<JWT-TOKEN>",
    "expiry": 3600
  }
  ```

### Player Management
#### GET `/players/{id}`
- Retrieves player details by ID.
- Response:
  ```json
  {
    "id": "player123",
    "username": "sample_user",
    "email": "user@example.com",
    "status": "online"
  }
  ```

#### POST `/players`
- Registers a new player.
- Body:
  ```json
  {
    "username": "new_player",
    "email": "new@example.com",
    "password": "secure_pass"
  }
  ```
- Response:
  ```json
  {
    "id": "player124",
    "status": "created"
  }
  ```

### Leaderboards
#### GET `/leaderboards/{game_id}`
- Retrieve leaderboard for a specific game.
- Response:
  ```json
  [
    { "player": "player1", "score": 1000 },
    { "player": "player2", "score": 950 }
  ]
  ```

### Real-Time Communication
#### WebSocket `/ws`
- Facilitates game channels for real-time communication between players.
- Supports player presence updates, chat, and live game events.

### Data Models
#### Player
```rust
#[derive(Serialize, Deserialize, sqlx::FromRow)]
pub struct Player {
    pub id: String,
    pub username: String,
    pub email: String,
    pub status: String, // e.g., online, offline
}
```

## Subaccount Chat Extension
### Concept
Subaccounts for players appear as distinct users but are linked to a master account. Game channels will treat subaccounts as individual participants with a dedicated `parent_account` field for relationship tracing.

**Extension Strategy**
- **Database**: Add `parent_account_id` to player schema.
- **API**: Enhance `/players` and `/auth` to support subaccount creation/login.
- **WebSocket**: Extend game channels to group messages by subaccount contexts.

## Deployment
The server will be containerized using Docker for ease of deployment, with examples included for both basic and HTTPS-enabled setups via Caddy or Nginx.

### Example Docker Compose
```yaml
version: '3.8'
services:
  backend:
    image: talo-backend-rust:latest
    build: .
    ports:
      - "80:80"
    environment:
      DATABASE_URL: "mysql://user:pass@db/talo"
      REDIS_URL: "redis://cache"
  db:
    image: mysql:8.0
    environment:
      MYSQL_ROOT_PASSWORD: rootpass
      MYSQL_DATABASE: talo
  cache:
    image: redis:alpine
```

## Monitoring and Maintenance
- **Structured Logging**: Use `tracing` for request/response tracing.
- **Health Checks**: Provide `/healthz` HTTP and WebSocket pings for liveness.
- **Migrations**: Use `sqlx-cli` to manage migrations (`sqlx migrate run`).

## Conclusion
This plan aims to implement a feature-complete Rust server based on Talo's architecture while extending functionality to introduce subaccount-based chat under a parent-user system.
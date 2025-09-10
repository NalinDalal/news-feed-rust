# News Feed Service

This project implements a simplified social media **news feed backend** in Rust. It demonstrates how to design a scalable, in-memory feed system with fan-out, caching, and basic social graph management. The system uses Warp for the HTTP API, DashMap for concurrent caching, and a custom message queue for post fanout.

---

## Overview

The core idea is to simulate how large-scale social networks handle user feeds:

- **Users** can follow each other, create posts, and like posts.
- **Posts** are fanned out to followers’ news feeds via an asynchronous message queue and worker system.
- **Caching** is managed with DashMap for thread-safe concurrent access.
- **Counters** track likes and replies.
- **Hydration** combines posts with user information and interaction state for feed responses.

This prototype is intended as an experiment in system design and concurrent programming, not a production-ready implementation.

---

## Architecture

1. **Cache Layer (`CacheLayer`)**
   - Stores users, posts, social graph, news feeds, hot cache, and action history.
   - Supports fast lookups and updates with DashMap.
   - Maintains post counters and user actions (likes).

2. **Message Queue (`MessageQueue`)**
   - Implements asynchronous fanout of posts to followers.
   - Uses Tokio `mpsc::UnboundedSender` to queue messages.
   - A dispatcher distributes messages to workers in a round-robin fashion.

3. **Services**
   - **PostService**: Create and fetch posts.
   - **FanoutService**: Triggers fanout to followers via the message queue.
   - **NewsFeedService**: Retrieves hydrated feeds (posts + author + liked state).

4. **API Endpoints (Warp)**
   - `POST /v1/me/feed` – Create a post.
   - `GET /v1/me/feed` – Get the authenticated user’s news feed.
   - `POST /v1/users/follow` – Follow a user.
   - `POST /v1/posts/like` – Like a post.

5. **Authentication**
   - Simplified via `auth_token` header or query param.
   - Accepts tokens of the form `user_<id>`.

---

## Algorithm: Fanout on Write

This system implements the **fanout-on-write** model:

- When a user creates a post:
  - The post is cached.
  - The `FanoutService` enqueues a message containing the post ID and the user’s followers.
  - Workers dequeue the message and insert the post into each follower’s news feed.
- News feeds are stored as bounded `VecDeque`s (latest 1000 items).

This model is chosen for simplicity. In real-world systems, fanout-on-read is also used to reduce write amplification.

---

## How to Run Locally

### Prerequisites

- Rust (latest stable, via [rustup](https://rustup.rs/))
- Cargo (comes with Rust)

### Steps

1. Clone the repository:

   ```bash
   git clone https://github.com/your-username/news-feed-rs.git
   cd news-feed-rs
   ```

2. Run the server:

   ```bash
   cargo run
   ```

3. The server starts on `http://127.0.0.1:3030`.

### Example Usage

Create a post:

```bash
curl -X POST "http://localhost:3030/v1/me/feed?auth_token=user_1" \
  -H "Content-Type: application/json" \
  -d '{"content":"Hello from Rust!"}'
```

Get feed:

```bash
curl "http://localhost:3030/v1/me/feed?auth_token=user_2"
```

Follow a user:

```bash
curl -X POST "http://localhost:3030/v1/users/follow?auth_token=user_2" \
  -H "Content-Type: application/json" \
  -d '{"target_user_id":"user1"}'
```

Like a post:

```bash
curl -X POST "http://localhost:3030/v1/posts/like?auth_token=user_2" \
  -H "Content-Type: application/json" \
  -d '{"post_id":"post_123"}'
```

---

## Limitations

- In-memory only (no persistence).
- Simplified authentication.
- No pagination or advanced feed ranking.
- Not horizontally scalable without external queue/cache systems.

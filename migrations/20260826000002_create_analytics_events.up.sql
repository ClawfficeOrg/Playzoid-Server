-- Analytics events (append-only event log).
--
-- Each row is one fire-and-forget analytics event (backing the batched
-- POST /v1/events ingest — task 0.4.6). Rows are never updated or deleted
-- by the application: there is no `updated_at` and no `public_id`, unlike
-- `game_saves`/`players` — events are write-only in v0 and clients never
-- address a stored event.
--
-- `player_id` is optional (events may be emitted before identify) and uses
-- ON DELETE SET NULL rather than CASCADE: deleting a player must never
-- erase their event history — that would break append-only semantics.
-- The internal BIGINT id never leaves the server, mirroring every other
-- table convention.
--
-- The event schema stays deliberately generic (`name` + JSON `props`);
-- typed event names/payloads are an open question deferred to Phase 1.0
-- (see docs/memory.md → Open Questions #6). Upstream Talo's analytics
-- event shape is undocumented in this repo, so no upstream-specific
-- columns are guessed. Size capping of `props` is deliberately not
-- enforced here — MySQL JSON columns cannot be size-bounded in schema;
-- the cap applies at the API layer (task 0.4.6), same split as the
-- leaderboards' props ≤ 4 KB rule.

CREATE TABLE IF NOT EXISTS analytics_events (
    id          BIGINT UNSIGNED NOT NULL AUTO_INCREMENT,
    player_id   BIGINT UNSIGNED NULL,
    name        VARCHAR(64)     NOT NULL,
    props       JSON            NULL,
    created_at  DATETIME        NOT NULL DEFAULT CURRENT_TIMESTAMP,

    PRIMARY KEY (id),
    KEY idx_analytics_events_player (player_id),
    KEY idx_analytics_events_name_created_at (name, created_at),

    CONSTRAINT fk_analytics_events_player
        FOREIGN KEY (player_id) REFERENCES players (id)
        ON DELETE SET NULL
        ON UPDATE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

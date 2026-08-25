-- Leaderboards and leaderboard entries.
--
-- A leaderboard is identified by its `internal_name` (Talo uses
-- /v1/leaderboards/:internalName routes). Entries hold one row per
-- player per leaderboard; re-submitting a score updates the existing
-- entry (enforced by the unique constraint), matching the PUT
-- /leaderboards/{game_id}/entries/{player_id} semantics in Phase 0.3.

CREATE TABLE IF NOT EXISTS leaderboards (
    id            BIGINT UNSIGNED NOT NULL AUTO_INCREMENT,
    internal_name VARCHAR(64)     NOT NULL,
    display_name  VARCHAR(255)    NULL,
    created_at    DATETIME        NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at    DATETIME        NOT NULL DEFAULT CURRENT_TIMESTAMP
                                   ON UPDATE CURRENT_TIMESTAMP,

    PRIMARY KEY (id),
    UNIQUE KEY uk_leaderboards_internal_name (internal_name)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

CREATE TABLE IF NOT EXISTS leaderboard_entries (
    id             BIGINT UNSIGNED NOT NULL AUTO_INCREMENT,
    leaderboard_id BIGINT UNSIGNED NOT NULL,
    player_id      BIGINT UNSIGNED NOT NULL,
    score          BIGINT          NOT NULL,
    props          JSON            NULL,
    created_at     DATETIME        NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at     DATETIME        NOT NULL DEFAULT CURRENT_TIMESTAMP
                                   ON UPDATE CURRENT_TIMESTAMP,

    PRIMARY KEY (id),
    UNIQUE KEY uk_lb_entries_lb_player (leaderboard_id, player_id),
    KEY idx_lb_entries_ranking (leaderboard_id, score DESC),
    KEY idx_lb_entries_player (player_id),

    CONSTRAINT fk_lb_entries_leaderboard
        FOREIGN KEY (leaderboard_id) REFERENCES leaderboards (id)
        ON DELETE CASCADE
        ON UPDATE CASCADE,
    CONSTRAINT fk_lb_entries_player
        FOREIGN KEY (player_id) REFERENCES players (id)
        ON DELETE CASCADE
        ON UPDATE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
